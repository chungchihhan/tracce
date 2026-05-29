use crate::event::{Event, EventData, EventKind, FileOp, ProcessRef, FLAG_COALESCED, FLAG_SENSITIVE};
use crate::sensitive::is_sensitive;
use crate::trace::pid_tree::PidTree;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

const BURST_WINDOW_NS: u64 = 100_000_000; // 100 ms
const BURST_MIN_EVENTS: usize = 50;

pub struct Aggregator {
    tree: PidTree,
    /// Open-event bursts indexed by (pid, common prefix depth-1).
    bursts: HashMap<(u32, PathBuf), Burst>,
}

struct Burst {
    first_ts: u64,
    last_ts: u64,
    count: usize,
    process: Arc<ProcessRef>,
    prefix: PathBuf,
}

impl Aggregator {
    pub fn new(tree: PidTree) -> Self {
        Self { tree, bursts: HashMap::new() }
    }

    /// Feed one event; returns 0..N events to forward downstream.
    pub fn process(&mut self, mut ev: Event) -> Vec<Event> {
        // Update tree on structural events even if we'll filter them.
        match ev.data {
            EventData::Fork { child_pid } => self.tree.on_fork(ev.pid, child_pid),
            EventData::Exit { .. }        => self.tree.on_exit(ev.pid),
            EventData::Exec { .. }        => self.tree.on_exec(ev.pid),
            _ => {}
        }
        if !self.tree.contains(ev.pid) {
            return Vec::new();
        }
        // Sensitive flag.
        if let EventData::File { path, .. } = &ev.data {
            if is_sensitive(path) {
                ev.flags |= FLAG_SENSITIVE;
            }
        }
        // Burst coalescing for Open events only.
        if let (EventKind::Open, EventData::File { path, .. }) = (&ev.kind, &ev.data) {
            let prefix = path.parent().unwrap_or_else(|| std::path::Path::new("/")).to_path_buf();
            let key = (ev.pid, prefix.clone());
            let b = self.bursts.entry(key.clone()).or_insert_with(|| Burst {
                first_ts: ev.ts_ns,
                last_ts: ev.ts_ns,
                count: 0,
                process: ev.process.clone(),
                prefix: prefix.clone(),
            });
            // Reset burst if window expired before this event.
            if ev.ts_ns.saturating_sub(b.first_ts) > BURST_WINDOW_NS {
                let emitted = emit_burst(b);
                *b = Burst {
                    first_ts: ev.ts_ns, last_ts: ev.ts_ns, count: 1,
                    process: ev.process.clone(), prefix,
                };
                let mut out = Vec::new();
                if let Some(e) = emitted { out.push(e); }
                return out;
            }
            b.last_ts = ev.ts_ns;
            b.count += 1;
            // Only emit the individual event if we're below the burst threshold.
            // Above, we suppress and emit a summary when the window expires (or at flush).
            if b.count < BURST_MIN_EVENTS {
                return vec![ev];
            } else {
                return Vec::new();
            }
        }
        vec![ev]
    }

    /// Flush any open bursts. Call when the trace is ending.
    pub fn flush(&mut self) -> Vec<Event> {
        let bursts = std::mem::take(&mut self.bursts);
        bursts.into_values().filter_map(|b| emit_burst(&b)).collect()
    }
}

fn emit_burst(b: &Burst) -> Option<Event> {
    if b.count < BURST_MIN_EVENTS {
        return None;
    }
    Some(Event {
        ts_ns: b.last_ts,
        kind: EventKind::Open,
        pid: b.process.pid,
        ppid: 0,
        process: b.process.clone(),
        data: EventData::File {
            op: FileOp::Open,
            path: b.prefix.clone(),
            size: Some(b.count as u64),
        },
        flags: FLAG_COALESCED,
    })
}
