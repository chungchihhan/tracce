use peekaboo::event::{Event, EventData, EventKind, FileOp, ProcessRef, FLAG_SENSITIVE, FLAG_COALESCED};
use peekaboo::trace::aggregator::Aggregator;
use peekaboo::trace::pid_tree::PidTree;
use std::sync::Arc;

fn file_event(ts: u64, pid: u32, path: &str) -> Event {
    Event {
        ts_ns: ts,
        kind: EventKind::Open,
        pid,
        ppid: 0,
        process: Arc::new(ProcessRef {
            pid, comm: "rg".into(),
            image: "/usr/bin/rg".into(), argv: vec!["rg".into()],
        }),
        data: EventData::File { op: FileOp::Open, path: path.into(), size: None },
        flags: 0,
    }
}

#[test]
fn drops_events_outside_pid_tree() {
    let tree = PidTree::new(100);
    let mut agg = Aggregator::new(tree);
    let ev = file_event(1, 999, "/x");
    let out = agg.process(ev);
    assert!(out.is_empty(), "999 isn't in tree, event should be dropped");
}

#[test]
fn tags_sensitive_paths() {
    let mut tree = PidTree::new(100);
    tree.on_fork(100, 200);
    let mut agg = Aggregator::new(tree);
    let ev = file_event(1, 200, "/Users/x/.aws/credentials");
    std::env::set_var("HOME", "/Users/x");
    let out = agg.process(ev);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].flags & FLAG_SENSITIVE, FLAG_SENSITIVE);
}

#[test]
fn ppid_in_tree_pulls_pid_into_tree() {
    let tree = PidTree::new(100);
    let mut agg = Aggregator::new(tree);
    // Event from pid 200 whose ppid is 100 (root); pid 200 is not yet in tree.
    let mut ev = file_event(1, 200, "/some/file");
    ev.ppid = 100;
    let out = agg.process(ev);
    assert_eq!(out.len(), 1, "ppid-in-tree should pull pid 200 into the tree");
}

#[test]
fn coalesces_burst_into_single_event() {
    let mut tree = PidTree::new(100);
    tree.on_fork(100, 200);
    let mut agg = Aggregator::new(tree);
    let mut total = 0usize;
    // 60 opens under /src in 60 ns — well within the 100 ms window.
    for i in 0..60 {
        let out = agg.process(file_event(i, 200, &format!("/src/file_{i}.rs")));
        total += out.len();
    }
    // Flush any pending burst at end.
    total += agg.flush().len();
    assert!(total < 60, "expected coalescing to reduce {total} events");
    assert!(total >= 1, "must still produce at least one summary event");
}
