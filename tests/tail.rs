use tracce::event::{Event, EventData, EventKind, ProcessRef};
use tracce::view::tail::Tail;
use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

fn ev(ts: u64) -> Event {
    Event {
        ts_ns: ts,
        kind: EventKind::Exec,
        pid: 1, ppid: 0,
        process: Arc::new(ProcessRef { pid: 1, comm: "x".into(), image: "/x".into(), argv: vec![] }),
        data: EventData::Exec { argv: vec!["x".into()], image: "/x".into() },
        flags: 0,
    }
}

#[test]
fn reads_existing_lines_then_follows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.jsonl");

    // Pre-populate.
    let mut f = std::fs::File::create(&path).unwrap();
    writeln!(f, "{}", serde_json::to_string(&ev(1)).unwrap()).unwrap();
    writeln!(f, "{}", serde_json::to_string(&ev(2)).unwrap()).unwrap();
    drop(f);

    let mut tail = Tail::open(&path, true).unwrap();
    let initial = tail.drain(Duration::from_millis(50)).unwrap();
    assert_eq!(initial.len(), 2);

    // Append more.
    let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
    writeln!(f, "{}", serde_json::to_string(&ev(3)).unwrap()).unwrap();
    drop(f);

    let more = tail.drain(Duration::from_millis(200)).unwrap();
    assert_eq!(more.len(), 1);
    assert_eq!(more[0].ts_ns, 3);
}

#[test]
fn no_follow_returns_eof_after_initial_read() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.jsonl");
    let mut f = std::fs::File::create(&path).unwrap();
    writeln!(f, "{}", serde_json::to_string(&ev(1)).unwrap()).unwrap();
    drop(f);
    let mut tail = Tail::open(&path, false).unwrap();
    let initial = tail.drain(Duration::from_millis(50)).unwrap();
    assert_eq!(initial.len(), 1);
    let none = tail.drain(Duration::from_millis(50)).unwrap();
    assert!(none.is_empty());
}

#[test]
fn zero_budget_follow_reopens_after_file_replacement() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.jsonl");
    std::fs::write(&path, format!("{}\n", serde_json::to_string(&ev(1)).unwrap())).unwrap();

    let mut tail = Tail::open(&path, true).unwrap();
    assert_eq!(tail.drain(Duration::ZERO).unwrap().len(), 1);

    let replacement = dir.path().join("replacement.jsonl");
    std::fs::write(
        &replacement,
        format!(
            "{}\n{}\n",
            serde_json::to_string(&ev(1)).unwrap(),
            serde_json::to_string(&ev(2)).unwrap()
        ),
    )
    .unwrap();
    std::fs::rename(replacement, &path).unwrap();

    let appended = tail.drain(Duration::ZERO).unwrap();
    assert_eq!(appended.len(), 1);
    assert_eq!(appended[0].ts_ns, 2);
}
