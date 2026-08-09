use crate::event::{Event, EventData, EventKind, ProcessRef};
use crate::trace::{
    aggregator::Aggregator,
    claude_transcript, codex_transcript, eslogger, network,
    persist::Persist,
    pid_tree::{self, PidTree},
    provider::Provider,
    session::{Session, SessionStatus},
};
use anyhow::{anyhow, Context, Result};
use std::collections::HashSet;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const RAW_CHAN_CAP: usize = 4096;
const PERSIST_CHAN_CAP: usize = 8192;
const POLL_INTERVAL: Duration = Duration::from_millis(500);
const TREE_POLL_INTERVAL: Duration = Duration::from_millis(100);
pub(crate) const LIVE_FLUSH_INTERVAL: Duration = Duration::from_millis(250);
/// How long to wait for eslogger's first event when sudo is already cached
/// (no prompt expected — events arrive within milliseconds if it's working).
const ESLOGGER_READY_TIMEOUT: Duration = Duration::from_secs(8);
/// Longer window when a sudo password prompt is expected first.
const ESLOGGER_READY_TIMEOUT_PROMPT: Duration = Duration::from_secs(60);
/// The exact kernel event types eslogger subscribes to. This is the single
/// source of truth shared by the spawn (`start_eslogger_thread`) and the
/// transparency banner (`bring_up_eslogger`) so the banner can never claim a
/// different command than the one we actually run as root.
const ESLOGGER_EVENTS: &[&str] = &[
    "exec", "fork", "exit", "open", "close", "create", "write", "unlink", "rename",
];

/// Launch a command, record it, and exit when it does. No TUI — the wrapped
/// command owns the terminal (this is the `tracce claude` / `tracce codex` /
/// `tracce exec` path). To watch a running agent live instead, see `attach`.
pub fn run(argv: Vec<String>, provider: Provider, root: PathBuf) -> Result<i32> {
    if argv.is_empty() {
        return Err(anyhow!("trace requires a command"));
    }

    let cwd = std::env::current_dir()?;
    let tracer_pid = std::process::id();

    // Raw events channel — shared by eslogger (if active), the tree poller,
    // the network poller, and the selected provider's intent tailer.
    let (raw_tx, raw_rx) = mpsc::sync_channel::<Event>(RAW_CHAN_CAP);

    // Bring eslogger up BEFORE spawning the wrapped child so the Endpoint
    // Security client is registered first (avoids an early event-loss race).
    let (eslogger_handle, eslogger_active) = bring_up_eslogger(raw_tx.clone());

    // Build Command for the wrapped child — always runs as the current user.
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawn {:?}", argv[0]))?;
    let child_pid = child.id();

    let session = Session::create(&root, provider, child_pid, tracer_pid, &argv, &cwd)?;
    eprintln!("tracce · recording to {}", session.dir().display());

    let mut tree = PidTree::new(child_pid);
    tree.seed_descendants();
    let agg = Arc::new(Mutex::new(Aggregator::new(tree)));

    emit_synthetic_root_exec(&raw_tx, child_pid, tracer_pid, &argv);

    let persist = Arc::new(Persist::open(&session.events_path())?);
    let flush_handle = start_flush_thread(persist.clone(), LIVE_FLUSH_INTERVAL);
    let (net_handle, tree_poll_handle, transcript_handle) =
        start_poll_sources(provider, child_pid, &cwd, eslogger_active, true, &agg, &raw_tx)?;

    let (aggregator_handle, persist_handle) = spawn_pipeline(agg, persist, raw_rx);

    let status = child.wait()?;
    let exit_code = status.code().unwrap_or(-1);

    shutdown_sources(
        eslogger_handle,
        net_handle,
        tree_poll_handle,
        transcript_handle,
    );
    drop(raw_tx);
    aggregator_handle.join().ok();
    persist_handle.join().ok();
    flush_handle.shutdown();

    session.mark_status(SessionStatus::Done)?;
    eprintln!("tracce · session ended · path: {}", session.dir().display());

    Ok(exit_code)
}

/// What eslogger's reader thread reports back about its own startup.
pub(crate) enum EsloggerSignal {
    /// First event parsed — the ES client is live.
    Ready,
    /// The reader loop ended before any event (sudo declined / spawn failed /
    /// stream closed). Used to degrade promptly instead of waiting the timeout.
    Exited,
}

/// Try to start eslogger. Returns its stop-handle and whether it is actually
/// active. On failure we print the poll-only degrade banner and return
/// `(None, false)` — the caller carries on with the poll sources.
pub(crate) fn bring_up_eslogger(raw_tx: SyncSender<Event>) -> (Option<ThreadStop>, bool) {
    let sudo_cached = Command::new("sudo")
        .args(["-n", "true"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !sudo_cached {
        eprintln!("tracce · eslogger needs root to read kernel events. About to run as root:");
        eprintln!(
            "         sudo /usr/bin/eslogger {}",
            ESLOGGER_EVENTS.join(" ")
        );
        eprintln!(
            "         Apple's own tool, read-only. tracce itself never runs as root. Decline → poll-only."
        );
        eprintln!("tracce · sudo will prompt for your password now…");
    }

    let (ready_tx, ready_rx) = mpsc::channel::<EsloggerSignal>();
    let handle = match start_eslogger_thread(raw_tx, ready_tx) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("tracce · eslogger failed to spawn: {e}");
            print_degrade_banner();
            return (None, false);
        }
    };

    eprintln!("tracce · waiting for eslogger to be ready (sudo may prompt)…");
    let timeout = if sudo_cached {
        ESLOGGER_READY_TIMEOUT
    } else {
        ESLOGGER_READY_TIMEOUT_PROMPT
    };
    match ready_rx.recv_timeout(timeout) {
        Ok(EsloggerSignal::Ready) => {
            eprintln!("tracce · eslogger active — full event capture on.");
            (Some(handle), true)
        }
        Ok(EsloggerSignal::Exited) | Err(_) => {
            // Declined, failed, or never produced an event in time. Shut the
            // thread down so a late-arriving eslogger can't clobber the tree
            // poller's exec attribution, then degrade to poll-only.
            handle.shutdown();
            print_degrade_banner();
            (None, false)
        }
    }
}

fn print_degrade_banner() {
    eprintln!("tracce · eslogger unavailable (sudo declined or failed to start)");
    eprintln!("tracce · running poll-only: process tree + network + agent tool calls.");
    eprintln!("         file open/write/delete events OFF.");
}

/// Emit a synthetic Exec for the traced root process so the viewer's Process
/// pane shows it as the root of the tree. In eslogger mode this may also arrive
/// via ES; the aggregator/app dedupe by pid.
pub(crate) fn emit_synthetic_root_exec(
    raw_tx: &SyncSender<Event>,
    pid: u32,
    ppid: u32,
    argv: &[String],
) {
    let exe = argv.first().cloned().unwrap_or_default();
    let comm = std::path::Path::new(&exe)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(&exe)
        .to_string();
    let process = Arc::new(ProcessRef {
        pid,
        comm,
        image: PathBuf::from(&exe),
        argv: argv.to_vec(),
    });
    let _ = raw_tx.send(Event {
        ts_ns: now_ns(),
        kind: EventKind::Exec,
        pid,
        ppid,
        process,
        data: EventData::Exec {
            argv: argv.to_vec(),
            image: PathBuf::from(&exe),
        },
        flags: 0,
    });
}

/// Start the poll-based sources that run regardless of mode. The tree poller is
/// started ONLY when eslogger is inactive, so it never clobbers eslogger's rich
/// exec argv with bare `ps` basenames. The selected provider's intent tailer
/// runs independently of eslogger because it is a different data class.
pub(crate) fn start_poll_sources(
    provider: Provider,
    root_pid: u32,
    cwd: &std::path::Path,
    eslogger_active: bool,
    replay_new_rollout: bool,
    agg: &Arc<Mutex<Aggregator>>,
    raw_tx: &SyncSender<Event>,
) -> Result<(ThreadStop, Option<ThreadStop>, Option<ThreadStop>)> {
    let net_handle = start_network_thread(root_pid, agg.clone(), raw_tx.clone())?;

    let tree_poll_handle = if !eslogger_active {
        Some(start_tree_poll_thread(
            root_pid,
            agg.clone(),
            raw_tx.clone(),
        )?)
    } else {
        None
    };

    let transcript_handle = match provider {
        Provider::Claude => match claude_transcript::start_transcript_thread(
            root_pid,
            cwd.to_path_buf(),
            raw_tx.clone(),
            replay_new_rollout,
        ) {
            Ok(h) => Some(h),
            Err(e) => {
                eprintln!("tracce · Claude transcript unavailable: {e}");
                None
            }
        },
        Provider::Codex => match codex_transcript::start_transcript_thread(
            root_pid,
            cwd.to_path_buf(),
            raw_tx.clone(),
            replay_new_rollout,
        ) {
            Ok(h) => Some(h),
            Err(e) => {
                eprintln!("tracce · Codex session history unavailable: {e}");
                None
            }
        },
        Provider::Other => None,
    };

    Ok((net_handle, tree_poll_handle, transcript_handle))
}

pub(crate) fn shutdown_sources(
    eslogger_handle: Option<ThreadStop>,
    net_handle: ThreadStop,
    tree_poll_handle: Option<ThreadStop>,
    transcript_handle: Option<ThreadStop>,
) {
    if let Some(h) = eslogger_handle {
        h.shutdown();
    }
    net_handle.shutdown();
    if let Some(h) = tree_poll_handle {
        h.shutdown();
    }
    if let Some(h) = transcript_handle {
        h.shutdown();
    }
}

/// Spawn the aggregator + persist threads that drain `raw_rx`, coalesce bursts,
/// and write JSONL. Returns their join handles; drop the raw sender to stop them.
pub(crate) fn spawn_pipeline(
    agg: Arc<Mutex<Aggregator>>,
    persist: Arc<Persist>,
    raw_rx: mpsc::Receiver<Event>,
) -> (thread::JoinHandle<()>, thread::JoinHandle<()>) {
    let (persist_tx, persist_rx) = mpsc::sync_channel::<Event>(PERSIST_CHAN_CAP);
    let agg_for_loop = agg.clone();
    let aggregator_handle = thread::spawn(move || {
        for raw in raw_rx {
            let mut g = agg_for_loop.lock().unwrap();
            for ev in g.process(raw) {
                let _ = persist_tx.send(ev);
            }
        }
        let mut g = agg_for_loop.lock().unwrap();
        for ev in g.flush() {
            let _ = persist_tx.send(ev);
        }
        drop(persist_tx);
    });
    let persist_for_writer = persist.clone();
    let persist_handle = thread::spawn(move || {
        for ev in persist_rx {
            let _ = persist_for_writer.write(&ev);
        }
        let _ = persist_for_writer.flush();
    });
    (aggregator_handle, persist_handle)
}

/// Flush the persist buffer on a timer so live viewers see events promptly
/// during quiet stretches (Persist otherwise only flushes every 256 events).
pub(crate) fn start_flush_thread(persist: Arc<Persist>, interval: Duration) -> ThreadStop {
    let flag = Arc::new(AtomicBool::new(false));
    let stop = flag.clone();
    let h = thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            thread::sleep(interval);
            let _ = persist.flush();
        }
        let _ = persist.flush();
    });
    ThreadStop::new(flag, h)
}

pub struct ThreadStop {
    flag: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
}

impl ThreadStop {
    pub(crate) fn new(flag: Arc<AtomicBool>, join: thread::JoinHandle<()>) -> Self {
        Self {
            flag,
            join: Some(join),
        }
    }

    pub(crate) fn shutdown(mut self) {
        self.flag.store(true, Ordering::SeqCst);
        if let Some(h) = self.join.take() {
            let _ = h.join();
        }
    }
}

fn start_eslogger_thread(
    tx: SyncSender<Event>,
    ready_tx: mpsc::Sender<EsloggerSignal>,
) -> Result<ThreadStop> {
    let flag = Arc::new(AtomicBool::new(false));
    let stop = flag.clone();
    let h = thread::spawn(move || {
        // Run eslogger via sudo so it lands in a different audit session from the
        // wrapped child process.  stdin is inherited so sudo can prompt for a password.
        let mut child = match Command::new("/usr/bin/sudo")
            .arg("/usr/bin/eslogger")
            .args(ESLOGGER_EVENTS)
            .stdin(Stdio::inherit())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                eprintln!("tracce · CRITICAL: failed to spawn sudo eslogger: {e}");
                let _ = ready_tx.send(EsloggerSignal::Exited);
                return;
            }
        };

        if let Some(stderr) = child.stderr.take() {
            thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines().flatten() {
                    eprintln!("tracce · eslogger stderr: {line}");
                }
            });
        }

        let stdout = child.stdout.take().unwrap();
        let reader = BufReader::new(stdout);
        let event_count = AtomicUsize::new(0);
        let parse_errors = AtomicUsize::new(0);
        let mut ready_tx = Some(ready_tx);
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
                    if let Some(tx) = ready_tx.take() {
                        let _ = tx.send(EsloggerSignal::Ready);
                    }
                    event_count.fetch_add(1, Ordering::Relaxed);
                    if tx.send(ev).is_err() {
                        break;
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    let n = parse_errors.fetch_add(1, Ordering::Relaxed) + 1;
                    if n == 1 || n % 100 == 0 {
                        eprintln!("tracce · eslogger parse error (#{n}): {e}");
                    }
                }
            }
        }

        // If we ended the read loop without ever signalling Ready, the client
        // never came up (sudo declined / closed). Tell the waiter so it can
        // degrade immediately rather than blocking on the timeout.
        if let Some(tx) = ready_tx.take() {
            let _ = tx.send(EsloggerSignal::Exited);
        }

        if event_count.load(Ordering::Relaxed) == 0 {
            eprintln!(
                "tracce · WARNING: no eslogger events recorded; \
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

/// Poll the OS process table periodically and emit synthetic Exec events for
/// any descendants of `root_pid` we haven't sent before.  The aggregator will
/// grow its tree from these events the same way it does from eslogger.
fn start_tree_poll_thread(
    root_pid: u32,
    _agg: Arc<Mutex<Aggregator>>,
    tx: SyncSender<Event>,
) -> Result<ThreadStop> {
    let flag = Arc::new(AtomicBool::new(false));
    let stop = flag.clone();
    let h = thread::spawn(move || {
        let mut seen: HashSet<u32> = HashSet::from([root_pid]);
        while !stop.load(Ordering::SeqCst) {
            if let Ok(descendants) = pid_tree::list_descendants_with_comm(root_pid) {
                let ts = now_ns();
                for (pid, ppid, comm) in descendants {
                    if !seen.insert(pid) {
                        continue;
                    }
                    let process = Arc::new(ProcessRef {
                        pid,
                        comm: comm.clone(),
                        image: PathBuf::new(),
                        argv: vec![comm.clone()],
                    });
                    let ev = Event {
                        ts_ns: ts,
                        kind: EventKind::Exec,
                        pid,
                        ppid,
                        process,
                        data: EventData::Exec {
                            argv: vec![comm],
                            image: PathBuf::new(),
                        },
                        flags: 0,
                    };
                    if tx.send(ev).is_err() {
                        return;
                    }
                }
            }
            thread::sleep(TREE_POLL_INTERVAL);
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

pub(crate) fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}
