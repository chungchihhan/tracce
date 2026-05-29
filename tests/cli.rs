use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_peekaboo"))
}

#[test]
fn shows_help_with_no_args() {
    let out = bin().output().unwrap();
    assert!(!out.status.success(), "no-args should be an error");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("Usage:"), "stderr should contain Usage: line, got:\n{stderr}");
}

#[test]
fn list_subcommand_runs() {
    let out = bin().arg("list").output().unwrap();
    assert!(out.status.success(), "list should succeed even with no sessions");
}

#[test]
fn view_help() {
    let out = bin().args(["view", "--help"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("--latest"));
}

#[test]
fn trace_requires_command_arg() {
    let out = bin().arg("trace").output().unwrap();
    assert!(!out.status.success());
}
