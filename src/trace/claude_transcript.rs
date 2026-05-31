//! Claude Code session-transcript tailer.
//!
//! Claude writes every tool call to `~/.claude/projects/<encoded-cwd>/<session-id>.jsonl`.
//! Each line is a JSON object; assistant messages may contain `tool_use` entries
//! with the file path / command claude wants to act on. We tail the newest such
//! file and emit synthetic `EventData::File` events for the tools we care about.
//!
//! Coverage: Edit, Write, MultiEdit, Read, Bash. Other tools (Glob, Grep,
//! WebFetch, TodoWrite, Task) are not surfaced — extend `parse_tool_use` to
//! add them.
//!
//! Attribution: every emitted event is tagged with the wrapped command's root
//! pid, so the rows slot under "claude" in the process tree.

use crate::event::{Event, EventData, EventKind, FileOp, ProcessRef};
use anyhow::{Context, Result};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use super::run::ThreadStop;

const POLL: Duration = Duration::from_millis(300);

pub fn start_transcript_thread(
    root_pid: u32,
    cwd: PathBuf,
    tx: SyncSender<Event>,
) -> Result<ThreadStop> {
    let projects_dir = transcript_dir_for(&cwd)
        .with_context(|| "could not resolve ~/.claude/projects directory")?;

    let flag = Arc::new(AtomicBool::new(false));
    let stop = flag.clone();

    let join = thread::spawn(move || {
        let proc_ref = Arc::new(ProcessRef {
            pid: root_pid,
            comm: "claude".to_string(),
            image: PathBuf::new(),
            argv: Vec::new(),
        });

        // We tail one file at a time — the newest .jsonl in the project dir
        // whose mtime is fresh enough that it's likely the active session.
        let mut current: Option<TailState> = None;

        while !stop.load(Ordering::SeqCst) {
            if let Some(newest) = newest_jsonl(&projects_dir) {
                let switch = match &current {
                    Some(s) => s.path != newest,
                    None => true,
                };
                if switch {
                    // Open new file and seek to end so we don't replay history.
                    if let Ok(file) = File::open(&newest) {
                        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
                        current = Some(TailState { path: newest, file, offset: len });
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
    if new_len <= state.offset {
        return;
    }
    if state.file.seek(SeekFrom::Start(state.offset)).is_err() {
        return;
    }
    let mut reader = BufReader::new(&mut state.file);
    let mut buf = String::new();
    loop {
        buf.clear();
        match reader.read_line(&mut buf) {
            Ok(0) => break,
            Ok(_) => {
                for ev in parse_transcript_line(&buf, root_pid, proc_ref) {
                    if tx.send(ev).is_err() { return; }
                }
            }
            Err(_) => break,
        }
    }
    state.offset = new_len;
}

fn parse_transcript_line(
    line: &str,
    root_pid: u32,
    proc_ref: &Arc<ProcessRef>,
) -> Vec<Event> {
    let v: serde_json::Value = match serde_json::from_str(line.trim()) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    // Only assistant turns contain tool_use entries. Older transcript formats
    // nest content differently, so check a couple of likely paths.
    if v.get("type").and_then(|t| t.as_str()) != Some("assistant") {
        return Vec::new();
    }
    let content = v
        .get("message")
        .and_then(|m| m.get("content"))
        .or_else(|| v.get("content"))
        .and_then(|c| c.as_array());
    let Some(content) = content else { return Vec::new(); };

    let mut out = Vec::new();
    for entry in content {
        if entry.get("type").and_then(|t| t.as_str()) != Some("tool_use") {
            continue;
        }
        let Some(ev) = parse_tool_use(entry, root_pid, proc_ref) else { continue };
        out.push(ev);
    }
    out
}

fn parse_tool_use(
    entry: &serde_json::Value,
    root_pid: u32,
    proc_ref: &Arc<ProcessRef>,
) -> Option<Event> {
    let name = entry.get("name").and_then(|n| n.as_str())?;
    let input = entry.get("input")?;

    let (op, path_str) = match name {
        "Edit" => (FileOp::Edit, input.get("file_path")?.as_str()?.to_string()),
        "Write" => (FileOp::Write, input.get("file_path")?.as_str()?.to_string()),
        "MultiEdit" => (FileOp::MultiEdit, input.get("file_path")?.as_str()?.to_string()),
        "Read" => (FileOp::Open, input.get("file_path")?.as_str()?.to_string()),
        "Bash" => (FileOp::Bash, input.get("command")?.as_str()?.to_string()),
        _ => return None,
    };

    Some(Event {
        ts_ns: now_ns(),
        kind: file_op_to_event_kind(op),
        pid: root_pid,
        ppid: 0,
        process: proc_ref.clone(),
        data: EventData::File { op, path: PathBuf::from(path_str), size: None },
        flags: 0,
    })
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

/// `/Users/h/proj/foo` → `~/.claude/projects/-Users-h-proj-foo`.
///
/// Claude encodes the cwd by replacing every character that isn't
/// ASCII-alphanumeric with `-`, not just `/`. So `_` and `.` collapse too:
/// `/Users/h/DDEI_5.1_Rocky` → `-Users-h-DDEI-5-1-Rocky`. Getting this wrong
/// means we never find the project dir for any path containing `_`/`.`.
pub fn transcript_dir_for(cwd: &Path) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let cwd_str = cwd.to_str()?;
    let encoded: String = cwd_str
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    Some(home.join(".claude").join("projects").join(encoded))
}

fn newest_jsonl(dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut best: Option<(PathBuf, std::time::SystemTime)> = None;
    for entry in entries.flatten() {
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) != Some("jsonl") { continue; }
        let mtime = entry.metadata().ok().and_then(|m| m.modified().ok())?;
        match &best {
            Some((_, t)) if *t >= mtime => {}
            _ => best = Some((p, mtime)),
        }
    }
    best.map(|(p, _)| p)
}

fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_cwd_to_claude_project_path() {
        let p = transcript_dir_for(Path::new("/Users/h/proj/foo")).unwrap();
        assert!(p.ends_with(".claude/projects/-Users-h-proj-foo"));
    }

    #[test]
    fn encodes_underscores_and_dots_as_dashes() {
        // Claude collapses every non-alphanumeric char to '-', so `_` and `.`
        // become dashes too — not just `/`.
        let p = transcript_dir_for(Path::new("/Users/harry_chung/Work/DDEI_5.1_Rocky")).unwrap();
        assert!(
            p.ends_with(".claude/projects/-Users-harry-chung-Work-DDEI-5-1-Rocky"),
            "got {p:?}"
        );
    }

    #[test]
    fn extracts_bash_tool_call() {
        let line = r#"{
            "type": "assistant",
            "message": {"content": [
                {"type": "tool_use", "name": "Bash", "input": {"command": "git status"}}
            ]}
        }"#;
        let proc_ref = Arc::new(ProcessRef {
            pid: 100, comm: "claude".into(), image: PathBuf::new(), argv: vec![],
        });
        let events = parse_transcript_line(line, 100, &proc_ref);
        assert_eq!(events.len(), 1);
        if let EventData::File { op, path, .. } = &events[0].data {
            assert_eq!(*op, FileOp::Bash);
            assert_eq!(path, &PathBuf::from("git status"));
        } else { panic!("expected File variant"); }
    }

    #[test]
    fn extracts_edit_tool_call() {
        let line = r#"{
            "type": "assistant",
            "message": {"content": [
                {"type": "tool_use", "name": "Edit",
                 "input": {"file_path": "/x/y.rs", "old_string": "a", "new_string": "b"}}
            ]}
        }"#;
        let proc_ref = Arc::new(ProcessRef {
            pid: 100, comm: "claude".into(), image: PathBuf::new(), argv: vec![],
        });
        let events = parse_transcript_line(line, 100, &proc_ref);
        assert_eq!(events.len(), 1);
        if let EventData::File { op, path, .. } = &events[0].data {
            assert_eq!(*op, FileOp::Edit);
            assert_eq!(path, &PathBuf::from("/x/y.rs"));
        } else { panic!("expected File variant"); }
    }

    #[test]
    fn ignores_user_messages_and_unknown_tools() {
        let user = r#"{"type": "user", "message": {"content": [{"type": "text"}]}}"#;
        let unknown = r#"{
            "type": "assistant",
            "message": {"content": [
                {"type": "tool_use", "name": "Glob", "input": {"pattern": "**/*.rs"}}
            ]}
        }"#;
        let proc_ref = Arc::new(ProcessRef {
            pid: 100, comm: "claude".into(), image: PathBuf::new(), argv: vec![],
        });
        assert!(parse_transcript_line(user, 100, &proc_ref).is_empty());
        assert!(parse_transcript_line(unknown, 100, &proc_ref).is_empty());
    }
}
