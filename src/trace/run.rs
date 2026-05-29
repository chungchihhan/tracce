use crate::event::Event;
use crate::trace::{
    aggregator::Aggregator, eslogger, network, persist::Persist, pid_tree::PidTree,
    session::{Session, SessionStatus},
};
use anyhow::{anyhow, Context, Result};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const RAW_CHAN_CAP: usize = 4096;
const PERSIST_CHAN_CAP: usize = 8192;
const POLL_INTERVAL: Duration = Duration::from_millis(500);
const ESLOGGER_READY_TIMEOUT: Duration = Duration::from_secs(10);

pub fn run(argv: Vec<String>, root: PathBuf) -> Result<i32> {
    if argv.is_empty() {
        return Err(anyhow!("trace requires a command"));
    }

    let cwd = std::env::current_dir()?;
    let tracer_pid = std::process::id();

    // Check whether sudo credentials are cached so we can warn the user early.
    let sudo_cached = Command::new("sudo")
        .args(["-n", "true"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !sudo_cached {
        eprintln!("peekaboo · sudo will prompt for your password to run eslogger…");
    }

    // 1. Channels + ready signal.
    let (raw_tx, raw_rx) = mpsc::sync_channel::<Event>(RAW_CHAN_CAP);
    // ready_tx is sent to the eslogger thread; it fires once the first event arrives.
    let (ready_tx, ready_rx) = mpsc::channel::<()>();

    // 2. Start eslogger early (via sudo) so it has time to register its ES client.
    //    stdin is inherited so sudo can prompt for a password on the terminal.
    let eslogger_handle = start_eslogger_thread(raw_tx.clone(), ready_tx)?;

    // 3. Wait until eslogger is actually producing events (or timeout after 10 s).
    eprintln!("peekaboo · waiting for eslogger to be ready (sudo may prompt)…");
    let _ = ready_rx.recv_timeout(ESLOGGER_READY_TIMEOUT);

    // 4. Build Command for the wrapped child (runs as the current user — no uid/gid drop needed).
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    // 5. Spawn the child now that eslogger is subscribed.
    let mut child = cmd.spawn().with_context(|| format!("spawn {:?}", argv[0]))?;
    let child_pid = child.id();

    // 6. Session dir.
    let session = Session::create(&root, child_pid, tracer_pid, &argv, &cwd)?;
    eprintln!("peekaboo · recording to {}", session.dir().display());

    // 7. pid_tree.
    let mut tree = PidTree::new(child_pid);
    tree.seed_descendants();
    eprintln!(
        "peekaboo · debug: root pid {child_pid}, seeded tree has {} pids: {:?}",
        tree.len(),
        tree.pids()
    );
    let agg = Arc::new(Mutex::new(Aggregator::new(tree)));

    // 8. Persist + network + aggregator threads.
    let persist = Arc::new(Persist::open(&session.events_path())?);
    let agg_for_net = agg.clone();
    let net_raw_tx = raw_tx.clone();
    let net_handle = start_network_thread(child_pid, agg_for_net, net_raw_tx)?;
    let (persist_tx, persist_rx) = mpsc::sync_channel::<Event>(PERSIST_CHAN_CAP);
    let agg_for_loop = agg.clone();
    let aggregator_handle = thread::spawn(move || {
        let mut raw_count = 0usize;
        let mut post_filter_count = 0usize;
        let mut samples_printed = 0usize;
        for raw in raw_rx {
            raw_count += 1;
            if samples_printed < 10 {
                eprintln!(
                    "peekaboo · debug: raw event #{}: kind={:?} pid={} ppid={}",
                    samples_printed + 1,
                    raw.kind,
                    raw.pid,
                    raw.ppid,
                );
                samples_printed += 1;
            }
            let mut g = agg_for_loop.lock().unwrap();
            for ev in g.process(raw) {
                post_filter_count += 1;
                let _ = persist_tx.send(ev);
            }
        }
        let mut g = agg_for_loop.lock().unwrap();
        for ev in g.flush() {
            post_filter_count += 1;
            let _ = persist_tx.send(ev);
        }
        eprintln!(
            "peekaboo · debug: raw={raw_count} post-filter={post_filter_count} tree-final-size={}",
            g.tree_len()
        );
        drop(persist_tx);
    });
    let persist_for_writer = persist.clone();
    let persist_handle = thread::spawn(move || {
        for ev in persist_rx {
            let _ = persist_for_writer.write(&ev);
        }
        let _ = persist_for_writer.flush();
    });

    // 9. Wait for child + shutdown.
    let status = child.wait()?;
    let exit_code = status.code().unwrap_or(-1);

    eslogger_handle.shutdown();
    net_handle.shutdown();
    drop(raw_tx);
    aggregator_handle.join().ok();
    persist_handle.join().ok();

    session.mark_status(SessionStatus::Done)?;
    // Session is owned by the user from the start — no chown needed.
    eprintln!("peekaboo · session ended · path: {}", session.dir().display());

    Ok(exit_code)
}

pub struct ThreadStop {
    flag: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
}

impl ThreadStop {
    fn shutdown(mut self) {
        self.flag.store(true, Ordering::SeqCst);
        if let Some(h) = self.join.take() {
            let _ = h.join();
        }
    }
}

fn start_eslogger_thread(tx: SyncSender<Event>, ready_tx: mpsc::Sender<()>) -> Result<ThreadStop> {
    let flag = Arc::new(AtomicBool::new(false));
    let stop = flag.clone();
    let h = thread::spawn(move || {
        // Run eslogger via sudo so it lands in a different audit session from the
        // wrapped child process.  stdin is inherited so sudo can prompt for a password.
        let mut child = match Command::new("/usr/bin/sudo")
            .args(["/usr/bin/eslogger", "exec", "fork", "exit", "open", "close", "create", "write"])
            .stdin(Stdio::inherit())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                eprintln!("peekaboo · CRITICAL: failed to spawn sudo eslogger: {e}");
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
        let raw_seen = AtomicUsize::new(0);
        let event_count = AtomicUsize::new(0);
        let parse_errors = AtomicUsize::new(0);
        let unknown_count = AtomicUsize::new(0);
        // Use Option so we can take() it after the first send (avoids clone).
        let mut ready_tx = Some(ready_tx);
        for line in reader.lines() {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            let line = match line {
                Ok(l) => l,
                Err(_) => break,
            };
            raw_seen.fetch_add(1, Ordering::Relaxed);
            match eslogger::parse_line(&line) {
                Ok(Some(ev)) => {
                    // Signal ready on the very first successfully parsed event.
                    if let Some(tx) = ready_tx.take() {
                        let _ = tx.send(());
                    }
                    event_count.fetch_add(1, Ordering::Relaxed);
                    if tx.send(ev).is_err() {
                        break;
                    }
                }
                Ok(None) => {
                    unknown_count.fetch_add(1, Ordering::Relaxed);
                }
                Err(e) => {
                    let n = parse_errors.fetch_add(1, Ordering::Relaxed) + 1;
                    if n == 1 || n % 100 == 0 {
                        eprintln!("peekaboo · eslogger parse error (#{n}): {e}");
                    }
                }
            }
        }

        let total_raw = raw_seen.load(Ordering::Relaxed);
        let total_events = event_count.load(Ordering::Relaxed);
        let total_unknown = unknown_count.load(Ordering::Relaxed);
        let total_errors = parse_errors.load(Ordering::Relaxed);
        eprintln!(
            "peekaboo · debug: eslogger raw-lines={total_raw} parsed-ok={total_events} \
             unrecognized={total_unknown} parse-errors={total_errors}"
        );
        if total_events == 0 {
            eprintln!(
                "peekaboo · WARNING: no eslogger events recorded; \
                check Full Disk Access for your terminal app"
            );
        }

        let unk = unknown_count.load(Ordering::Relaxed);
        if unk > 0 {
            eprintln!("peekaboo · note: {unk} eslogger lines had unrecognized event payload");
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
    let flag = Arc::new(AtomicBool::new(false));
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
        while !stop.load(Ordering::SeqCst) {
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
