//! Attach to an already-running claude and render the live TUI.
//!
//! Unlike `run` (which launches the command and owns nothing but a child
//! handle), attach hooks onto an existing pid. Because claude is in its own
//! terminal, attach is free to draw the live dashboard in *this* terminal —
//! it records to JSONL exactly like a wrap trace, then tails that same live
//! session file through the normal viewer (`view::run::run_entry`).
//!
//! Limitation: we can only capture events from the attach moment forward;
//! anything claude did before you attached is not in the recording.

use crate::event::Event;
use crate::trace::{
    aggregator::Aggregator,
    claude_transcript,
    persist::Persist,
    pid_tree::PidTree,
    run,
    session::{Session, SessionStatus},
};
use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

const RAW_CHAN_CAP: usize = 4096;
const FLUSH_INTERVAL: Duration = Duration::from_millis(250);

pub fn run(pid: Option<u32>, root: &Path) -> Result<()> {
    let target_pid = select_pid(pid)?;
    let command = proc_command(target_pid).unwrap_or_else(|| "claude".to_string());
    let cwd = proc_cwd(target_pid)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default();
    let tracer_pid = std::process::id();

    eprintln!("ctrace · attaching to pid {target_pid} ({command})");
    eprintln!("ctrace · cwd {}", cwd.display());

    // Raw events channel — shared by eslogger (if active), the tree/network
    // pollers, and the claude transcript tailer.
    let (raw_tx, raw_rx) = mpsc::sync_channel::<Event>(RAW_CHAN_CAP);

    // Bring eslogger up (prints/sudo-prompts to stderr) BEFORE we enter the
    // TUI's alternate screen further down.
    let (eslogger_handle, eslogger_active) = run::bring_up_eslogger(raw_tx.clone());

    // argv for the synthetic root Exec: split the ps command line so argv[0] is
    // the executable and its basename drives the tree-root comm.
    let argv: Vec<String> = {
        let parts: Vec<String> = command.split_whitespace().map(|s| s.to_string()).collect();
        if parts.is_empty() {
            vec!["claude".to_string()]
        } else {
            parts
        }
    };

    let session = Session::create(root, target_pid, tracer_pid, &argv, &cwd)?;
    eprintln!("ctrace · recording to {}", session.dir().display());

    let mut tree = PidTree::new(target_pid);
    tree.seed_descendants();
    let agg = Arc::new(Mutex::new(Aggregator::new(tree)));

    run::emit_synthetic_root_exec(&raw_tx, target_pid, 0, &argv);

    let persist = Arc::new(Persist::open(&session.events_path())?);
    let (net_handle, tree_poll_handle, transcript_handle) =
        run::start_poll_sources(target_pid, &cwd, eslogger_active, &agg, &raw_tx)?;
    let flush_handle = run::start_flush_thread(persist.clone(), FLUSH_INTERVAL);

    let (aggregator_handle, persist_handle) = run::spawn_pipeline(agg, persist, raw_rx);

    // Render the live session in the TUI until the user quits. claude keeps
    // running when we leave — we only detach.
    let entry = crate::view::discovery::entry_for_dir(session.dir())?;
    let render_result = crate::view::run::run_entry(entry, true);

    // Tear everything down once the TUI exits.
    run::shutdown_sources(eslogger_handle, net_handle, tree_poll_handle, transcript_handle);
    flush_handle.shutdown();
    drop(raw_tx);
    aggregator_handle.join().ok();
    persist_handle.join().ok();

    session.mark_status(SessionStatus::Done)?;
    eprintln!("ctrace · detached · session: {}", session.dir().display());

    render_result
}

/// Resolve which pid to attach to: an explicit pid (verified alive), the single
/// running claude, or an interactive picker when several are running.
fn select_pid(explicit: Option<u32>) -> Result<u32> {
    if let Some(p) = explicit {
        if !pid_alive(p) {
            return Err(anyhow!("pid {p} is not running"));
        }
        return Ok(p);
    }
    let found = find_claude_procs();
    match found.len() {
        0 => Err(anyhow!(
            "no running `claude` process found — start claude first, \
             or attach to a specific pid: `ctrace attach <pid>`"
        )),
        1 => Ok(found[0].0),
        _ => {
            let rows = enrich(found);
            let current_dir = std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            pick_process(rows, current_dir)?
                .ok_or_else(|| anyhow!("no process selected"))
        }
    }
}

/// One row in the attach picker. When several claude sessions share a `cwd`,
/// the session id and name are what tell them apart.
struct ProcRow {
    pid: u32,
    age: String,
    sid: String,
    name: String,
    cwd: String,
}

/// Decorate the bare `(pid, command)` candidates with cwd, uptime, session id,
/// and session name for display, in batched calls to lsof/ps.
fn enrich(procs: Vec<(u32, String)>) -> Vec<ProcRow> {
    let pids: Vec<u32> = procs.iter().map(|(p, _)| *p).collect();
    let cwds = cwds_for(&pids);
    let ages = ages_for(&pids);
    procs
        .into_iter()
        .map(|(pid, command)| {
            let elapsed = ages.get(&pid).and_then(|e| parse_etime(e));
            let started = elapsed.and_then(|d| SystemTime::now().checked_sub(d));
            let age = elapsed.map(human_age).unwrap_or_else(|| "?".into());
            let cwd_path = cwds.get(&pid);
            let sid = match (cwd_path, started) {
                (Some(cwd), Some(start)) => {
                    resolve_sid(cwd, &command, start).unwrap_or_else(|| "?".into())
                }
                _ => "?".into(),
            };
            // Read the session name (first user prompt / summary) from the
            // matched transcript, when we have a real session id.
            let name = match cwd_path {
                Some(cwd) if sid != "?" => transcript_path(cwd, &sid)
                    .and_then(|p| session_name(&p))
                    .unwrap_or_default(),
                _ => String::new(),
            };
            ProcRow {
                pid,
                age,
                sid,
                name,
                cwd: cwd_path
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "?".into()),
            }
        })
        .collect()
}

/// Path to a session's transcript: `<project-dir>/<sid>.jsonl`.
fn transcript_path(cwd: &Path, sid: &str) -> Option<PathBuf> {
    Some(claude_transcript::transcript_dir_for(cwd)?.join(format!("{sid}.jsonl")))
}

/// A human-readable name for a session: a `summary` line if present, otherwise
/// the first real user prompt. Only the head of the transcript is scanned, so
/// this stays cheap even on multi-megabyte files.
fn session_name(path: &Path) -> Option<String> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);
    for line in reader.lines().take(300).map_while(Result::ok) {
        let v: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        match v.get("type").and_then(|t| t.as_str()) {
            Some("summary") => {
                if let Some(s) = v.get("summary").and_then(|s| s.as_str()) {
                    return Some(clip(s, 60));
                }
            }
            Some("user") => {
                let content = v.get("message").and_then(|m| m.get("content"));
                let text = match content {
                    Some(serde_json::Value::String(s)) => Some(s.clone()),
                    Some(serde_json::Value::Array(arr)) => arr.iter().find_map(|p| {
                        (p.get("type").and_then(|t| t.as_str()) == Some("text"))
                            .then(|| p.get("text").and_then(|t| t.as_str()))
                            .flatten()
                            .map(|s| s.to_string())
                    }),
                    _ => None,
                };
                if let Some(t) = text {
                    let t = t.trim();
                    // Skip system/command wrappers like "<command-name>…".
                    if !t.is_empty() && !t.starts_with('<') {
                        return Some(clip(t, 60));
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// Truncate to `max` chars, appending `…` when shortened.
fn clip(s: &str, max: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= max {
        s
    } else {
        let kept: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{kept}…")
    }
}

/// Compact a duration for display: `9d`, `9d3h`, `3h12m`, `12m`, `45s`.
fn human_age(d: Duration) -> String {
    let s = d.as_secs();
    let (days, hours, mins) = (s / 86_400, (s % 86_400) / 3_600, (s % 3_600) / 60);
    if days > 0 {
        if hours > 0 {
            format!("{days}d{hours}h")
        } else {
            format!("{days}d")
        }
    } else if hours > 0 {
        format!("{hours}h{mins}m")
    } else if mins > 0 {
        format!("{mins}m")
    } else {
        format!("{s}s")
    }
}

/// How far a transcript's birth time may be from the process start time before
/// we stop trusting the match (and show `?` instead of a possibly-wrong id).
const SID_MATCH_WINDOW: Duration = Duration::from_secs(6 * 3600);

/// Resolve a running claude's session id. First honors an explicit
/// `--resume <id>` in argv; otherwise matches the project dir's transcript
/// whose birth time is closest to the process start (a fresh session writes its
/// `<id>.jsonl` shortly after launch). Returns `None` when nothing is close
/// enough to trust — better an honest `?` than a wrong id.
fn resolve_sid(cwd: &Path, command: &str, started: SystemTime) -> Option<String> {
    if let Some(id) = resume_id_from_command(command) {
        return Some(id);
    }
    let dir = claude_transcript::transcript_dir_for(cwd)?;
    let mut best: Option<(Duration, String)> = None;
    for entry in std::fs::read_dir(&dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let Some(birth) = entry.metadata().ok().and_then(|m| m.created().ok()) else {
            continue;
        };
        // Distance in either direction (birth is usually just after start).
        let diff = birth
            .duration_since(started)
            .or_else(|_| started.duration_since(birth))
            .unwrap_or(Duration::MAX);
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if best.as_ref().map(|(d, _)| diff < *d).unwrap_or(true) {
            best = Some((diff, stem.to_string()));
        }
    }
    best.and_then(|(d, stem)| (d <= SID_MATCH_WINDOW).then_some(stem))
}

/// Extract a session id from `--resume <id>` / `-r <id>` / `--resume=<id>`.
fn resume_id_from_command(command: &str) -> Option<String> {
    let toks: Vec<&str> = command.split_whitespace().collect();
    for (i, tok) in toks.iter().enumerate() {
        if let Some(rest) = tok.strip_prefix("--resume=") {
            if looks_like_uuid(rest) {
                return Some(rest.to_string());
            }
        }
        if *tok == "--resume" || *tok == "-r" {
            if let Some(next) = toks.get(i + 1) {
                if looks_like_uuid(next) {
                    return Some((*next).to_string());
                }
            }
        }
    }
    None
}

fn looks_like_uuid(s: &str) -> bool {
    s.len() == 36 && s.bytes().filter(|b| *b == b'-').count() == 4
}

/// Parse `ps` `etime` (`[[DD-]HH:]MM:SS`) into an elapsed duration.
fn parse_etime(s: &str) -> Option<Duration> {
    let (days, hms) = match s.split_once('-') {
        Some((d, rest)) => (d.trim().parse::<u64>().ok()?, rest),
        None => (0, s),
    };
    let parts: Vec<&str> = hms.split(':').collect();
    let (h, m, sec) = match parts.as_slice() {
        [h, m, s] => (
            h.parse::<u64>().ok()?,
            m.parse::<u64>().ok()?,
            s.parse::<u64>().ok()?,
        ),
        [m, s] => (0, m.parse::<u64>().ok()?, s.parse::<u64>().ok()?),
        _ => return None,
    };
    Some(Duration::from_secs(days * 86_400 + h * 3_600 + m * 60 + sec))
}

/// Interactive picker for choosing among several running claude processes.
/// Defaults to showing only claude(s) whose cwd matches `current_dir`; `a`
/// toggles to show every running claude. Returns the chosen pid, or `None` if
/// the user quit without selecting.
fn pick_process(rows: Vec<ProcRow>, current_dir: String) -> Result<Option<u32>> {
    use crossterm::event::{self, Event as CtEvent, KeyCode};
    use ratatui::backend::CrosstermBackend;
    use ratatui::layout::Rect;
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Block, Borders, List, ListItem, ListState};
    use ratatui::Terminal;
    use std::io::stdout;

    // Indices of rows whose cwd is the directory ctrace was launched from.
    let in_dir: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.cwd == current_dir)
        .map(|(i, _)| i)
        .collect();
    // Start filtered to this dir; if nothing matches, start showing all so the
    // list is never empty.
    let mut show_all = in_dir.is_empty();

    let _guard = crate::view::TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(stdout());
    let mut term = Terminal::new(backend)?;
    let mut state = ListState::default();
    state.select(Some(0));
    let mut picked: Option<u32> = None;

    loop {
        // Which underlying rows are visible right now.
        let visible: Vec<usize> = if show_all {
            (0..rows.len()).collect()
        } else {
            in_dir.clone()
        };
        // Keep the selection in range as the visible set changes.
        let sel = state.selected().unwrap_or(0).min(visible.len().saturating_sub(1));
        state.select(Some(sel));

        term.draw(|f| {
            let area = f.area();
            let inner = Rect {
                x: area.x + 1,
                y: area.y + 1,
                width: area.width.saturating_sub(2),
                height: area.height.saturating_sub(2),
            };
            const GUTTER: usize = 11; // "pid 1234567" / "up 1234567 "
            const NAME_COL: usize = 38; // value column where the name starts

            // Palette for the SELECTED card: vivid, bold pastels. Unselected
            // cards collapse to a single mid gray (clearly visible, but plainly
            // secondary) so the focused one stands out by hue + weight.
            let bold = Modifier::BOLD;
            let muted = Style::default().fg(Color::Rgb(0x90, 0x90, 0x90));
            let sel_label = Style::default().fg(Color::Rgb(0xc4, 0xc4, 0xc4));
            let sel_id = Style::default().fg(Color::Rgb(0xf2, 0xda, 0x8c)).add_modifier(bold); // gold
            let sel_name = Style::default().fg(Color::Rgb(0xcf, 0xe8, 0xb2)).add_modifier(bold); // sage
            let sel_path = Style::default().fg(Color::Rgb(0xae, 0xd6, 0xf5)).add_modifier(bold); // steel

            let items: Vec<ListItem> = visible
                .iter()
                .enumerate()
                .map(|(pos, &i)| {
                    let r = &rows[i];
                    let has_name = !r.name.is_empty();
                    let on = pos == sel;

                    // Pick the per-field style based on whether this card is
                    // selected; unselected -> everything muted gray.
                    let pid_s = if on {
                        Style::default().fg(Color::Rgb(0xff, 0xff, 0xff)).add_modifier(bold)
                    } else {
                        muted
                    };
                    let up_s = if on { sel_label } else { muted };
                    let sep_s = muted;
                    let lbl_s = if on { sel_label } else { muted };
                    let id_s = if on { sel_id } else { muted };
                    let name_s = if on { sel_name } else { muted };
                    let path_s = if on { sel_path } else { muted };
                    let sep = || Span::styled(" │ ", sep_s);

                    // Row 1 — labels: pid in the gutter, then field headers.
                    let mut l1 = vec![
                        Span::styled(format!("pid {:<width$}", r.pid, width = GUTTER - 4), pid_s),
                        sep(),
                        Span::styled("Session ID", lbl_s),
                    ];
                    if has_name {
                        l1.push(Span::raw(" ".repeat(NAME_COL.saturating_sub("Session ID".len()))));
                        l1.push(Span::styled("Session Name", lbl_s));
                    }

                    // Row 2 — values: uptime in the gutter, sid + name.
                    let mut l2 = vec![
                        Span::styled(format!("up {:<width$}", r.age, width = GUTTER - 3), up_s),
                        sep(),
                        Span::styled(r.sid.clone(), id_s),
                    ];
                    if has_name {
                        l2.push(Span::raw(
                            " ".repeat(NAME_COL.saturating_sub(r.sid.chars().count())),
                        ));
                        l2.push(Span::styled(r.name.clone(), name_s));
                    }

                    // Rows 3 & 4 — the path, labeled.
                    let l3 = Line::from(vec![
                        Span::raw(" ".repeat(GUTTER)),
                        sep(),
                        Span::styled("Path", lbl_s),
                    ]);
                    let l4 = Line::from(vec![
                        Span::raw(" ".repeat(GUTTER)),
                        sep(),
                        Span::styled(r.cwd.clone(), path_s),
                    ]);

                    ListItem::new(vec![
                        Line::from(l1),
                        Line::from(l2),
                        l3,
                        l4,
                        Line::from(""),
                    ])
                })
                .collect();
            let scope = if show_all {
                format!("all {} running — press a for this dir only", rows.len())
            } else {
                format!("{} in this dir — press a for all", visible.len())
            };
            let block = Block::default()
                .borders(Borders::ALL)
                .title(format!(" ctrace · attach to which claude?  [{scope}]  (↑/↓, Enter, q) "));
            let list = List::new(items)
                .block(block)
                .highlight_symbol("▸ ");
            f.render_stateful_widget(list, inner, &mut state);
        })?;

        if let CtEvent::Key(k) = event::read()? {
            match k.code {
                KeyCode::Char('q') | KeyCode::Esc => break,
                KeyCode::Char('a') => {
                    // Toggle scope. Don't collapse to an empty list.
                    if show_all || !in_dir.is_empty() {
                        show_all = !show_all;
                        state.select(Some(0));
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let i = state.selected().unwrap_or(0);
                    state.select(Some((i + 1).min(visible.len().saturating_sub(1))));
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    let i = state.selected().unwrap_or(0);
                    state.select(Some(i.saturating_sub(1)));
                }
                KeyCode::Enter => {
                    if let Some(&row_idx) = state.selected().and_then(|s| visible.get(s)) {
                        picked = Some(rows[row_idx].pid);
                    }
                    break;
                }
                _ => {}
            }
        }
    }

    Ok(picked)
}

/// Batched cwd lookup for several pids in a single `lsof` call.
fn cwds_for(pids: &[u32]) -> HashMap<u32, PathBuf> {
    let mut out = HashMap::new();
    if pids.is_empty() {
        return out;
    }
    let list = pids
        .iter()
        .map(|p| p.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let output = match Command::new("lsof")
        .args(["-a", "-p", &list, "-d", "cwd", "-Fpn"])
        .output()
    {
        Ok(o) => o,
        Err(_) => return out,
    };
    // -F output is one field per line: `p<pid>` starts a record, `n<path>` is
    // the cwd path for the current pid.
    let mut current: Option<u32> = None;
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if let Some(rest) = line.strip_prefix('p') {
            current = rest.trim().parse::<u32>().ok();
        } else if let Some(rest) = line.strip_prefix('n') {
            if let Some(pid) = current {
                out.entry(pid).or_insert_with(|| PathBuf::from(rest));
            }
        }
    }
    out
}

/// Batched uptime (`etime`) lookup for several pids in a single `ps` call.
fn ages_for(pids: &[u32]) -> HashMap<u32, String> {
    let mut out = HashMap::new();
    if pids.is_empty() {
        return out;
    }
    let list = pids
        .iter()
        .map(|p| p.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let output = match Command::new("/bin/ps")
        .args(["-o", "pid=,etime=", "-p", &list])
        .output()
    {
        Ok(o) => o,
        Err(_) => return out,
    };
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut parts = line.split_whitespace();
        if let (Some(p), Some(etime)) = (parts.next(), parts.next()) {
            if let Ok(pid) = p.parse::<u32>() {
                out.insert(pid, etime.to_string());
            }
        }
    }
    out
}

fn pid_alive(pid: u32) -> bool {
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    kill(Pid::from_raw(pid as i32), None).is_ok()
}

/// Find running processes whose argv[0] basename is `claude`. Returns
/// `(pid, full command line)`, excluding our own process.
fn find_claude_procs() -> Vec<(u32, String)> {
    let out = match Command::new("/bin/ps")
        .args(["-A", "-o", "pid=,command="])
        .output()
    {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };
    let me = std::process::id();
    let mut v = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let line = line.trim_start();
        let mut parts = line.splitn(2, char::is_whitespace);
        let pid = match parts.next().and_then(|s| s.trim().parse::<u32>().ok()) {
            Some(p) => p,
            None => continue,
        };
        if pid == me {
            continue;
        }
        let cmd = parts.next().unwrap_or("").trim();
        if cmd.is_empty() {
            continue;
        }
        let argv0 = cmd.split_whitespace().next().unwrap_or("");
        let bn = Path::new(argv0)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(argv0);
        if bn == "claude" {
            v.push((pid, cmd.to_string()));
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ps_etime_forms() {
        assert_eq!(parse_etime("45"), None); // ps always gives at least MM:SS
        assert_eq!(parse_etime("12:30"), Some(Duration::from_secs(12 * 60 + 30)));
        assert_eq!(parse_etime("01:23:45"), Some(Duration::from_secs(3600 + 23 * 60 + 45)));
        assert_eq!(
            parse_etime("09-00:16:52"),
            Some(Duration::from_secs(9 * 86_400 + 16 * 60 + 52))
        );
    }

    #[test]
    fn humanizes_durations() {
        assert_eq!(human_age(Duration::from_secs(9 * 86_400)), "9d");
        assert_eq!(human_age(Duration::from_secs(9 * 86_400 + 3 * 3600)), "9d3h");
        assert_eq!(human_age(Duration::from_secs(3 * 3600 + 12 * 60)), "3h12m");
        assert_eq!(human_age(Duration::from_secs(12 * 60)), "12m");
        assert_eq!(human_age(Duration::from_secs(45)), "45s");
    }

    #[test]
    fn extracts_resume_id_from_argv() {
        let id = "4466223c-3ecc-46c1-acea-8700460be36f";
        assert_eq!(resume_id_from_command(&format!("claude --resume {id}")).as_deref(), Some(id));
        assert_eq!(resume_id_from_command(&format!("claude -r {id}")).as_deref(), Some(id));
        assert_eq!(resume_id_from_command(&format!("claude --resume={id}")).as_deref(), Some(id));
        assert_eq!(resume_id_from_command("claude --resume"), None);
        assert_eq!(resume_id_from_command("claude -c"), None);
    }
}

fn proc_command(pid: u32) -> Option<String> {
    let out = Command::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Resolve a process's current working directory via `lsof`. Works without
/// sudo for the user's own processes.
fn proc_cwd(pid: u32) -> Option<PathBuf> {
    let out = Command::new("lsof")
        .args(["-a", "-p", &pid.to_string(), "-d", "cwd", "-Fn"])
        .output()
        .ok()?;
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if let Some(rest) = line.strip_prefix('n') {
            return Some(PathBuf::from(rest));
        }
    }
    None
}
