use peekaboo::view::discovery::{discover, SessionEntry};
use tempfile::TempDir;

fn touch_session(root: &std::path::Path, id: &str, status: &str, started: &str) {
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
        "tracer_pid": 2,
        "hostname": "h",
        "macos_version": "15.4",
        "peekaboo_version": "0.1.0"
    }}"#)).unwrap();
}

#[test]
fn discover_empty_returns_empty() {
    let root = TempDir::new().unwrap();
    let entries = discover(root.path()).unwrap();
    assert!(entries.is_empty());
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
