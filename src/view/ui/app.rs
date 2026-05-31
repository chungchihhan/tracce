use crate::event::Event;
use crate::view::discovery::SessionEntry;
use crossterm::event::{self, Event as CtEvent, KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, List, ListItem, Paragraph, Sparkline};
use ratatui::Frame;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::time::Duration;

const RATE_BUCKET_NS: u64 = 1_000_000_000;
const RATE_HISTORY_LEN: usize = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane { Process, File, Commands, Network }

pub struct App {
    pub session: SessionEntry,
    pub events: Vec<Event>,
    pub focus: Pane,
    pub paused: bool,
    pub dropped: usize,
    pub quit: bool,
    process_scroll: usize,
    file_scroll: usize,
    commands_scroll: usize,
    network_scroll: usize,

    // derived state
    pub processes: HashMap<u32, ProcInfo>,
    pub recent_files: Vec<FileRow>,
    pub commands: Vec<CommandRow>,
    pub network: HashMap<String, NetRow>,

    // header stats
    pub sensitive_count: usize,
    rate_history: VecDeque<u64>,
    current_bucket: u64,
    bucket_anchor_ns: u64,
    first_event_ns: Option<u64>,
    last_event_ns: u64,
}

pub struct ProcInfo { pub pid: u32, pub comm: String, pub ppid: u32, pub event_count: usize }
pub struct FileRow { pub pid: u32, pub comm: String, pub op: char, pub path: PathBuf, pub sensitive: bool, pub coalesced: bool }
pub struct CommandRow { pub pid: u32, pub argv: String }
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
            commands_scroll: 0,
            network_scroll: 0,
            processes: HashMap::new(),
            recent_files: Vec::new(),
            commands: Vec::new(),
            network: HashMap::new(),
            sensitive_count: 0,
            rate_history: VecDeque::with_capacity(RATE_HISTORY_LEN),
            current_bucket: 0,
            bucket_anchor_ns: 0,
            first_event_ns: None,
            last_event_ns: 0,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        match (key.code, key.modifiers) {
            (KeyCode::Char('q'), _) => self.quit = true,
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => self.quit = true,
            (KeyCode::Tab, _) => {
                self.focus = match self.focus {
                    Pane::Process => Pane::File,
                    Pane::File => Pane::Commands,
                    Pane::Commands => Pane::Network,
                    Pane::Network => Pane::Process,
                };
            }
            (KeyCode::Char('p'), _) => self.paused = !self.paused,
            (KeyCode::Char('j'), _) | (KeyCode::Down, _)  => self.scroll(1),
            (KeyCode::Char('k'), _) | (KeyCode::Up, _)    => self.scroll(-1),
            (KeyCode::PageDown, _)                        => self.scroll(10),
            (KeyCode::PageUp, _)                          => self.scroll(-10),
            (KeyCode::Char('g'), _) | (KeyCode::Home, _)  => self.scroll_to(0),
            (KeyCode::Char('G'), _) | (KeyCode::End, _)   => self.scroll_to(usize::MAX),
            _ => {}
        }
    }

    fn focus_len(&self) -> usize {
        match self.focus {
            Pane::Process => self.processes.len(),
            Pane::File => self.recent_files.len(),
            Pane::Commands => self.commands.len(),
            Pane::Network => self.network.len(),
        }
    }

    fn scroll(&mut self, delta: i64) {
        let max = self.focus_len();
        let s = match self.focus {
            Pane::Process => &mut self.process_scroll,
            Pane::File => &mut self.file_scroll,
            Pane::Commands => &mut self.commands_scroll,
            Pane::Network => &mut self.network_scroll,
        };
        let next = (*s as i64 + delta).max(0) as usize;
        *s = next.min(max.saturating_sub(1));
    }

    fn scroll_to(&mut self, pos: usize) {
        let max = self.focus_len();
        let s = match self.focus {
            Pane::Process => &mut self.process_scroll,
            Pane::File => &mut self.file_scroll,
            Pane::Commands => &mut self.commands_scroll,
            Pane::Network => &mut self.network_scroll,
        };
        *s = pos.min(max.saturating_sub(1));
    }

    pub fn ingest(&mut self, ev: Event) {
        use crate::event::EventData::*;

        // Update rate history. Buckets are keyed off event timestamps so the
        // sparkline matches the recording even in replay mode.
        if self.first_event_ns.is_none() {
            self.first_event_ns = Some(ev.ts_ns);
            self.bucket_anchor_ns = ev.ts_ns;
        }
        self.last_event_ns = ev.ts_ns;
        while ev.ts_ns >= self.bucket_anchor_ns.saturating_add(RATE_BUCKET_NS) {
            self.rate_history.push_back(self.current_bucket);
            if self.rate_history.len() > RATE_HISTORY_LEN { self.rate_history.pop_front(); }
            self.current_bucket = 0;
            self.bucket_anchor_ns = self.bucket_anchor_ns.saturating_add(RATE_BUCKET_NS);
        }
        self.current_bucket += 1;
        if ev.flags & crate::event::FLAG_SENSITIVE != 0 {
            self.sensitive_count += 1;
        }

        // Every event counts against some pid. Fork creates the *child*, so it
        // should be attributed to child_pid; everything else to ev.pid.
        let (target_pid, target_ppid, default_comm) = match &ev.data {
            Fork { child_pid } => (*child_pid, ev.pid, "(forked)".to_string()),
            _ => (ev.pid, ev.ppid, ev.process.comm.clone()),
        };
        {
            let info = self.processes.entry(target_pid).or_insert(ProcInfo {
                pid: target_pid,
                comm: default_comm,
                ppid: target_ppid,
                event_count: 0,
            });
            info.event_count += 1;
            if let Exec { argv, .. } = &ev.data {
                if let Some(c) = argv.first() {
                    info.comm = c.clone();
                }
            }
        }

        match &ev.data {
            Exec { argv, .. } => {
                // Each Exec becomes a row in the COMMANDS pane. argv may be
                // a single-element vec (poll-mode synthetic Exec uses the full
                // basenamed command as argv[0]) or a real argv array (eslogger).
                // Joining with spaces works for both shapes.
                let joined = argv.join(" ");
                if !joined.is_empty() {
                    self.commands.insert(0, CommandRow { pid: ev.pid, argv: joined });
                    if self.commands.len() > 500 { self.commands.truncate(500); }
                }
            }
            Fork { .. } | Exit { .. } => {}
            File { op, path, .. } => {
                let comm = self.processes.get(&ev.pid)
                    .map(|p| p.comm.clone())
                    .unwrap_or_else(|| ev.process.comm.clone());
                self.recent_files.insert(0, FileRow {
                    pid: ev.pid,
                    comm,
                    op: match op {
                        crate::event::FileOp::Open => 'R',
                        crate::event::FileOp::Write => 'W',
                        crate::event::FileOp::Create => 'C',
                        crate::event::FileOp::Close => 'X',
                        crate::event::FileOp::Delete => 'D',
                        crate::event::FileOp::Rename => 'M',  // move
                        crate::event::FileOp::Edit => 'E',
                        crate::event::FileOp::MultiEdit => 'A', // mAny edits
                        crate::event::FileOp::Bash => '$',
                    },
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
            .constraints([
                Constraint::Length(4),  // header (1 status + 3 sparkline)
                Constraint::Min(3),     // main
                Constraint::Length(1),  // footer
            ])
            .split(area);
        self.draw_header(f, v[0]);
        let h = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(v[1]);
        self.draw_processes(f, h[0]);
        let right = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Percentage(34),
                Constraint::Percentage(33),
                Constraint::Percentage(33),
            ])
            .split(h[1]);
        self.draw_files(f, right[0]);
        self.draw_commands(f, right[1]);
        self.draw_network(f, right[2]);
        self.draw_footer(f, v[2]);
    }

    fn draw_header(&self, f: &mut Frame, area: Rect) {
        let v = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(1)])  // status, sparkline-area
            .split(area);

        // Row 1: status line.
        let id = &self.session.meta.session_id;
        let live_span = if self.session.status == "live" {
            Span::styled("● LIVE", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD))
        } else {
            Span::styled("○ replay", Style::default().fg(Color::DarkGray))
        };
        let pause_span = if self.paused {
            Span::styled(" [paused]", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
        } else {
            Span::raw("")
        };
        let sens_style = if self.sensitive_count > 0 {
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let drop_style = if self.dropped > 0 {
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let status = Line::from(vec![
            Span::styled("ctrace ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::raw("· "), live_span,
            Span::raw(format!(" · {id} · uptime {}", self.uptime_str())),
            pause_span,
            Span::raw("    events "),
            Span::styled(format!("{}", self.events.len()), Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" · sensitive "),
            Span::styled(format!("{}", self.sensitive_count), sens_style),
            Span::raw(" · dropped "),
            Span::styled(format!("{}", self.dropped), drop_style),
        ]);
        f.render_widget(Paragraph::new(status), v[0]);

        // Row 2: sparkline of events-per-second over the last minute.
        let h = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(16), Constraint::Min(10)])
            .split(v[1]);
        let label = Line::from(vec![
            Span::styled("EVENTS/s ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::styled(format!("{:>5}", self.current_rate()), Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" "),
        ]);
        f.render_widget(Paragraph::new(label), h[0]);
        let data: Vec<u64> = self.rate_history.iter().copied().collect();
        let spark = Sparkline::default()
            .data(data)
            .style(Style::default().fg(Color::Green));
        f.render_widget(spark, h[1]);
    }

    fn current_rate(&self) -> u64 {
        *self.rate_history.back().unwrap_or(&self.current_bucket)
    }

    fn uptime_str(&self) -> String {
        let span_ns = match self.first_event_ns {
            Some(start) => self.last_event_ns.saturating_sub(start),
            None => 0,
        };
        let secs = span_ns / 1_000_000_000;
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        let s = secs % 60;
        if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
    }

    fn draw_processes(&self, f: &mut Frame, area: Rect) {
        // Per-row layout: "PPPPP  <prefix><comm padded to fill>  EEEEEE"
        //                    5    2  variable      to flush      2  6
        const PID_W: usize = 5;
        const SEP: usize = 2;
        const EV_W: usize = 6;
        let total_w = area.width as usize;
        // Reserve room for the static columns; remainder is the comm budget.
        // We compute comm_at_depth_zero so the column header aligns with the
        // widest available comm — deeper rows just borrow from the comm budget.
        let base_comm_w = total_w
            .saturating_sub(PID_W + SEP + SEP + EV_W + 2 /* borders */)
            .max(8);

        let header = format!(
            "{:>w_pid$}  {:<w_comm$}  {:>w_ev$}",
            "PID", "COMMAND", "EV",
            w_pid = PID_W, w_comm = base_comm_w, w_ev = EV_W,
        );
        let (_inner, body) = self.framed_pane(f, area, "PROCESS TREE", Pane::Process, header);

        let rows = build_tree_rows(&self.processes);
        let items: Vec<ListItem> = rows.iter()
            .skip(self.process_scroll)
            .take(body.height as usize)
            .map(|(pid, prefix)| {
                let p = &self.processes[pid];
                let prefix_cols = prefix.chars().count();
                let comm_w = base_comm_w.saturating_sub(prefix_cols).max(4);
                ListItem::new(format!(
                    "{:>w_pid$}  {}{:<w_comm$}  {:>w_ev$}",
                    p.pid, prefix, truncate(&p.comm, comm_w), p.event_count,
                    w_pid = PID_W, w_comm = comm_w, w_ev = EV_W,
                ))
            }).collect();
        f.render_widget(List::new(items), body);
    }

    fn draw_files(&self, f: &mut Frame, area: Rect) {
        let (_inner, body) = self.framed_pane(f, area, "ACTIVITY", Pane::File,
            format!("    {:>5} {:<8} {}", "PID", "COMM", "PATH / CMD"));
        // Prefix is "X G PPPPP CCCCCCCC " = 1+1+1+1+5+1+8+1 = 19 cols
        const PREFIX_COLS: usize = 19;
        let path_cols = (body.width as usize).saturating_sub(PREFIX_COLS).max(1);
        let items: Vec<ListItem> = self.recent_files.iter()
            .skip(self.file_scroll)
            .take(body.height as usize)
            .map(|r| {
                let glyph = if r.sensitive { "⚠" } else { " " };
                let suffix = if r.coalesced { " (burst)" } else { "" };
                let style = if r.sensitive {
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
                } else { Style::default() };
                ListItem::new(Line::from(vec![
                    Span::raw(format!("{} {} {:>5} {:<8} ", r.op, glyph, r.pid, truncate(&r.comm, 8))),
                    Span::styled(format!("{}{}", truncate(&r.path.display().to_string(), path_cols), suffix), style),
                ]))
            }).collect();
        f.render_widget(List::new(items), body);
    }

    fn draw_commands(&self, f: &mut Frame, area: Rect) {
        let (_inner, body) = self.framed_pane(f, area, "COMMANDS", Pane::Commands,
            format!("{:>5}  {}", "PID", "ARGV"));
        const PREFIX_COLS: usize = 7; // 5 pid + 2 sep
        let argv_cols = (body.width as usize).saturating_sub(PREFIX_COLS).max(1);
        let items: Vec<ListItem> = self.commands.iter()
            .skip(self.commands_scroll)
            .take(body.height as usize)
            .map(|c| ListItem::new(format!(
                "{:>5}  {}", c.pid, truncate(&c.argv, argv_cols),
            )))
            .collect();
        f.render_widget(List::new(items), body);
    }

    fn draw_network(&self, f: &mut Frame, area: Rect) {
        let (_inner, body) = self.framed_pane(f, area, "NETWORK", Pane::Network,
            format!("{:<28}  {}", "HOST", "CONNS"));
        // HashMap iteration is non-deterministic; sort so scrolling stays stable.
        let mut nets: Vec<&NetRow> = self.network.values().collect();
        nets.sort_by(|a, b| a.host.cmp(&b.host));
        let items: Vec<ListItem> = nets.iter()
            .skip(self.network_scroll)
            .take(body.height as usize)
            .map(|n| ListItem::new(format!("{:<28}  {}", truncate(&n.host, 28), n.conns)))
            .collect();
        f.render_widget(List::new(items), body);
    }

    /// Render the rounded outer block + the column-header row, returning the
    /// inner area and the area below the header where the list should draw.
    fn framed_pane(&self, f: &mut Frame, area: Rect, name: &str, pane: Pane, header: String) -> (Rect, Rect) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(if self.focus == pane {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::DarkGray)
            })
            .title(self.title(name, pane));
        let inner = block.inner(area);
        f.render_widget(block, area);

        let split = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(1)])
            .split(inner);
        let hdr = Paragraph::new(Line::from(Span::styled(
            header,
            Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD),
        )));
        f.render_widget(hdr, split[0]);

        (inner, split[1])
    }

    fn title(&self, name: &str, pane: Pane) -> Line<'_> {
        let focused = self.focus == pane;
        Line::from(Span::styled(
            format!(" {} ", name),
            if focused {
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            }
        ))
    }

    fn draw_footer(&self, f: &mut Frame, area: Rect) {
        let key_style = Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD);
        let label_style = Style::default().fg(Color::Gray);
        let mut spans: Vec<Span> = Vec::new();
        for (k, label) in [("Tab", "cycle"), ("p", "pause"), ("j/k", "scroll"), ("q", "quit")] {
            spans.push(Span::styled(format!(" {k} "), key_style));
            spans.push(Span::styled(format!(" {label}  "), label_style));
        }
        f.render_widget(Paragraph::new(Line::from(spans)), area);
    }
}

/// Flatten the process map into rows pre-rendered with box-drawing tree
/// connectors. Pids whose ppid isn't tracked become roots; visited-tracking
/// guards against cycles in case bad data ever lands in the map.
fn build_tree_rows(processes: &HashMap<u32, ProcInfo>) -> Vec<(u32, String)> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut roots: Vec<u32> = Vec::new();
    for info in processes.values() {
        if processes.contains_key(&info.ppid) {
            children.entry(info.ppid).or_default().push(info.pid);
        } else {
            roots.push(info.pid);
        }
    }
    roots.sort();
    for kids in children.values_mut() { kids.sort(); }

    let mut rows: Vec<(u32, String)> = Vec::new();
    let mut visited: HashSet<u32> = HashSet::new();
    // Stack entry: (pid, ancestors_were_last_sibling, this_node_is_last_sibling).
    // ancestors_were_last_sibling drives whether to draw a vertical bar in each
    // spacer column. this_node_is_last_sibling picks ├─ vs └─.
    let mut stack: Vec<(u32, Vec<bool>, bool)> = Vec::new();
    for (i, &p) in roots.iter().enumerate().rev() {
        stack.push((p, Vec::new(), i + 1 == roots.len()));
    }
    while let Some((pid, ancestors_last, is_last)) = stack.pop() {
        if !visited.insert(pid) { continue; }
        let mut prefix = String::new();
        for &anc_last in &ancestors_last {
            prefix.push_str(if anc_last { "   " } else { "│  " });
        }
        if !ancestors_last.is_empty() {
            prefix.push_str(if is_last { "└─ " } else { "├─ " });
        }
        rows.push((pid, prefix));
        if let Some(kids) = children.get(&pid) {
            let mut new_ancestors = ancestors_last.clone();
            new_ancestors.push(is_last);
            for (i, &c) in kids.iter().enumerate().rev() {
                stack.push((c, new_ancestors.clone(), i + 1 == kids.len()));
            }
        }
    }
    rows
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n { s.to_string() } else {
        let mut t = s.chars().rev().take(n.saturating_sub(1)).collect::<String>();
        t = t.chars().rev().collect();
        format!("…{t}")
    }
}
