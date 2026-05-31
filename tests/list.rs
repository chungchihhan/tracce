use std::process::Command;
use tempfile::TempDir;

fn bin() -> Command { Command::new(env!("CARGO_BIN_EXE_ctrace")) }

#[test]
fn list_empty_root_succeeds_with_header_only() {
    let root = TempDir::new().unwrap();
    let out = bin()
        .env("CTRACE_HOME", root.path())
        .arg("list")
        .output().unwrap();
    assert!(out.status.success());
    let s = String::from_utf8(out.stdout).unwrap();
    assert!(s.starts_with("STATUS"));
    assert_eq!(s.lines().count(), 1);
}

#[test]
fn list_with_one_session_shows_one_row() {
    let root = TempDir::new().unwrap();
    let sessions = root.path().join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let id = "2026-05-28T22-04-31_demo_4711";
    let d = sessions.join(id);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("status"), "live\n").unwrap();
    std::fs::write(d.join("events.jsonl"), "{}\n{}\n").unwrap();
    std::fs::write(d.join("meta.json"), r#"{
        "session_id": "2026-05-28T22-04-31_demo_4711",
        "started_at": "2026-05-28T22:04:31Z",
        "ended_at": null,
        "cwd": "/tmp/demo",
        "argv": ["claude"],
        "claude_pid": 4711,
        "tracer_pid": 4710,
        "hostname": "h",
        "macos_version": "15.4",
        "ctrace_version": "0.1.0"
    }"#).unwrap();

    let out = bin()
        .env("CTRACE_HOME", root.path())
        .arg("list")
        .output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let s = String::from_utf8(out.stdout).unwrap();
    assert_eq!(s.lines().count(), 2);
    assert!(s.contains("demo"));
    assert!(s.contains("4711"));
}
