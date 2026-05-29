use crate::view::{discovery, picker, tail::Tail, ui::app::App};
use anyhow::{anyhow, Result};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::stdout;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub fn run(target: Option<String>, latest: bool, no_follow: bool, root: &Path) -> Result<()> {
    let entry = select_entry(target, latest, root)?;
    let follow = !no_follow && entry.status == "live";
    let mut tail = Tail::open(&entry.events_path, follow)?;
    let mut app = App::new(entry);

    let _guard = crate::view::TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(stdout());
    let mut term = Terminal::new(backend)?;

    while !app.quit {
        // Drain new events.
        for ev in tail.drain(Duration::from_millis(50))? {
            if !app.paused { app.ingest(ev); }
        }
        term.draw(|f| app.draw(f))?;
        app.poll_input(Duration::from_millis(50))?;
    }

    Ok(())
}

fn select_entry(target: Option<String>, latest: bool, root: &Path) -> Result<crate::view::discovery::SessionEntry> {
    // 1. Explicit path: a file on disk.
    if let Some(t) = &target {
        let p = PathBuf::from(t);
        if p.is_file() {
            return synthesize_entry_for_path(&p);
        }
    }
    let entries = discovery::discover(root)?;
    if entries.is_empty() {
        return Err(anyhow!("no sessions found under {}", root.display()));
    }
    if let Some(t) = target {
        let matches: Vec<_> = entries.iter().filter(|e| e.meta.session_id.starts_with(&t)).cloned().collect();
        if matches.len() == 1 { return Ok(matches.into_iter().next().unwrap()); }
        if matches.is_empty() { return Err(anyhow!("no session id matches `{t}`")); }
        return Err(anyhow!("`{t}` is ambiguous: {} matches", matches.len()));
    }
    if latest {
        let live: Vec<_> = entries.iter().filter(|e| e.status == "live").collect();
        if let Some(e) = live.first() { return Ok((*e).clone()); }
        return Err(anyhow!("no live session to open with --latest"));
    }
    // Auto-pick rule from spec: 1 live -> open it; 1 total -> open it; else picker.
    let live: Vec<_> = entries.iter().filter(|e| e.status == "live").collect();
    if live.len() == 1 {
        let only_live = live[0].clone();
        return Ok(only_live);
    }
    if entries.len() == 1 {
        return Ok(entries[0].clone());
    }
    match picker::pick(entries)? {
        Some(e) => Ok(e),
        None => Err(anyhow!("no session selected")),
    }
}

fn synthesize_entry_for_path(_p: &Path) -> Result<crate::view::discovery::SessionEntry> {
    Err(anyhow!("opening raw JSONL files outside session dirs is not supported yet"))
}
