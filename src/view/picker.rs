use crate::view::discovery::SessionEntry;
use anyhow::Result;
use crossterm::event::{self, Event as CtEvent, KeyCode};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState};
use ratatui::Terminal;
use std::io::stdout;

pub fn pick(entries: Vec<SessionEntry>) -> Result<Option<SessionEntry>> {
    if entries.is_empty() { return Ok(None); }
    let _guard = crate::view::TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(stdout());
    let mut term = Terminal::new(backend)?;

    let mut state = ListState::default();
    state.select(Some(0));
    let mut picked: Option<usize> = None;

    loop {
        term.draw(|f| {
            let area = f.area();
            let inner = Rect { x: area.x+1, y: area.y+1, width: area.width-2, height: area.height-2 };
            let items: Vec<ListItem> = entries.iter().map(|e| {
                let glyph = if e.status == "live" { "● LIVE" } else { "○ done" };
                ListItem::new(Line::from(vec![
                    Span::raw(format!("{glyph}  ")),
                    Span::raw(format!("{}  ", e.meta.started_at.format("%Y-%m-%d %H:%M"))),
                    Span::raw(format!("{:<20}  ", e.meta.cwd.file_name().and_then(|s| s.to_str()).unwrap_or("?"))),
                    Span::raw(format!("pid {:<8}", e.meta.claude_pid)),
                    Span::styled(format!("  {}", e.meta.session_id), Style::default().add_modifier(Modifier::DIM)),
                ]))
            }).collect();
            let block = Block::default().borders(Borders::ALL).title(" peekaboo · select a session ");
            let list = List::new(items).block(block).highlight_style(
                Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD));
            f.render_stateful_widget(list, inner, &mut state);
        })?;
        if let CtEvent::Key(k) = event::read()? {
            match k.code {
                KeyCode::Char('q') | KeyCode::Esc => break,
                KeyCode::Down | KeyCode::Char('j') => {
                    let i = state.selected().unwrap_or(0);
                    state.select(Some((i + 1).min(entries.len() - 1)));
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    let i = state.selected().unwrap_or(0);
                    state.select(Some(i.saturating_sub(1)));
                }
                KeyCode::Enter => { picked = state.selected(); break; }
                _ => {}
            }
        }
    }

    Ok(picked.map(|i| entries[i].clone()))
}
