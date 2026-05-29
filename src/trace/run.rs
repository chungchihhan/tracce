use crate::event::Event;
use crate::trace::{
    aggregator::Aggregator, eslogger, network, persist::Persist, pid_tree::PidTree,
    session::{Session, SessionStatus},
};
use anyhow::{anyhow, Context, Result};
use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const RAW_CHAN_CAP: usize = 4096;
const PERSIST_CHAN_CAP: usize = 8192;
const POLL_INTERVAL: Duration = Duration::from_millis(500);

pub fn run(argv: Vec<String>, root: PathBuf) -> Result<i32> {
    if argv.is_empty() {
        return Err(anyhow!("trace requires a command"));
    }
    if !is_root() {
        return Err(anyhow!(
            "peekaboo trace must run as root; try: sudo peekaboo trace ..."
        ));
    }

    // 1. Spawn the wrapped child.
    let cwd = std::env::current_dir()?;
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    // Drop privileges back to the invoking user so the child sees its own
    // ~/.config/claude, credentials, etc.
    if let (Ok(uid), Ok(gid)) = (sudo_uid(), sudo_gid()) {
        cmd.uid(uid).gid(gid);
        if let Some(home) = sudo_home(uid) {
            cmd.env("HOME", home);
        }
    }

    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawn {:?}", argv[0]))?;
    let child_pid = child.id();
    let tracer_pid = std::process::id();

    // 2. Session dir.
    let session = Session::create(&root, child_pid, tracer_pid, &argv, &cwd)?;
    eprintln!("peekaboo · recording to {}", session.dir().display());

    let persist = Arc::new(Persist::open(&session.events_path())?);
    let tree = PidTree::new(child_pid);
    let agg = Arc::new(Mutex::new(Aggregator::new(tree)));

    // 3. Start eslogger reader thread.
    let (raw_tx, raw_rx) = mpsc::sync_channel::<Event>(RAW_CHAN_CAP);
    let eslogger_handle = start_eslogger_thread(raw_tx.clone())?;

    // 4. Start network poller thread.
    let agg_for_net = agg.clone();
    let net_raw_tx = raw_tx.clone();
    let net_handle = start_network_thread(child_pid, agg_for_net, net_raw_tx)?;

    // 5. Aggregator thread: drain raw_rx, write to persist.
    let (persist_tx, persist_rx) = mpsc::sync_channel::<Event>(PERSIST_CHAN_CAP);
    let agg_for_loop = agg.clone();
    let aggregator_handle = thread::spawn(move || {
        for raw in raw_rx {
            let mut g = agg_for_loop.lock().unwrap();
            for ev in g.process(raw) {
                let _ = persist_tx.send(ev);
            }
        }
        // Flush bursts on shutdown.
        let mut g = agg_for_loop.lock().unwrap();
        for ev in g.flush() {
            let _ = persist_tx.send(ev);
        }
        drop(persist_tx);
    });

    // 6. Persist thread.
    let persist_for_writer = persist.clone();
    let persist_handle = thread::spawn(move || {
        for ev in persist_rx {
            let _ = persist_for_writer.write(&ev);
        }
        let _ = persist_for_writer.flush();
    });

    // 7. Wait for child.
    let status = child.wait()?;
    let exit_code = status.code().unwrap_or(-1);

    // 8. Shut down ordered: signal eslogger + net to stop, then drain.
    eslogger_handle.shutdown();
    net_handle.shutdown();
    drop(raw_tx); // close the channel so aggregator thread exits

    aggregator_handle.join().ok();
    persist_handle.join().ok();

    // 9. Finalize session.
    session.mark_status(SessionStatus::Done)?;
    if let (Ok(uid), Ok(gid)) = (sudo_uid(), sudo_gid()) {
        let _ = session.chown_to(uid, gid);
    }
    eprintln!(
        "peekaboo · session ended · path: {}",
        session.dir().display()
    );

    Ok(exit_code)
}

fn is_root() -> bool {
    nix::unistd::Uid::effective().is_root()
}

fn sudo_uid() -> Result<u32> {
    Ok(std::env::var("SUDO_UID")?.parse()?)
}

fn sudo_gid() -> Result<u32> {
    Ok(std::env::var("SUDO_GID")?.parse()?)
}

fn sudo_home(uid: u32) -> Option<PathBuf> {
    use nix::unistd::{Uid, User};
    User::from_uid(Uid::from_raw(uid))
        .ok()
        .flatten()
        .map(|u| u.dir)
}

pub struct ThreadStop {
    flag: Arc<std::sync::atomic::AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
}

impl ThreadStop {
    fn shutdown(mut self) {
        self.flag
            .store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(h) = self.join.take() {
            let _ = h.join();
        }
    }
}

fn start_eslogger_thread(tx: SyncSender<Event>) -> Result<ThreadStop> {
    let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop = flag.clone();
    let h = thread::spawn(move || {
        let mut child = match Command::new("/usr/bin/eslogger")
            .args(["exec", "fork", "exit", "open", "close", "create", "write"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                eprintln!("peekaboo · CRITICAL: eslogger failed to start: {e}");
                return;
            }
        };

        // Drain eslogger's stderr in a background thread so it doesn't block.
        if let Some(stderr) = child.stderr.take() {
            thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines().flatten() {
                    eprintln!("peekaboo · eslogger stderr: {line}");
                }
            });
        }

        let stdout = child.stdout.take().unwrap();
        let reader = BufReader::new(stdout);
        let event_count = AtomicUsize::new(0);
        let parse_errors = AtomicUsize::new(0);
        for line in reader.lines() {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            let line = match line {
                Ok(l) => l,
                Err(_) => break,
            };
            match eslogger::parse_line(&line) {
                Ok(Some(ev)) => {
                    event_count.fetch_add(1, Ordering::Relaxed);
                    if tx.send(ev).is_err() {
                        break;
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    let n = parse_errors.fetch_add(1, Ordering::Relaxed) + 1;
                    if n == 1 || n % 100 == 0 {
                        eprintln!("peekaboo · eslogger parse error (#{n}): {e}");
                    }
                }
            }
        }

        let total_events = event_count.load(Ordering::Relaxed);
        if total_events == 0 {
            eprintln!(
                "peekaboo · WARNING: no eslogger events recorded; \
                check Full Disk Access for your terminal app"
            );
        }

        let _ = child.kill();
        let _ = child.wait();
    });
    Ok(ThreadStop {
        flag,
        join: Some(h),
    })
}

fn start_network_thread(
    root_pid: u32,
    agg: Arc<Mutex<Aggregator>>,
    tx: SyncSender<Event>,
) -> Result<ThreadStop> {
    use crate::event::{EventData, EventKind, NetProto, ProcessRef};
    let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop = flag.clone();
    let h = thread::spawn(move || {
        let host_cache = crate::hosts::HostCache::new();
        let mut prev: Vec<network::Connection> = Vec::new();
        let placeholder_proc = Arc::new(ProcessRef {
            pid: root_pid,
            comm: "?".into(),
            image: PathBuf::new(),
            argv: vec![],
        });
        while !stop.load(std::sync::atomic::Ordering::SeqCst) {
            let pids = collect_pids(&agg);

            let raw = match network::run_lsof(&pids) {
                Ok(s) => s,
                Err(_) => String::new(),
            };
            let now = network::parse_lsof(&raw);
            let (opens, closes) = network::diff_connections(&prev, &now);
            let ts = now_ns();
            for c in opens {
                let host = host_cache.resolve(c.remote.ip());
                let ev = crate::event::Event {
                    ts_ns: ts,
                    kind: EventKind::NetOpen,
                    pid: c.pid,
                    ppid: 0,
                    process: placeholder_proc.clone(),
                    data: EventData::NetOpen {
                        remote: c.remote,
                        local: c.local,
                        host,
                        proto: NetProto::Tcp,
                    },
                    flags: 0,
                };
                if tx.send(ev).is_err() {
                    return;
                }
            }
            for c in closes {
                let ev = crate::event::Event {
                    ts_ns: ts,
                    kind: EventKind::NetClose,
                    pid: c.pid,
                    ppid: 0,
                    process: placeholder_proc.clone(),
                    data: EventData::NetClose {
                        remote: c.remote,
                        bytes_in: 0,
                        bytes_out: 0,
                    },
                    flags: 0,
                };
                if tx.send(ev).is_err() {
                    return;
                }
            }
            prev = now;
            thread::sleep(POLL_INTERVAL);
        }
    });
    Ok(ThreadStop {
        flag,
        join: Some(h),
    })
}

fn collect_pids(agg: &Arc<Mutex<Aggregator>>) -> Vec<u32> {
    agg.lock().unwrap().pids()
}

fn now_ns() -> u64 {
    use std::time::SystemTime;
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}
