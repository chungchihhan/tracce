use tracce::trace::eslogger::parse_line;
use tracce::event::{EventKind, EventData, FileOp};

#[test]
fn parses_exec_fixture() {
    let raw = include_str!("fixtures/eslogger_exec.json");
    let ev = parse_line(raw).unwrap().expect("exec should parse");
    assert_eq!(ev.kind, EventKind::Exec);
    assert_eq!(ev.pid, 52674);
    match ev.data {
        EventData::Exec { argv, image } => {
            assert_eq!(argv, vec!["/bin/zsh", "-f", "-c", "GIT_OPTIONAL_LOCKS=0 git symbolic-ref --short HEAD"]);
            assert_eq!(image.to_str().unwrap(), "/bin/zsh");
        }
        _ => panic!("expected Exec"),
    }
}

#[test]
fn parses_open_fixture() {
    let raw = include_str!("fixtures/eslogger_open.json");
    let ev = parse_line(raw).unwrap().expect("open should parse");
    assert_eq!(ev.kind, EventKind::Open);
    assert_eq!(ev.pid, 6954);
    match ev.data {
        EventData::File { op, path, .. } => {
            assert_eq!(op, FileOp::Open);
            assert!(path.to_str().unwrap().ends_with("macvpn.222"));
        }
        _ => panic!("expected File"),
    }
}

#[test]
fn unknown_event_returns_none() {
    let raw = r#"{"event_type": 999999, "event": {}, "process": {"audit_token": {"pid": 1, "auid": 0, "euid": 0, "egid": 0, "ruid": 0, "rgid": 0, "asid": 0, "pidversion": 0}, "ppid": 0, "executable": {"path": "/x"}}, "time": "2026-05-29T07:28:26.228785687Z"}"#;
    let result = parse_line(raw).unwrap();
    assert!(result.is_none());
}

#[test]
fn parses_fork_fixture() {
    let raw = r#"{"event":{"fork":{"child":{"audit_token":{"pid":12345}}}},"process":{"audit_token":{"pid":1234},"ppid":1,"executable":{"path":"/bin/bash"}},"time":"2026-05-29T07:59:42.757444182Z"}"#;
    let ev = tracce::trace::eslogger::parse_line(raw).unwrap().expect("fork should parse");
    assert_eq!(ev.kind, tracce::event::EventKind::Fork);
    assert_eq!(ev.pid, 1234);
    match ev.data {
        tracce::event::EventData::Fork { child_pid } => {
            assert_eq!(child_pid, 12345);
        }
        _ => panic!("expected Fork data"),
    }
}
