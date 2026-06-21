use crate::view::{discovery, picker, tail::Tail, ui::app::App};
use anyhow::{anyhow, Result};
use crossterm::event::{self, Event as CtEvent};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::stdout;
use std::path::{Path, PathBuf};
use std::time::Duration;

const INGEST_CHUNK: usize = 5000;

pub fn run(target: Option<String>, latest: bool, no_follow: bool, root: &Path) -> Result<()> {
    let entry = select_entry(target, latest, root)?;
    let follow = !no_follow && entry.status == "live";
    run_entry(entry, follow)
}

/// Render a specific session entry in the TUI. Shared by `view` (which selects
/// an entry first) and `attach` (which hands in a live session it just created,
/// with `follow = true`).
pub fn run_entry(entry: discovery::SessionEntry, follow: bool) -> Result<()> {
    let mut tail = Tail::open(&entry.events_path, follow)?;
    let mut app = App::new(entry);

    let _guard = crate::view::TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(stdout());
    let mut term = Terminal::new(backend)?;

    let mut initial_load_done = false;
    while !app.quit {
        if !initial_load_done {
            let batch = tail.drain_up_to(Duration::from_millis(0), INGEST_CHUNK)?;
            if batch.is_empty() {
                initial_load_done = true;
                continue;
            }
            for ev in batch {
                if !app.paused {
                    app.ingest(ev);
                }
            }
            term.draw(|f| app.draw(f))?;
            // Non-blocking key check so 'q' can abort a huge load.
            app.poll_input(Duration::from_millis(0))?;
            continue;
        }

        if follow {
            for ev in tail.drain_up_to(Duration::from_millis(0), INGEST_CHUNK)? {
                if !app.paused {
                    app.ingest(ev);
                }
            }
            term.draw(|f| app.draw(f))?;
            app.poll_input(Duration::from_millis(50))?;
        } else {
            // Replay: no more events will ever arrive. Block on input so Tab/q
            // respond instantly without burning CPU.
            term.draw(|f| app.draw(f))?;
            match event::read()? {
                CtEvent::Key(k) => app.handle_key(k),
                _ => {} // Resize / other -> fall through, loop redraws.
            }
        }
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
    resolve_entry(target, latest, root)
}

/// Resolve a target (session id / id-prefix) or `--latest` to a single session
/// entry, falling back to the picker when neither is given. Shared by `view`
/// (after its file-path special case) and `export`.
pub fn resolve_entry(target: Option<String>, latest: bool, root: &Path) -> Result<discovery::SessionEntry> {
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
        // entries is sorted live-first then by started_at desc, so entries[0]
        // is the most recent — live if any, otherwise the latest finished one.
        return Ok(entries[0].clone());
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
