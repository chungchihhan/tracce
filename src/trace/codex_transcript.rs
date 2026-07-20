//! Best-effort Codex rollout tailer.
//!
//! Codex keeps local rollout JSONL under `$CODEX_HOME/sessions` (normally
//! `~/.codex/sessions`). The file is an intent stream, not an audit record:
//! eslogger remains authoritative for the process and filesystem effects.
//!
//! The rollout format is intentionally parsed defensively. Unknown records,
//! malformed lines, and private desktop orchestration payloads are ignored so
//! a format change can only reduce enrichment, never stop kernel tracing.

use crate::event::{Event, EventData, EventKind, FileOp, ProcessRef};
use anyhow::{Context, Result};
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime};

use super::run::ThreadStop;

const POLL: Duration = Duration::from_millis(300);
const MAX_MATCH_DISTANCE: Duration = Duration::from_secs(6 * 60 * 60);
const AMBIGUOUS_DISTANCE: Duration = Duration::from_secs(1);

pub fn start_transcript_thread(
    root_pid: u32,
    cwd: PathBuf,
    tx: SyncSender<Event>,
) -> Result<ThreadStop> {
    let sessions_dir = sessions_dir().context("could not resolve CODEX_HOME/sessions")?;
    if !sessions_dir.exists() {
        anyhow::bail!("{} does not exist", sessions_dir.display());
    }

    let flag = Arc::new(AtomicBool::new(false));
    let stop = flag.clone();
    let selection_started = SystemTime::now();

    let join = thread::spawn(move || {
        let proc_ref = Arc::new(ProcessRef {
            pid: root_pid,
            comm: "codex".to_string(),
            image: PathBuf::new(),
            argv: Vec::new(),
        });
        let mut current: Option<TailState> = None;

        while !stop.load(Ordering::SeqCst) {
            if let Some(candidate) = matching_rollout(&sessions_dir, &cwd, selection_started) {
                let switch = current
                    .as_ref()
                    .map(|s| s.path != candidate)
                    .unwrap_or(true);
                if switch {
                    if let Ok(file) = File::open(&candidate) {
                        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
                        current = Some(TailState {
                            path: candidate,
                            file,
                            offset: len,
                            pending: String::new(),
                        });
                    }
                }
            }

            if let Some(state) = current.as_mut() {
                drain_new_lines(state, root_pid, &proc_ref, &tx);
            }

            thread::sleep(POLL);
        }
    });

    Ok(ThreadStop::new(flag, join))
}

struct TailState {
    path: PathBuf,
    file: File,
    offset: u64,
    /// Preserve an unterminated last line until Codex finishes writing it.
    pending: String,
}

fn drain_new_lines(
    state: &mut TailState,
    root_pid: u32,
    proc_ref: &Arc<ProcessRef>,
    tx: &SyncSender<Event>,
) {
    let new_len = match state.file.metadata() {
        Ok(m) => m.len(),
        Err(_) => return,
    };
    if new_len < state.offset {
        // A rollout was replaced/truncated. Start over at its current end so
        // we do not replay the old history or spin on an invalid offset.
        state.pending.clear();
        state.offset = new_len;
        let _ = state.file.seek(SeekFrom::Start(new_len));
        return;
    }
    if new_len == state.offset {
        return;
    }
    if state.file.seek(SeekFrom::Start(state.offset)).is_err() {
        return;
    }

    let mut bytes = Vec::new();
    if state.file.read_to_end(&mut bytes).is_err() {
        return;
    }
    state.pending.push_str(&String::from_utf8_lossy(&bytes));

    while let Some(end) = state.pending.find('\n') {
        let line = state.pending[..end].to_string();
        state.pending.drain(..=end);
        for ev in parse_rollout_line(&line, root_pid, proc_ref) {
            if tx.send(ev).is_err() {
                return;
            }
        }
    }
    state.offset = new_len.saturating_sub(state.pending.len() as u64);
}

fn parse_rollout_line(line: &str, root_pid: u32, proc_ref: &Arc<ProcessRef>) -> Vec<Event> {
    let value: Value = match serde_json::from_str(line.trim()) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let Some(payload) = value.get("payload") else {
        return Vec::new();
    };
    if value.get("type").and_then(Value::as_str) != Some("response_item") {
        return Vec::new();
    }

    match payload.get("type").and_then(Value::as_str) {
        Some("function_call") | Some("custom_tool_call") => {
            let name = payload.get("name").and_then(Value::as_str).unwrap_or("");
            let args = payload.get("arguments").or_else(|| payload.get("input"));
            parse_call(name, args, root_pid, proc_ref)
        }
        Some("message") => parse_message_calls(payload, root_pid, proc_ref),
        _ => Vec::new(),
    }
}

fn parse_message_calls(payload: &Value, root_pid: u32, proc_ref: &Arc<ProcessRef>) -> Vec<Event> {
    let Some(content) = payload.get("content").and_then(Value::as_array) else {
        return Vec::new();
    };
    content
        .iter()
        .flat_map(|call| {
            let name = call.get("name").and_then(Value::as_str).unwrap_or("");
            let args = call.get("arguments").or_else(|| call.get("input"));
            parse_call(name, args, root_pid, proc_ref)
        })
        .collect()
}

fn parse_call(
    name: &str,
    args: Option<&Value>,
    root_pid: u32,
    proc_ref: &Arc<ProcessRef>,
) -> Vec<Event> {
    let Some(args) = args.and_then(as_object) else {
        return Vec::new();
    };
    match name {
        "exec_command" | "shell" | "local_shell_call" | "exec" => {
            let Some(command) = string_field(&args, &["cmd", "command", "command_line"]) else {
                return Vec::new();
            };
            vec![file_event(root_pid, proc_ref, FileOp::Bash, command)]
        }
        "apply_patch" => {
            let Some(patch) = string_field(&args, &["patch", "diff"]) else {
                return Vec::new();
            };
            patch_events(&patch, root_pid, proc_ref)
        }
        _ => Vec::new(),
    }
}

fn as_object(value: &Value) -> Option<Value> {
    match value {
        Value::Object(_) => Some(value.clone()),
        // CLI function-call arguments are commonly JSON encoded as a string.
        // A desktop `exec` payload may instead be JavaScript; that is not a
        // supported contract and intentionally returns None here.
        Value::String(s) => serde_json::from_str(s).ok(),
        _ => None,
    }
}

fn string_field(value: &Value, names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| value.get(*name).and_then(Value::as_str).map(str::to_owned))
}

fn patch_events(patch: &str, root_pid: u32, proc_ref: &Arc<ProcessRef>) -> Vec<Event> {
    let mut paths = Vec::new();
    let mut ops = Vec::new();
    for line in patch.lines() {
        let (prefix, op) = if let Some(path) = line.strip_prefix("*** Update File: ") {
            (path, FileOp::Edit)
        } else if let Some(path) = line.strip_prefix("*** Add File: ") {
            (path, FileOp::Write)
        } else if let Some(path) = line.strip_prefix("*** Delete File: ") {
            (path, FileOp::Delete)
        } else {
            continue;
        };
        paths.push(prefix.trim().to_string());
        ops.push(op);
    }
    if paths.len() > 1 {
        return paths
            .into_iter()
            .map(|path| file_event(root_pid, proc_ref, FileOp::MultiEdit, path))
            .collect();
    }
    paths
        .into_iter()
        .zip(ops)
        .map(|(path, op)| file_event(root_pid, proc_ref, op, path))
        .collect()
}

fn file_event(
    root_pid: u32,
    proc_ref: &Arc<ProcessRef>,
    op: FileOp,
    path: impl Into<PathBuf>,
) -> Event {
    Event {
        ts_ns: super::run::now_ns(),
        kind: file_op_to_event_kind(op),
        pid: root_pid,
        ppid: 0,
        process: proc_ref.clone(),
        data: EventData::File {
            op,
            path: path.into(),
            size: None,
        },
        flags: 0,
    }
}

fn file_op_to_event_kind(op: FileOp) -> EventKind {
    match op {
        FileOp::Open => EventKind::Open,
        FileOp::Write => EventKind::Write,
        FileOp::Create => EventKind::Create,
        FileOp::Close => EventKind::Close,
        FileOp::Delete => EventKind::Unlink,
        FileOp::Rename => EventKind::Rename,
        FileOp::Edit => EventKind::Edit,
        FileOp::MultiEdit => EventKind::MultiEdit,
        FileOp::Bash => EventKind::Bash,
    }
}

fn sessions_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("CODEX_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(home).join("sessions"));
    }
    dirs::home_dir().map(|home| home.join(".codex").join("sessions"))
}

fn matching_rollout(root: &Path, cwd: &Path, started: SystemTime) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    collect_rollouts(root, &mut candidates);
    let mut scored = candidates
        .into_iter()
        .filter(|path| rollout_cwd(path).as_deref() == Some(cwd))
        .filter_map(|path| nearest_file_time(&path, started).map(|score| (score, path)))
        .collect::<Vec<_>>();
    scored.sort_by_key(|(score, _)| *score);
    let (best_score, best_path) = scored.first()?.clone();
    if best_score > MAX_MATCH_DISTANCE {
        return None;
    }
    if let Some((second_score, _)) = scored.get(1) {
        if second_score.saturating_sub(best_score) <= AMBIGUOUS_DISTANCE {
            return None;
        }
    }
    Some(best_path)
}

fn collect_rollouts(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rollouts(&path, out);
        } else if path
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.starts_with("rollout-"))
            && path.extension().and_then(|s| s.to_str()) == Some("jsonl")
        {
            out.push(path);
        }
    }
}

fn rollout_cwd(path: &Path) -> Option<PathBuf> {
    let file = File::open(path).ok()?;
    for line in BufReader::new(file).lines().take(8).flatten() {
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) != Some("session_meta") {
            continue;
        }
        return value
            .get("payload")
            .and_then(|p| p.get("cwd"))
            .or_else(|| value.get("cwd"))
            .and_then(Value::as_str)
            .map(PathBuf::from);
    }
    None
}

fn nearest_file_time(path: &Path, target: SystemTime) -> Option<Duration> {
    let metadata = path.metadata().ok()?;
    [metadata.created().ok(), metadata.modified().ok()]
        .into_iter()
        .flatten()
        .map(|t| {
            t.duration_since(target)
                .or_else(|_| target.duration_since(t))
                .unwrap_or(Duration::MAX)
        })
        .min()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn proc_ref() -> Arc<ProcessRef> {
        Arc::new(ProcessRef {
            pid: 100,
            comm: "codex".into(),
            image: PathBuf::new(),
            argv: vec![],
        })
    }

    #[test]
    fn extracts_exec_command_from_function_call() {
        let line = r#"{"type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"git status\"}"}}"#;
        let events = parse_rollout_line(line, 100, &proc_ref());
        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0].data,
            EventData::File {
                op: FileOp::Bash,
                ..
            }
        ));
    }

    #[test]
    fn extracts_files_from_apply_patch() {
        let line = r#"{"type":"response_item","payload":{"type":"function_call","name":"apply_patch","arguments":"{\"patch\":\"*** Begin Patch\\n*** Update File: src/main.rs\\n*** End Patch\"}"}}"#;
        let events = parse_rollout_line(line, 100, &proc_ref());
        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0].data,
            EventData::File {
                op: FileOp::Edit,
                ..
            }
        ));
    }

    #[test]
    fn ignores_unknown_and_private_payloads() {
        let unknown = r#"{"type":"event_msg","payload":{"type":"something_new"}}"#;
        let desktop_exec = r#"{"type":"response_item","payload":{"type":"custom_tool_call","name":"exec","input":"return await tools.exec_command(...)"}}"#;
        assert!(parse_rollout_line(unknown, 100, &proc_ref()).is_empty());
        assert!(parse_rollout_line(desktop_exec, 100, &proc_ref()).is_empty());
    }

    #[test]
    fn matches_rollout_by_metadata_cwd() {
        let root = tempfile::TempDir::new().unwrap();
        let day = root.path().join("2026/07/20");
        std::fs::create_dir_all(&day).unwrap();
        let wanted = day.join("rollout-wanted.jsonl");
        let other = day.join("rollout-other.jsonl");
        std::fs::write(
            &wanted,
            "{\"type\":\"session_meta\",\"payload\":{\"cwd\":\"/tmp/project\"}}\n",
        ).unwrap();
        std::fs::write(
            &other,
            "{\"type\":\"session_meta\",\"payload\":{\"cwd\":\"/tmp/other\"}}\n",
        ).unwrap();
        let got = matching_rollout(
            root.path(),
            Path::new("/tmp/project"),
            SystemTime::now(),
        ).unwrap();
        assert_eq!(got, wanted);
    }
}
