use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_ctrace"))
}

#[test]
fn help_lists_the_core_subcommands() {
    let out = bin().arg("--help").output().unwrap();
    assert!(out.status.success(), "--help should succeed");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Usage:"), "help should contain Usage:, got:\n{stdout}");
    for sub in ["claude", "attach", "exec", "view", "list"] {
        assert!(stdout.contains(sub), "help should mention `{sub}`, got:\n{stdout}");
    }
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
fn attach_help_documents_optional_pid() {
    let out = bin().args(["attach", "--help"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.to_lowercase().contains("pid"), "attach help should mention pid, got:\n{stdout}");
}

#[test]
fn exec_requires_a_command_arg() {
    let out = bin().arg("exec").output().unwrap();
    assert!(!out.status.success(), "`exec` with no command should be an error");
}
