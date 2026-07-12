use tracce::view::discovery::{discover, SessionEntry};
use tempfile::TempDir;

fn touch_session(root: &std::path::Path, id: &str, status: &str, started: &str) {
    // Use the current process PID as tracer_pid so that "live" sessions pass
    // the pid_alive check (the test process is definitely alive).
    let tracer_pid = std::process::id();
    let d = root.join("sessions").join(id);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("status"), format!("{status}\n")).unwrap();
    std::fs::write(d.join("events.jsonl"), "").unwrap();
    std::fs::write(d.join("meta.json"), format!(r#"{{
        "session_id": "{id}",
        "started_at": "{started}",
        "ended_at": null,
        "cwd": "/tmp/{id}",
        "argv": ["claude"],
        "claude_pid": 1,
        "tracer_pid": {tracer_pid},
        "hostname": "h",
        "macos_version": "15.4",
        "tracce_version": "0.1.0"
    }}"#)).unwrap();
}

#[test]
fn discover_empty_returns_empty() {
    let root = TempDir::new().unwrap();
    let entries = discover(root.path()).unwrap();
    assert!(entries.is_empty());
}

#[test]
fn discover_reads_pre_rename_ctrace_version_key() {
    // Sessions recorded before the ctrace→tracce rename store the version under
    // "ctrace_version". The serde alias must keep them discoverable.
    let root = TempDir::new().unwrap();
    let d = root.path().join("sessions").join("2026-05-28T10-00-00_old_1");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("status"), "done\n").unwrap();
    std::fs::write(d.join("events.jsonl"), "").unwrap();
    std::fs::write(d.join("meta.json"), r#"{
        "session_id": "2026-05-28T10-00-00_old_1",
        "started_at": "2026-05-28T10:00:00Z",
        "ended_at": null,
        "cwd": "/tmp/old",
        "argv": ["claude"],
        "claude_pid": 1,
        "tracer_pid": 1,
        "hostname": "h",
        "macos_version": "15.4",
        "ctrace_version": "0.1.0"
    }"#).unwrap();

    let entries = discover(root.path()).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].meta.tracce_version, "0.1.0");
}

#[test]
fn discover_orders_live_first_then_recent() {
    let root = TempDir::new().unwrap();
    touch_session(root.path(), "2026-05-28T10-00-00_a_1", "done", "2026-05-28T10:00:00Z");
    touch_session(root.path(), "2026-05-28T12-00-00_b_2", "live", "2026-05-28T12:00:00Z");
    touch_session(root.path(), "2026-05-28T11-00-00_c_3", "done", "2026-05-28T11:00:00Z");

    let entries = discover(root.path()).unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].status, "live");
    // Done entries are most-recent-first.
    assert!(entries[1].meta.session_id.contains("c_3"));
    assert!(entries[2].meta.session_id.contains("a_1"));
}

#[test]
fn app_ingests_events_into_panes() {
    use tracce::event::{Event, EventData, EventKind, FileOp, ProcessRef};
    use tracce::flags::FlagConfig;
    use tracce::view::discovery::SessionEntry;
    use tracce::view::ui::app::App;
    use tracce::trace::session::Meta;
    use std::sync::Arc;

    let session = SessionEntry {
        dir: "/tmp".into(),
        meta: Meta {
            session_id: "s".into(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            cwd: "/tmp".into(),
            argv: vec!["claude".into()],
            claude_pid: 1, tracer_pid: 2,
            hostname: "h".into(), macos_version: "15".into(), tracce_version: "0.1".into(),
        },
        status: "live".into(),
        events_path: "/tmp/events.jsonl".into(),
    };
    let mut app = App::new(session, FlagConfig::empty());

    app.ingest(Event {
        ts_ns: 1, kind: EventKind::Open, pid: 4711, ppid: 1,
        process: Arc::new(ProcessRef { pid: 4711, comm: "node".into(), image: "/usr/bin/node".into(), argv: vec![] }),
        data: EventData::File { op: FileOp::Open, path: "/tmp/README.md".into(), size: None },
        flags: 0,
    });
    assert_eq!(app.recent_files.len(), 1);
}

#[test]
fn discover_marks_stale_live_as_crashed() {
    let root = TempDir::new().unwrap();
    let id = "2026-05-28T09-00-00_stale_99999";
    touch_session(root.path(), id, "live", "2026-05-28T09:00:00Z");
    // Overwrite meta.json to set tracer_pid to a guaranteed-dead pid.
    // We rewrite the entire file so we don't have to pattern-match the dynamic pid.
    let meta_path = root.path().join("sessions").join(id).join("meta.json");
    std::fs::write(&meta_path, format!(r#"{{
        "session_id": "{id}",
        "started_at": "2026-05-28T09:00:00Z",
        "ended_at": null,
        "cwd": "/tmp/{id}",
        "argv": ["claude"],
        "claude_pid": 1,
        "tracer_pid": 4294967290,
        "hostname": "h",
        "macos_version": "15.4",
        "tracce_version": "0.1.0"
    }}"#)).unwrap();

    let entries = tracce::view::discovery::discover(root.path()).unwrap();
    let entry = entries.iter().find(|e| e.meta.session_id == id).unwrap();
    assert_eq!(entry.status, "crashed");
    let status = std::fs::read_to_string(root.path().join("sessions").join(id).join("status")).unwrap();
    assert_eq!(status.trim(), "crashed");
}
