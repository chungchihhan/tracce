use peekaboo::event::{Event, EventData, EventKind, FileOp, NetProto, ProcessRef};
use std::net::SocketAddr;
use std::sync::Arc;

fn proc_ref(pid: u32, comm: &str) -> Arc<ProcessRef> {
    Arc::new(ProcessRef {
        pid,
        comm: comm.into(),
        image: format!("/usr/bin/{comm}").into(),
        argv: vec![comm.into()],
    })
}

#[test]
fn roundtrip_exec_event() {
    let e = Event {
        ts_ns: 1_700_000_000_000_000_000,
        kind: EventKind::Exec,
        pid: 4711,
        ppid: 4710,
        process: proc_ref(4711, "claude"),
        data: EventData::Exec {
            argv: vec!["claude".into(), "--print".into(), "hi".into()],
            image: "/usr/local/bin/claude".into(),
        },
        flags: 0,
    };
    let line = serde_json::to_string(&e).unwrap();
    let back: Event = serde_json::from_str(&line).unwrap();
    assert_eq!(e.kind, back.kind);
    assert_eq!(e.pid, back.pid);
    assert_eq!(e.ts_ns, back.ts_ns);
}

#[test]
fn roundtrip_file_event_with_sensitive_flag() {
    let e = Event {
        ts_ns: 1_700_000_000_000_000_001,
        kind: EventKind::Open,
        pid: 4719,
        ppid: 4711,
        process: proc_ref(4719, "node"),
        data: EventData::File {
            op: FileOp::Open,
            path: "/Users/x/.aws/credentials".into(),
            size: None,
        },
        flags: peekaboo::event::FLAG_SENSITIVE,
    };
    let line = serde_json::to_string(&e).unwrap();
    let back: Event = serde_json::from_str(&line).unwrap();
    assert_eq!(back.flags & peekaboo::event::FLAG_SENSITIVE, peekaboo::event::FLAG_SENSITIVE);
}

#[test]
fn roundtrip_net_open() {
    let remote: SocketAddr = "54.230.93.21:443".parse().unwrap();
    let local: SocketAddr = "192.168.1.5:55321".parse().unwrap();
    let e = Event {
        ts_ns: 1_700_000_000_000_000_002,
        kind: EventKind::NetOpen,
        pid: 4719,
        ppid: 4711,
        process: proc_ref(4719, "node"),
        data: EventData::NetOpen {
            remote,
            local,
            host: Some("api.anthropic.com".into()),
            proto: NetProto::Tcp,
        },
        flags: 0,
    };
    let line = serde_json::to_string(&e).unwrap();
    let back: Event = serde_json::from_str(&line).unwrap();
    match back.data {
        EventData::NetOpen { host, proto, .. } => {
            assert_eq!(host.as_deref(), Some("api.anthropic.com"));
            assert!(matches!(proto, NetProto::Tcp));
        }
        _ => panic!("expected NetOpen"),
    }
}
