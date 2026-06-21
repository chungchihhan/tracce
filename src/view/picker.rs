use crate::view::discovery::SessionEntry;
use crate::view::ui::app::{key_cap, logo_lines};
use anyhow::Result;
use chrono::Utc;
use crossterm::event::{self, Event as CtEvent, KeyCode};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, List, ListItem, ListState, Padding, Paragraph};
use ratatui::{Frame, Terminal};
use std::io::stdout;
use std::path::Path;

pub fn pick(entries: Vec<SessionEntry>) -> Result<Option<SessionEntry>> {
    if entries.is_empty() { return Ok(None); }
    let _guard = crate::view::TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(stdout());
    let mut term = Terminal::new(backend)?;

    // Count events once per session up front; redraws then stay cheap.
    let counts: Vec<usize> = entries.iter()
        .map(|e| count_lines(&e.events_path).unwrap_or(0))
        .collect();

    let n = entries.len();
    let mut state = ListState::default();
    state.select(Some(0));
    let mut picked: Option<usize> = None;
    // Result of the most recent `e` export, shown in the footer until the next key.
    let mut flash: Option<String> = None;

    loop {
        term.draw(|f| draw(f, &entries, &counts, &mut state, flash.as_deref()))?;
        let ev = event::read()?;
        // Any event (key, resize, …) dismisses a stale export flash so it never
        // lingers on screen waiting specifically for a keypress.
        flash = None;
        if let CtEvent::Key(k) = ev {
            let cur = state.selected().unwrap_or(0);
            match k.code {
                KeyCode::Char('q') | KeyCode::Esc => break,
                KeyCode::Down | KeyCode::Char('j') => state.select(Some((cur + 1).min(n - 1))),
                KeyCode::Up | KeyCode::Char('k') => state.select(Some(cur.saturating_sub(1))),
                KeyCode::PageDown => state.select(Some((cur + 10).min(n - 1))),
                KeyCode::PageUp => state.select(Some(cur.saturating_sub(10))),
                KeyCode::Home | KeyCode::Char('g') => state.select(Some(0)),
                KeyCode::End | KeyCode::Char('G') => state.select(Some(n - 1)),
                KeyCode::Char('e') => {
                    let entry = &entries[cur];
                    let out = std::path::PathBuf::from(
                        format!("{}.tracce.tgz", entry.meta.session_id),
                    );
                    flash = Some(match crate::bundle::export(entry, &out) {
                        Ok(n) => format!("exported -> {} ({} bytes)", out.display(), n),
                        Err(e) => format!("export failed: {e:#}"),
                    });
                }
                KeyCode::Enter => { picked = state.selected(); break; }
                _ => {}
            }
        }
    }

    Ok(picked.map(|i| entries[i].clone()))
}

fn draw(f: &mut Frame, entries: &[SessionEntry], counts: &[usize], state: &mut ListState, flash: Option<&str>) {
    let area = f.area();

    // Outer frame: rounded cyan border, matching the dashboard chrome.
    let title = Line::from(vec![
        Span::styled(" tracce ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Span::styled(
            format!("· {} session{} ", entries.len(), if entries.len() == 1 { "" } else { "s" }),
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    let outer = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Cyan))
        .title(title)
        .padding(Padding::new(1, 1, 0, 0));
    let inner = outer.inner(area);
    f.render_widget(outer, area);

    let sel = state.selected().unwrap_or(0).min(entries.len().saturating_sub(1));
    let show_logo = inner.height >= 16 && inner.width >= 50;

    // Session-table column widths (left pane). PROJECT flexes to fill the rest of
    // the pane, so the selection highlight spans the whole column.
    const W_STATUS: usize = 8;
    const W_START: usize = 17;
    const W_DUR: usize = 7;
    const W_EVENTS: usize = 9;
    const W_GAP: usize = 2;
    let fixed = W_STATUS + W_START + W_DUR + W_EVENTS + W_GAP;

    // Vertical: [logo] · body · footer.
    let mut constraints: Vec<Constraint> = Vec::new();
    if show_logo { constraints.push(Constraint::Length(7)); } // 6 logo rows + gap
    constraints.push(Constraint::Min(3)); // body (table | detail)
    constraints.push(Constraint::Length(1)); // footer
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);

    let mut ri = 0;
    if show_logo {
        let mut lines = logo_lines();
        lines.push(Line::raw(""));
        f.render_widget(Paragraph::new(lines), rows[ri]);
        ri += 1;
    }
    let body = rows[ri];
    ri += 1;
    let footer_rect = rows[ri];

    // Body: split the width in half — session table on the LEFT, detail of the
    // selected session on the RIGHT. On terminals too narrow for a useful split,
    // drop the detail and size the table to its content.
    let (table_area, proj_w, detail_area) = if body.width as usize >= 2 * (fixed + 8) {
        let h = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(body);
        let pw = (h[0].width as usize).saturating_sub(fixed); // PROJECT fills the half
        (h[0], pw, Some(h[1]))
    } else {
        let pw = entries.iter()
            .map(|e| project_name(e).chars().count())
            .max().unwrap_or(0)
            .max("PROJECT".len())
            .min(30);
        let tw = ((fixed + pw) as u16).min(body.width);
        let h = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(tw), Constraint::Min(0)])
            .split(body);
        (h[0], pw, None)
    };

    // Left column: column header + scrolling list, left-anchored.
    let lv = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(table_area);
    let header_style = Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD);
    f.render_widget(
        Paragraph::new(Line::styled(
            format!(
                "{:<W_STATUS$}{:<W_START$}{:>W_DUR$}{:>W_EVENTS$}{:W_GAP$}{:<proj_w$}",
                "STATUS", "STARTED", "DUR", "EVENTS", "", "PROJECT",
            ),
            header_style,
        )),
        lv[0],
    );
    let items: Vec<ListItem> = entries.iter().zip(counts).map(|(e, &cnt)| {
        let (glyph, gstyle) = status_badge(&e.status);
        ListItem::new(Line::from(vec![
            Span::styled(format!("{glyph:<W_STATUS$}"), gstyle),
            Span::styled(format!("{:<W_START$}", e.meta.started_at.format("%Y-%m-%d %H:%M")), gray()),
            Span::styled(format!("{:>W_DUR$}", duration_str(e)), gray()),
            Span::styled(format!("{:>W_EVENTS$}", with_commas(cnt)), gray()),
            Span::raw(" ".repeat(W_GAP)),
            Span::styled(format!("{:<proj_w$}", trunc(&project_name(e), proj_w)), Style::default().fg(Color::White)),
        ]))
    }).collect();
    let list = List::new(items).highlight_style(
        Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD),
    );
    f.render_stateful_widget(list, lv[1], state);

    // Right column: detail of the selected session, behind a left divider line.
    if let Some(area) = detail_area {
        let e = &entries[sel];
        let block = Block::default()
            .borders(Borders::LEFT)
            .border_style(Style::default().fg(Color::DarkGray))
            .padding(Padding::new(2, 0, 0, 0));
        let dinner = block.inner(area);
        f.render_widget(block, area);
        let dv = (dinner.width as usize).saturating_sub(9).max(1); // value width after the label

        let (glyph, gstyle) = status_badge(&e.status);
        let cmd = if e.meta.argv.is_empty() { "claude".to_string() } else { e.meta.argv.join(" ") };
        let kv = |k: &str, v: Span<'static>| {
            Line::from(vec![Span::styled(format!("{k:<9}"), Style::default().fg(Color::DarkGray)), v])
        };
        let lines = vec![
            Line::from(Span::styled("SELECTED SESSION", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))),
            Line::raw(""),
            kv("status", Span::styled(glyph.to_string(), gstyle)),
            kv("started", Span::styled(e.meta.started_at.format("%Y-%m-%d %H:%M:%S").to_string(), gray())),
            kv("duration", Span::styled(duration_str(e), gray())),
            kv("events", Span::styled(with_commas(counts[sel]), gray())),
            Line::raw(""),
            // Keep the tail of the path (the project) rather than the /Users prefix.
            kv("cwd", Span::styled(front_trunc(&e.meta.cwd.display().to_string(), dv), gray())),
            kv("command", Span::styled(trunc(&cmd, dv), gray())),
            kv("session", Span::styled(trunc(&e.meta.session_id, dv), gray())),
            kv("pid", Span::styled(e.meta.claude_pid.to_string(), gray())),
            kv("host", Span::styled(e.meta.hostname.clone(), gray())),
        ];
        f.render_widget(Paragraph::new(lines), dinner);
    }

    // Footer: the latest export result if any, else the centered key hints.
    let footer = if let Some(msg) = flash {
        Line::from(Span::styled(msg.to_string(), Style::default().fg(Color::Cyan)))
    } else {
        Line::from(vec![
            key_cap("↑/↓"), Span::styled(" move   ", gray()),
            key_cap("Enter"), Span::styled(" open   ", gray()),
            key_cap("e"), Span::styled(" export   ", gray()),
            key_cap("q"), Span::styled(" cancel", gray()),
        ])
    };
    f.render_widget(Paragraph::new(footer).alignment(Alignment::Center), footer_rect);
}

fn gray() -> Style { Style::default().fg(Color::Gray) }

/// Truncate to at most `n` chars, marking elision with a trailing `…`.
fn trunc(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= n { return s.to_string(); }
    if n == 0 { return String::new(); }
    let head: String = chars[..n - 1].iter().collect();
    format!("{head}…")
}

/// Truncate from the front, keeping the last `n` chars behind a leading `…`.
/// Used for paths, where the tail (the project dir) matters more than the prefix.
fn front_trunc(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= n { return s.to_string(); }
    if n == 0 { return String::new(); }
    let tail: String = chars[chars.len() - (n - 1)..].iter().collect();
    format!("…{tail}")
}

/// Colored badge for a session's run state.
fn status_badge(status: &str) -> (&'static str, Style) {
    match status {
        "live" => ("● live", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
        "crashed" => ("✗ crash", Style::default().fg(Color::Red)),
        _ => ("○ done", Style::default().fg(Color::DarkGray)),
    }
}

fn project_name(e: &SessionEntry) -> String {
    e.meta.cwd.file_name().and_then(|s| s.to_str()).unwrap_or("?").to_string()
}

/// Wall-clock span of the session. Live/crashed sessions with no recorded end
/// are measured to "now".
fn duration_str(e: &SessionEntry) -> String {
    let end = e.meta.ended_at.unwrap_or_else(Utc::now);
    let secs = (end - e.meta.started_at).num_seconds().max(0);
    if secs >= 3600 { format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60) }
    else if secs >= 60 { format!("{}m{:02}s", secs / 60, secs % 60) }
    else { format!("{secs}s") }
}

/// Group an integer with thousands separators, e.g. `12345` → `12,345`.
fn with_commas(n: usize) -> String {
    let s = n.to_string();
    let len = s.len();
    let mut out = String::with_capacity(len + len / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (len - i) % 3 == 0 { out.push(','); }
        out.push(ch);
    }
    out
}

fn count_lines(p: &Path) -> std::io::Result<usize> {
    use std::io::{BufRead, BufReader};
    let f = std::fs::File::open(p)?;
    Ok(BufReader::new(f).lines().count())
}





