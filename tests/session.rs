use tracce::trace::session::{Session, SessionStatus};
use tracce::trace::provider::Provider;
use tempfile::TempDir;

#[test]
fn session_creates_dir_with_expected_files() {
    let root = TempDir::new().unwrap();
    let s = Session::create(
        root.path(),
        Provider::Claude,
        4711,
        std::process::id(),
        &["claude".into(), "--print".into(), "hi".into()],
        std::path::Path::new("/Users/x/Developer/Personal/tracce"),
    ).unwrap();

    assert!(s.dir().exists());
    assert!(s.dir().join("events.jsonl").exists());
    assert!(s.dir().join("meta.json").exists());

    let status = std::fs::read_to_string(s.dir().join("status")).unwrap();
    assert_eq!(status.trim(), "live");
}

#[test]
fn session_id_contains_cwd_basename_and_pid() {
    let root = TempDir::new().unwrap();
    let s = Session::create(
        root.path(),
        Provider::Claude,
        4711,
        std::process::id(),
        &["claude".into()],
        std::path::Path::new("/Users/x/Developer/Personal/tracce"),
    ).unwrap();

    let id = s.id();
    assert!(id.contains("tracce"));
    assert!(id.contains("4711"));
}

#[test]
fn session_records_provider_and_neutral_root_pid() {
    let root = TempDir::new().unwrap();
    let s = Session::create(
        root.path(),
        Provider::Codex,
        4711,
        std::process::id(),
        &["codex".into()],
        std::path::Path::new("/tmp"),
    ).unwrap();
    let meta: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(s.dir().join("meta.json")).unwrap(),
    ).unwrap();
    assert_eq!(meta["provider"], "codex");
    assert_eq!(meta["root_pid"], 4711);
    assert!(meta.get("claude_pid").is_none());
}

#[test]
fn legacy_metadata_infers_provider_from_root_command() {
    let meta: tracce::trace::session::Meta = serde_json::from_str(r#"{
        "session_id": "legacy",
        "started_at": "2026-05-28T10:00:00Z",
        "ended_at": null,
        "cwd": "/tmp",
        "argv": ["bash", "-lc", "echo hi"],
        "claude_pid": 4711,
        "tracer_pid": 2,
        "hostname": "h",
        "macos_version": "15.4",
        "tracce_version": "0.1.0"
    }"#).unwrap();

    assert_eq!(meta.provider, Provider::Other);
}

#[test]
fn session_mark_done_updates_status_and_ended_at() {
    let root = TempDir::new().unwrap();
    let s = Session::create(
        root.path(),
        Provider::Claude,
        4711,
        std::process::id(),
        &["claude".into()],
        std::path::Path::new("/tmp"),
    ).unwrap();
    s.mark_status(SessionStatus::Done).unwrap();
    let status = std::fs::read_to_string(s.dir().join("status")).unwrap();
    assert_eq!(status.trim(), "done");

    let meta_text = std::fs::read_to_string(s.dir().join("meta.json")).unwrap();
    assert!(meta_text.contains("ended_at"));
    assert!(!meta_text.contains("\"ended_at\":null"));
}
