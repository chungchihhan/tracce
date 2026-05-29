use crate::event::Event;
use crate::view::discovery::SessionEntry;
use crossterm::event::{self, Event as CtEvent, KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::Frame;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane { Process, File, Network }

pub struct App {
    pub session: SessionEntry,
    pub events: Vec<Event>,
    pub focus: Pane,
    pub paused: bool,
    pub dropped: usize,
    pub quit: bool,
    process_scroll: usize,
    file_scroll: usize,
    network_scroll: usize,

    // derived state
    pub processes: HashMap<u32, ProcInfo>,
    pub recent_files: Vec<FileRow>,
    pub network: HashMap<String, NetRow>,
}

pub struct ProcInfo { pub pid: u32, pub comm: String, pub ppid: u32, pub event_count: usize }
pub struct FileRow { pub op: char, pub path: PathBuf, pub sensitive: bool, pub coalesced: bool }
pub struct NetRow { pub host: String, pub conns: usize }

impl App {
    pub fn new(session: SessionEntry) -> Self {
        Self {
            session,
            events: Vec::new(),
            focus: Pane::Process,
            paused: false,
            dropped: 0,
            quit: false,
            process_scroll: 0,
            file_scroll: 0,
            network_scroll: 0,
            processes: HashMap::new(),
            recent_files: Vec::new(),
            network: HashMap::new(),
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        match (key.code, key.modifiers) {
            (KeyCode::Char('q'), _) => self.quit = true,
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => self.quit = true,
            (KeyCode::Tab, _) => {
                self.focus = match self.focus { Pane::Process => Pane::File, Pane::File => Pane::Network, Pane::Network => Pane::Process };
            }
            (KeyCode::Char('p'), _) => self.paused = !self.paused,
            (KeyCode::Char('j'), _) | (KeyCode::Down, _) => self.scroll(1),
            (KeyCode::Char('k'), _) | (KeyCode::Up, _)   => self.scroll(-1),
            _ => {}
        }
    }

    fn scroll(&mut self, delta: i64) {
        let s = match self.focus {
            Pane::Process => &mut self.process_scroll,
            Pane::File => &mut self.file_scroll,
            Pane::Network => &mut self.network_scroll,
        };
        *s = ((*s as i64 + delta).max(0)) as usize;
    }

    pub fn ingest(&mut self, ev: Event) {
        use crate::event::EventData::*;
        match &ev.data {
            Exec { argv, .. } => {
                let info = self.processes.entry(ev.pid).or_insert(ProcInfo {
                    pid: ev.pid, comm: argv.first().cloned().unwrap_or_default(),
                    ppid: ev.ppid, event_count: 0,
                });
                info.event_count += 1;
            }
            Fork { child_pid } => {
                self.processes.entry(*child_pid).or_insert(ProcInfo {
                    pid: *child_pid, comm: "(forked)".into(), ppid: ev.pid, event_count: 0,
                });
            }
            Exit { .. } => {}
            File { op, path, .. } => {
                self.recent_files.insert(0, FileRow {
                    op: match op { crate::event::FileOp::Open => 'R', crate::event::FileOp::Write => 'W', crate::event::FileOp::Create => 'C', crate::event::FileOp::Close => 'X' },
                    path: path.clone(),
                    sensitive: ev.flags & crate::event::FLAG_SENSITIVE != 0,
                    coalesced: ev.flags & crate::event::FLAG_COALESCED != 0,
                });
                if self.recent_files.len() > 200 { self.recent_files.truncate(200); }
            }
            NetOpen { host, remote, .. } => {
                let key = host.clone().unwrap_or_else(|| remote.ip().to_string());
                self.network.entry(key.clone()).or_insert(NetRow { host: key, conns: 0 }).conns += 1;
            }
            NetClose { .. } => {}
        }
        self.events.push(ev);
    }

    pub fn poll_input(&mut self, tick: Duration) -> std::io::Result<()> {
        if event::poll(tick)? {
            if let CtEvent::Key(k) = event::read()? {
                self.handle_key(k);
            }
        }
        Ok(())
    }

    pub fn draw(&self, f: &mut Frame) {
        let area = f.area();
        let v = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(2), Constraint::Min(3)])
            .split(area);
        self.draw_status(f, v[0]);
        let h = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(34), Constraint::Percentage(33), Constraint::Percentage(33)])
            .split(v[1]);
        self.draw_processes(f, h[0]);
        self.draw_files(f, h[1]);
        self.draw_network(f, h[2]);
    }

    fn draw_status(&self, f: &mut Frame, area: Rect) {
        let id = &self.session.meta.session_id;
        let live = if self.session.status == "live" { "● LIVE" } else { "○ replay" };
        let pause = if self.paused { "  [paused]" } else { "" };
        let line = Line::from(vec![
            Span::styled(format!("peekaboo · {live} · {id}{pause}"), Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(format!("   events {} · dropped {}   [q]uit [Tab] cycle [p] pause", self.events.len(), self.dropped)),
        ]);
        f.render_widget(Paragraph::new(line), area);
    }

    fn draw_processes(&self, f: &mut Frame, area: Rect) {
        let mut procs: Vec<&ProcInfo> = self.processes.values().collect();
        procs.sort_by(|a, b| a.comm.cmp(&b.comm));
        let items: Vec<ListItem> = procs.into_iter()
            .map(|p| ListItem::new(format!("{:5}  {:<20}  ev:{}", p.pid, truncate(&p.comm, 20), p.event_count)))
            .collect();
        let block = Block::default().borders(Borders::ALL).title(self.title("PROCESS TREE", Pane::Process));
        f.render_widget(List::new(items).block(block), area);
    }

    fn draw_files(&self, f: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self.recent_files.iter().take(area.height as usize)
            .map(|r| {
                let glyph = if r.sensitive { "⚠ " } else { "  " };
                let suffix = if r.coalesced { " (burst)" } else { "" };
                let style = if r.sensitive {
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
                } else { Style::default() };
                ListItem::new(Line::from(vec![
                    Span::raw(format!("{} {} ", r.op, glyph)),
                    Span::styled(format!("{}{}", truncate(&r.path.display().to_string(), area.width as usize - 8), suffix), style),
                ]))
            }).collect();
        let block = Block::default().borders(Borders::ALL).title(self.title("FILE I/O", Pane::File));
        f.render_widget(List::new(items).block(block), area);
    }

    fn draw_network(&self, f: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self.network.values()
            .map(|n| ListItem::new(format!("{:<28}  conns:{}", truncate(&n.host, 28), n.conns)))
            .collect();
        let block = Block::default().borders(Borders::ALL).title(self.title("NETWORK", Pane::Network));
        f.render_widget(List::new(items).block(block), area);
    }

    fn title(&self, name: &str, pane: Pane) -> Line<'_> {
        let focused = self.focus == pane;
        Line::from(Span::styled(
            format!(" {} {} ", if focused { "▌" } else { " " }, name),
            if focused {
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
            } else { Style::default() }
        ))
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n { s.to_string() } else {
        let mut t = s.chars().rev().take(n.saturating_sub(1)).collect::<String>();
        t = t.chars().rev().collect();
        format!("…{t}")
    }
}
