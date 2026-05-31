use ctrace::trace::session::{Session, SessionStatus};
use tempfile::TempDir;

#[test]
fn session_creates_dir_with_expected_files() {
    let root = TempDir::new().unwrap();
    let s = Session::create(
        root.path(),
        4711,
        std::process::id(),
        &["claude".into(), "--print".into(), "hi".into()],
        std::path::Path::new("/Users/x/Developer/Personal/ctrace"),
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
        4711,
        std::process::id(),
        &["claude".into()],
        std::path::Path::new("/Users/x/Developer/Personal/ctrace"),
    ).unwrap();

    let id = s.id();
    assert!(id.contains("ctrace"));
    assert!(id.contains("4711"));
}

#[test]
fn session_mark_done_updates_status_and_ended_at() {
    let root = TempDir::new().unwrap();
    let s = Session::create(
        root.path(),
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
