use ctrace::event::{Event, EventData, EventKind, ProcessRef};
use ctrace::trace::persist::Persist;
use std::sync::Arc;
use tempfile::NamedTempFile;

fn ev(ts: u64) -> Event {
    Event {
        ts_ns: ts,
        kind: EventKind::Exec,
        pid: 4711,
        ppid: 1,
        process: Arc::new(ProcessRef {
            pid: 4711,
            comm: "claude".into(),
            image: "/usr/local/bin/claude".into(),
            argv: vec!["claude".into()],
        }),
        data: EventData::Exec { argv: vec!["claude".into()], image: "/usr/local/bin/claude".into() },
        flags: 0,
    }
}

#[test]
fn writes_one_line_per_event() {
    let f = NamedTempFile::new().unwrap();
    let p = Persist::open(f.path()).unwrap();
    p.write(&ev(1)).unwrap();
    p.write(&ev(2)).unwrap();
    p.flush().unwrap();
    let body = std::fs::read_to_string(f.path()).unwrap();
    assert_eq!(body.lines().count(), 2);
}

#[test]
fn each_line_is_valid_json() {
    let f = NamedTempFile::new().unwrap();
    let p = Persist::open(f.path()).unwrap();
    for i in 0..5 { p.write(&ev(i)).unwrap(); }
    p.flush().unwrap();
    for line in std::fs::read_to_string(f.path()).unwrap().lines() {
        let _: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("bad json line {line:?}: {e}"));
    }
}
