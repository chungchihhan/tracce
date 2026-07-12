use crate::flags::FlagConfig;
use crate::view::{discovery, picker, tail::Tail, ui::app::App};
use anyhow::{anyhow, Result};
use crossterm::event::{self, Event as CtEvent};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::stdout;
use std::path::{Path, PathBuf};
use std::time::Duration;

const INGEST_CHUNK: usize = 5000;

/// What the render loop ended with: the user quit outright, or asked (via `s`)
/// to go back to the session picker and view something else.
pub enum Outcome {
    Quit,
    Switch,
}

pub fn run(target: Option<String>, latest: bool, no_follow: bool, root: &Path) -> Result<()> {
    let mut entry = select_entry(target, latest, root)?;
    loop {
        let follow = !no_follow && entry.status == "live";
        match run_entry(entry, follow)? {
            Outcome::Quit => return Ok(()),
            Outcome::Switch => {
                match picker::pick(discovery::discover(root)?)? {
                    Some(e) => entry = e,
                    None => return Ok(()),
                }
            }
        }
    }
}

/// Render a specific session entry in the TUI. Shared by `view` (which selects
/// an entry first) and `attach` (which hands in a live session it just created,
/// with `follow = true`).
pub fn run_entry(entry: discovery::SessionEntry, follow: bool) -> Result<Outcome> {
    let mut tail = Tail::open(&entry.events_path, follow)?;
    let mut app = App::new(entry, FlagConfig::empty());

    let _guard = crate::view::TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(stdout());
    let mut term = Terminal::new(backend)?;

    let mut initial_load_done = false;
    while !app.quit && !app.switch {
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

    Ok(if app.switch { Outcome::Switch } else { Outcome::Quit })
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
///
/// No auto-pick shortcuts: a bare `view`/`export` with no target and no
/// `--latest` always opens the picker, even when there's only one session or
/// exactly one live one — the user asked for the selection page every time, so
/// they always get a chance to pick a different (e.g. finished) session instead.
pub fn resolve_entry(target: Option<String>, latest: bool, root: &Path) -> Result<discovery::SessionEntry> {
    let entries = discovery::discover(root)?;
    if entries.is_empty() {
        return Err(anyhow!("no sessions found under {}", root.display()));
    }
    if let Some(t) = target {
        let matches: Vec<_> = entries.iter().filter(|e| e.meta.session_id.starts_with(&t)).cloned().collect();
        if matches.len() == 1 { return Ok(matches.into_iter().next().unwrap()); }
        if matches.is_empty() {
            // A file-path target is a common mistake here (e.g. `export ./meta.json`):
            // this resolver only matches session ids, so say so rather than the
            // bare "no session id matches".
            if Path::new(&t).is_file() {
                return Err(anyhow!(
                    "`{t}` is a file, not a session id — pass a session id or id-prefix"
                ));
            }
            return Err(anyhow!("no session id matches `{t}`"));
        }
        return Err(anyhow!("`{t}` is ambiguous: {} matches", matches.len()));
    }
    if latest {
        // Purely by recency (started_at), live or not — unlike the picker's
        // display order, `--latest` never favors a live session over a more
        // recently-started finished one.
        let newest = entries.iter().max_by_key(|e| e.meta.started_at).unwrap().clone();
        return Ok(newest);
    }
    match picker::pick(entries)? {
        Some(e) => Ok(e),
        None => Err(anyhow!("no session selected")),
    }
}

fn synthesize_entry_for_path(_p: &Path) -> Result<crate::view::discovery::SessionEntry> {
    Err(anyhow!("opening raw JSONL files outside session dirs is not supported yet"))
}
