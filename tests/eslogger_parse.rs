use peekaboo::trace::eslogger::parse_line;
use peekaboo::event::{EventKind, EventData, FileOp};

#[test]
fn parses_exec_fixture() {
    let raw = include_str!("fixtures/eslogger_exec.json");
    let ev = parse_line(raw).unwrap().expect("exec should parse");
    assert_eq!(ev.kind, EventKind::Exec);
    assert_eq!(ev.pid, 4710);
    match ev.data {
        EventData::Exec { argv, image } => {
            assert_eq!(argv, vec!["claude", "--print", "hi"]);
            assert_eq!(image.to_str().unwrap(), "/usr/local/bin/claude");
        }
        _ => panic!("expected Exec"),
    }
}

#[test]
fn parses_open_fixture() {
    let raw = include_str!("fixtures/eslogger_open.json");
    let ev = parse_line(raw).unwrap().expect("open should parse");
    assert_eq!(ev.kind, EventKind::Open);
    match ev.data {
        EventData::File { op, path, .. } => {
            assert_eq!(op, FileOp::Open);
            assert!(path.to_str().unwrap().ends_with("README.md"));
        }
        _ => panic!("expected File"),
    }
}

#[test]
fn unknown_event_returns_none() {
    let raw = r#"{"event_type": 999999, "process": {"audit_token": {"pid": 1, "auid": 0, "euid": 0, "egid": 0, "ruid": 0, "rgid": 0, "asid": 0, "pidversion": 0}, "ppid": 0, "executable": {"path": "/x", "path_truncated": false}}, "time": {"tv_sec": 0, "tv_nsec": 0}}"#;
    let result = parse_line(raw).unwrap();
    assert!(result.is_none());
}
