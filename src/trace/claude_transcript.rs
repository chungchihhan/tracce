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
use std::io::{Read, Seek, SeekFrom};
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
const NEW_TRANSCRIPT_WINDOW: Duration = Duration::from_secs(60);

pub fn start_transcript_thread(
    root_pid: u32,
    cwd: PathBuf,
    tx: SyncSender<Event>,
    replay_new_transcript: bool,
) -> Result<ThreadStop> {
    let projects_dir = transcript_dir_for(&cwd)
        .with_context(|| "could not resolve ~/.claude/projects directory")?;

    let flag = Arc::new(AtomicBool::new(false));
    let stop = flag.clone();
    let selection_started = SystemTime::now();

    let join = thread::spawn(move || {
        let proc_ref = Arc::new(ProcessRef {
            pid: root_pid,
            comm: "claude".to_string(),
            image: PathBuf::new(),
            argv: Vec::new(),
        });

        // We tail one file at a time. Wrapped launches match by creation time
        // so another active Claude session in the same project cannot steal the
        // tailer merely by writing more recently. Attach mode uses mtime.
        let mut current: Option<TailState> = None;

        while !stop.load(Ordering::SeqCst) {
            if let Some(newest) = matching_transcript(
                &projects_dir,
                selection_started,
                replay_new_transcript,
            ) {
                let switch = match &current {
                    Some(s) => s.path != newest,
                    None => true,
                };
                if switch {
                    if let Ok(file) = File::open(&newest) {
                        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
                        let offset = initial_offset(
                            &file,
                            len,
                            selection_started,
                            replay_new_transcript,
                        );
                        current = Some(TailState {
                            path: newest,
                            file,
                            offset,
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
    /// Preserve an unterminated final line until Claude finishes writing it.
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
        for ev in parse_transcript_line(&line, root_pid, proc_ref) {
            if tx.send(ev).is_err() {
                return;
            }
        }
    }
    // `pending` already owns the unterminated bytes, so continue reading from
    // the physical EOF rather than reading those bytes a second time.
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

fn matching_transcript(
    dir: &Path,
    started: SystemTime,
    prefer_created: bool,
) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut candidates = Vec::new();
    for entry in entries.flatten() {
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) != Some("jsonl") { continue; }
        let metadata = entry.metadata().ok()?;
        let score = transcript_time_score(
            metadata.created().ok(),
            metadata.modified().ok(),
            started,
            prefer_created,
        )?;
        candidates.push((score, p));
    }
    candidates.sort_by_key(|(score, _)| *score);
    let (best_score, best_path) = candidates.first()?.clone();
    if prefer_created && best_score > MAX_MATCH_DISTANCE {
        return None;
    }
    if let Some((second_score, _)) = candidates.get(1) {
        if second_score.saturating_sub(best_score) <= AMBIGUOUS_DISTANCE {
            return None;
        }
    }
    Some(best_path)
}

fn transcript_time_score(
    created: Option<SystemTime>,
    modified: Option<SystemTime>,
    target: SystemTime,
    prefer_created: bool,
) -> Option<Duration> {
    let times = if prefer_created {
        [created.or(modified), None]
    } else {
        [created, modified]
    };
    times
        .into_iter()
        .flatten()
        .map(|time| {
            time.duration_since(target)
                .or_else(|_| target.duration_since(time))
                .unwrap_or(Duration::MAX)
        })
        .min()
}

fn initial_offset(
    file: &File,
    len: u64,
    selection_started: SystemTime,
    replay_new_transcript: bool,
) -> u64 {
    if !replay_new_transcript {
        return len;
    }
    let created_near_start = file
        .metadata()
        .ok()
        .and_then(|metadata| metadata.created().ok())
        .and_then(|created| {
            created
                .duration_since(selection_started)
                .or_else(|_| selection_started.duration_since(created))
                .ok()
        })
        .is_some_and(|distance| distance <= NEW_TRANSCRIPT_WINDOW);
    if created_near_start { 0 } else { len }
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
    use std::io::Write as _;

    fn proc_ref() -> Arc<ProcessRef> {
        Arc::new(ProcessRef {
            pid: 100,
            comm: "claude".into(),
            image: PathBuf::new(),
            argv: vec![],
        })
    }

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
        let events = parse_transcript_line(line, 100, &proc_ref());
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
        let events = parse_transcript_line(line, 100, &proc_ref());
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
        assert!(parse_transcript_line(user, 100, &proc_ref()).is_empty());
        assert!(parse_transcript_line(unknown, 100, &proc_ref()).is_empty());
    }

    #[test]
    fn extracts_read_write_and_multi_edit_tool_calls() {
        let line = r#"{
            "type": "assistant",
            "message": {"content": [
                {"type": "tool_use", "name": "Read", "input": {"file_path": "/x/in.rs"}},
                {"type": "tool_use", "name": "Write", "input": {"file_path": "/x/out.rs"}},
                {"type": "tool_use", "name": "MultiEdit", "input": {"file_path": "/x/many.rs"}}
            ]}
        }"#;
        let events = parse_transcript_line(line, 100, &proc_ref());
        assert_eq!(events.len(), 3);
        assert!(matches!(&events[0].data, EventData::File { op: FileOp::Open, .. }));
        assert!(matches!(&events[1].data, EventData::File { op: FileOp::Write, .. }));
        assert!(matches!(&events[2].data, EventData::File { op: FileOp::MultiEdit, .. }));
    }

    #[test]
    fn launch_matching_ignores_an_old_transcripts_recent_modification() {
        let target = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let old_created = target - Duration::from_secs(600);
        let recently_modified = target + Duration::from_secs(1);
        let new_created = target + Duration::from_secs(20);

        assert_eq!(
            transcript_time_score(Some(old_created), Some(recently_modified), target, true),
            Some(Duration::from_secs(600))
        );
        assert_eq!(
            transcript_time_score(Some(new_created), Some(new_created), target, true),
            Some(Duration::from_secs(20))
        );
        assert_eq!(
            transcript_time_score(Some(old_created), Some(recently_modified), target, false),
            Some(Duration::from_secs(1))
        );
    }

    #[test]
    fn replays_a_transcript_created_during_launch_but_not_attach() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), "already written\n").unwrap();
        let opened = File::open(file.path()).unwrap();
        let len = opened.metadata().unwrap().len();

        assert_eq!(initial_offset(&opened, len, SystemTime::now(), true), 0);
        assert_eq!(initial_offset(&opened, len, SystemTime::now(), false), len);
    }

    #[test]
    fn preserves_a_tool_call_split_across_writes() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"git status"}}]}}"#;
        let split = line.len() / 2;
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), &line[..split]).unwrap();
        let opened = File::open(file.path()).unwrap();
        let mut state = TailState {
            path: file.path().to_path_buf(),
            file: opened,
            offset: 0,
            pending: String::new(),
        };
        let (tx, rx) = std::sync::mpsc::sync_channel(4);

        drain_new_lines(&mut state, 100, &proc_ref(), &tx);
        assert!(rx.try_recv().is_err());

        let mut append = std::fs::OpenOptions::new()
            .append(true)
            .open(file.path())
            .unwrap();
        writeln!(append, "{}", &line[split..]).unwrap();
        drop(append);

        drain_new_lines(&mut state, 100, &proc_ref(), &tx);
        let event = rx.try_recv().expect("completed line should emit an event");
        assert!(matches!(event.data, EventData::File { op: FileOp::Bash, .. }));
    }
}
