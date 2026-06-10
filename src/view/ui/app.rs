use crate::event::Event;
use crate::view::discovery::SessionEntry;
use crossterm::event::{self, Event as CtEvent, KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, Padding, Paragraph, Sparkline};
use ratatui::Frame;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::time::Duration;

const RATE_BUCKET_NS: u64 = 1_000_000_000;
const RATE_HISTORY_LEN: usize = 60;

/// ctrace wordmark, shown atop the help and quit modals. All rows are padded to
/// the same width so centered alignment stays flush.
const LOGO: [&str; 6] = [
    " ██████╗████████╗██████╗  █████╗  ██████╗███████╗",
    "██╔════╝╚══██╔══╝██╔══██╗██╔══██╗██╔════╝██╔════╝",
    "██║        ██║   ██████╔╝███████║██║     █████╗  ",
    "██║        ██║   ██╔══██╗██╔══██║██║     ██╔══╝  ",
    "╚██████╗   ██║   ██║  ██║██║  ██║╚██████╗███████╗",
    " ╚═════╝   ╚═╝   ╚═╝  ╚═╝╚═╝  ╚═╝ ╚═════╝╚══════╝",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane { Process, File, Commands, Network }

/// Panes in display + toggle order. Index here is the `1`..`4` key and the
/// position in `App::visible` / `App::filters`.
const PANE_ORDER: [Pane; 4] = [Pane::Process, Pane::File, Pane::Commands, Pane::Network];

/// Which interaction mode the UI is in. Only `Normal` runs navigation keys; the
/// others are transient overlays/input modes that capture the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode { Normal, Help, QuitConfirm, Filter }

pub struct App {
    pub session: SessionEntry,
    pub events: Vec<Event>,
    pub focus: Pane,
    pub paused: bool,
    pub dropped: usize,
    pub quit: bool,
    pub mode: Mode,
    /// Per-pane visibility, indexed by `PANE_ORDER`. At least one is always true.
    visible: [bool; 4],
    /// Whether the EVENTS/s rate panel (beside NETWORK) is shown. Toggled by `5`.
    /// Deliberately *not* a `Pane`: it never takes focus and can't be filtered.
    show_events: bool,
    /// Committed per-pane substring filter (lowercased compare), "" = no filter.
    filters: [String; 4],
    /// Live text being typed while in `Mode::Filter`, not yet committed.
    filter_draft: String,
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
            mode: Mode::Normal,
            visible: [true; 4],
            show_events: true,
            filters: [String::new(), String::new(), String::new(), String::new()],
            filter_draft: String::new(),
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
        // Ctrl-C is the always-works escape hatch, regardless of mode.
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.quit = true;
            return;
        }
        match self.mode {
            Mode::Help => self.mode = Mode::Normal, // any key dismisses help
            Mode::QuitConfirm => self.handle_quit_key(key),
            Mode::Filter => self.handle_filter_key(key),
            Mode::Normal => self.handle_normal_key(key),
        }
    }

    fn handle_quit_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => self.quit = true,
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => self.mode = Mode::Normal,
            _ => {}
        }
    }

    fn handle_filter_key(&mut self, key: KeyEvent) {
        match key.code {
            // Cancel: drop the draft, leaving any previously-committed filter intact.
            KeyCode::Esc => { self.filter_draft.clear(); self.mode = Mode::Normal; }
            // Commit the draft as the focused pane's filter.
            KeyCode::Enter => {
                let i = pane_index(self.focus);
                self.filters[i] = std::mem::take(&mut self.filter_draft);
                self.mode = Mode::Normal;
                self.clamp_scroll();
            }
            KeyCode::Backspace => { self.filter_draft.pop(); }
            KeyCode::Char(c) => self.filter_draft.push(c),
            _ => {}
        }
    }

    fn handle_normal_key(&mut self, key: KeyEvent) {
        match (key.code, key.modifiers) {
            (KeyCode::Char('q'), _) => self.mode = Mode::QuitConfirm,
            // Esc precedence: clear the focused pane's filter if set, else quit-confirm.
            (KeyCode::Esc, _) => {
                let i = pane_index(self.focus);
                if self.filters[i].is_empty() {
                    self.mode = Mode::QuitConfirm;
                } else {
                    self.filters[i].clear();
                    self.clamp_scroll();
                }
            }
            (KeyCode::Char('h'), _) | (KeyCode::Char('?'), _) | (KeyCode::F(1), _) => {
                self.mode = Mode::Help;
            }
            (KeyCode::Char('f'), _) | (KeyCode::Char('/'), _) => {
                // Seed the draft with the existing filter so it can be edited.
                self.filter_draft = self.filters[pane_index(self.focus)].clone();
                self.mode = Mode::Filter;
            }
            (KeyCode::Tab, _) => self.cycle_focus(1),
            (KeyCode::BackTab, _) => self.cycle_focus(-1),
            (KeyCode::Char('1'), _) => self.toggle_visible(Pane::Process),
            (KeyCode::Char('2'), _) => self.toggle_visible(Pane::File),
            (KeyCode::Char('3'), _) => self.toggle_visible(Pane::Commands),
            (KeyCode::Char('4'), _) => self.toggle_visible(Pane::Network),
            (KeyCode::Char('5'), _) => self.show_events = !self.show_events,
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

    fn is_visible(&self, p: Pane) -> bool { self.visible[pane_index(p)] }

    /// Toggle a pane's visibility. Hiding the last visible pane is a no-op;
    /// hiding the focused pane moves focus to the next visible one.
    fn toggle_visible(&mut self, p: Pane) {
        let i = pane_index(p);
        if self.visible[i] {
            if self.visible.iter().filter(|v| **v).count() <= 1 { return; }
            self.visible[i] = false;
            if self.focus == p { self.cycle_focus(1); }
        } else {
            self.visible[i] = true;
        }
    }

    /// Move focus by `dir` among visible panes. If the current focus is hidden,
    /// snap to the first visible pane instead.
    fn cycle_focus(&mut self, dir: i32) {
        let vis: Vec<Pane> = PANE_ORDER.into_iter().filter(|p| self.is_visible(*p)).collect();
        if vis.is_empty() { return; }
        let cur = match vis.iter().position(|p| *p == self.focus) {
            Some(c) => c,
            None => { self.focus = vis[0]; return; }
        };
        let n = vis.len() as i32;
        let next = (((cur as i32 + dir) % n) + n) % n;
        self.focus = vis[next as usize];
    }

    fn pane_filter(&self, p: Pane) -> &str { &self.filters[pane_index(p)] }

    fn focus_len(&self) -> usize {
        match self.focus {
            Pane::Process => self.filtered_proc_rows().len(),
            Pane::File => self.filtered_files().len(),
            Pane::Commands => self.filtered_commands().len(),
            Pane::Network => self.filtered_network().len(),
        }
    }

    fn scroll_mut(&mut self) -> &mut usize {
        match self.focus {
            Pane::Process => &mut self.process_scroll,
            Pane::File => &mut self.file_scroll,
            Pane::Commands => &mut self.commands_scroll,
            Pane::Network => &mut self.network_scroll,
        }
    }

    fn scroll(&mut self, delta: i64) {
        let max = self.focus_len();
        let s = self.scroll_mut();
        let next = (*s as i64 + delta).max(0) as usize;
        *s = next.min(max.saturating_sub(1));
    }

    fn scroll_to(&mut self, pos: usize) {
        let max = self.focus_len();
        let s = self.scroll_mut();
        *s = pos.min(max.saturating_sub(1));
    }

    /// Re-clamp the focused pane's scroll offset after its row count shrinks
    /// (e.g. a filter was applied/cleared).
    fn clamp_scroll(&mut self) {
        let max = self.focus_len();
        let s = self.scroll_mut();
        *s = (*s).min(max.saturating_sub(1));
    }

    // --- filtered views ---------------------------------------------------
    // Each returns the rows a pane should display given its committed filter.
    // focus_len() and the draw_* methods share these so scrolling stays in sync.

    fn filtered_files(&self) -> Vec<&FileRow> {
        let f = self.pane_filter(Pane::File).to_lowercase();
        self.recent_files.iter().filter(|r| {
            f.is_empty()
                || r.comm.to_lowercase().contains(&f)
                || r.path.display().to_string().to_lowercase().contains(&f)
        }).collect()
    }

    fn filtered_commands(&self) -> Vec<&CommandRow> {
        let f = self.pane_filter(Pane::Commands).to_lowercase();
        self.commands.iter()
            .filter(|c| f.is_empty() || c.argv.to_lowercase().contains(&f))
            .collect()
    }

    fn filtered_network(&self) -> Vec<&NetRow> {
        let f = self.pane_filter(Pane::Network).to_lowercase();
        // HashMap iteration is non-deterministic; sort so scrolling stays stable.
        let mut nets: Vec<&NetRow> = self.network.values()
            .filter(|n| f.is_empty() || n.host.to_lowercase().contains(&f))
            .collect();
        nets.sort_by(|a, b| a.host.cmp(&b.host));
        nets
    }

    /// Process rows to render. With no filter this is the full tree (with
    /// box-drawing connectors); with a filter it collapses to a flat,
    /// pid-sorted list of matching processes (connectors would dangle).
    fn filtered_proc_rows(&self) -> Vec<(u32, String)> {
        let f = self.pane_filter(Pane::Process).to_lowercase();
        if f.is_empty() {
            return build_tree_rows(&self.processes);
        }
        let mut v: Vec<(u32, String)> = self.processes.values()
            .filter(|p| p.comm.to_lowercase().contains(&f))
            .map(|p| (p.pid, String::new()))
            .collect();
        v.sort_by_key(|(pid, _)| *pid);
        v
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
                // argv[0] is the raw invoked path (e.g. "/usr/bin/rg"); the tree
                // wants the clean command name. Basename it, falling back to the
                // existing comm so we never blank the column.
                if let Some(c) = argv.first() {
                    let base = std::path::Path::new(c)
                        .file_name()
                        .and_then(|s| s.to_str())
                        .filter(|s| !s.is_empty());
                    if let Some(base) = base {
                        info.comm = base.to_string();
                    }
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
                Constraint::Length(1),  // header (status line; rate moved to EVENTS/s panel)
                Constraint::Min(3),     // main
                Constraint::Length(1),  // footer
            ])
            .split(area);
        self.draw_header(f, v[0]);
        self.draw_main(f, v[1]);
        self.draw_footer(f, v[2]);

        match self.mode {
            Mode::Help => { dim_backdrop(f, area); self.draw_help(f, area); }
            Mode::QuitConfirm => { dim_backdrop(f, area); self.draw_quit(f, area); }
            _ => {}
        }
    }

    /// Lay out the visible panes. Process owns the left column; Activity /
    /// Commands / Network stack in the right column, splitting height equally
    /// among whichever are visible. A hidden column lets the other span full
    /// width. The EVENTS/s panel (toggle `5`) shares NETWORK's row, splitting it
    /// 50/50; if NETWORK is hidden it becomes its own row in the right column.
    fn draw_main(&self, f: &mut Frame, area: Rect) {
        let right: Vec<Pane> = [Pane::File, Pane::Commands, Pane::Network]
            .into_iter().filter(|p| self.is_visible(*p)).collect();
        let process_vis = self.is_visible(Pane::Process);
        // EVENTS/s gets a dedicated row only when it can't ride NETWORK's row.
        let events_own_row = self.show_events && !self.is_visible(Pane::Network);
        let right_has_content = !right.is_empty() || events_own_row;

        let (proc_area, right_area) = if process_vis && right_has_content {
            let h = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(area);
            (Some(h[0]), Some(h[1]))
        } else if process_vis {
            (Some(area), None)
        } else {
            (None, Some(area))
        };

        if let Some(a) = proc_area {
            self.draw_processes(f, a);
        }
        let Some(a) = right_area else { return };

        let n = right.len() as u32 + events_own_row as u32;
        if n == 0 { return; }
        let constraints: Vec<Constraint> = (0..n).map(|_| Constraint::Ratio(1, n)).collect();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(a);
        for (i, pane) in right.iter().enumerate() {
            match pane {
                Pane::File => self.draw_files(f, chunks[i]),
                Pane::Commands => self.draw_commands(f, chunks[i]),
                Pane::Network if self.show_events => {
                    let split = Layout::default()
                        .direction(Direction::Horizontal)
                        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                        .split(chunks[i]);
                    self.draw_network(f, split[0]);
                    self.draw_events(f, split[1]);
                }
                Pane::Network => self.draw_network(f, chunks[i]),
                Pane::Process => {}
            }
        }
        if events_own_row {
            self.draw_events(f, chunks[right.len()]);
        }
    }

    fn draw_header(&self, f: &mut Frame, area: Rect) {
        // Single status line; the events-per-second sparkline lives in its own
        // panel beside NETWORK (toggle `5`), not in the header.
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
        f.render_widget(Paragraph::new(status), area);
    }

    /// The EVENTS/s panel: a one-minute sparkline under the live rate. Framed
    /// exactly like the other panes (same border + title + header row), it just
    /// never takes focus, so its title carries the unfocused gray styling and
    /// the leading `5` doubles as its show/hide hotkey hint.
    fn draw_events(&self, f: &mut Frame, area: Rect) {
        let title = Line::from(Span::styled(" 5 EVENTS/s ", Style::default().fg(Color::Gray)));
        let header = format!("{:>5}/s   last 60s", self.current_rate());
        let body = framed_chrome(f, area, title, false, header);

        let data: Vec<u64> = self.rate_history.iter().copied().collect();
        let spark = Sparkline::default()
            .data(data)
            .style(Style::default().fg(Color::Green));
        f.render_widget(spark, body);
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
        let body = self.framed_pane(f, area, "PROCESS TREE", Pane::Process, header);

        let rows = self.filtered_proc_rows();
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
        let body = self.framed_pane(f, area, "ACTIVITY", Pane::File,
            format!("    {:>5} {:<8} {}", "PID", "COMM", "PATH / CMD"));
        // Prefix is "X G PPPPP CCCCCCCC " = 1+1+1+1+5+1+8+1 = 19 cols
        const PREFIX_COLS: usize = 19;
        let path_cols = (body.width as usize).saturating_sub(PREFIX_COLS).max(1);
        let items: Vec<ListItem> = self.filtered_files().into_iter()
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
        let body = self.framed_pane(f, area, "COMMANDS", Pane::Commands,
            format!("{:>5}  {}", "PID", "ARGV"));
        const PREFIX_COLS: usize = 7; // 5 pid + 2 sep
        let argv_cols = (body.width as usize).saturating_sub(PREFIX_COLS).max(1);
        let items: Vec<ListItem> = self.filtered_commands().into_iter()
            .skip(self.commands_scroll)
            .take(body.height as usize)
            .map(|c| ListItem::new(format!(
                "{:>5}  {}", c.pid, truncate(&c.argv, argv_cols),
            )))
            .collect();
        f.render_widget(List::new(items), body);
    }

    fn draw_network(&self, f: &mut Frame, area: Rect) {
        // Host column flexes with width so the CONNS count is never clipped —
        // matters because NETWORK gets narrow when the EVENTS/s panel shares its
        // row.
        const CONNS_W: usize = 5;
        let host_w = (area.width as usize).saturating_sub(2 + CONNS_W + 2).max(6);
        let body = self.framed_pane(f, area, "NETWORK", Pane::Network,
            format!("{:<host_w$}  {:>CONNS_W$}", "HOST", "CONNS"));
        let items: Vec<ListItem> = self.filtered_network().into_iter()
            .skip(self.network_scroll)
            .take(body.height as usize)
            .map(|n| ListItem::new(format!(
                "{:<host_w$}  {:>CONNS_W$}", truncate(&n.host, host_w), n.conns,
            )))
            .collect();
        f.render_widget(List::new(items), body);
    }

    /// Render the rounded outer block + the column-header row for a focusable
    /// pane, returning the area below the header where the list should draw.
    fn framed_pane(&self, f: &mut Frame, area: Rect, name: &str, pane: Pane, header: String) -> Rect {
        framed_chrome(f, area, self.title(name, pane), self.focus == pane, header)
    }

    /// Pane title: " <n> NAME " plus a " [filter] " suffix when one is active.
    /// The leading digit doubles as the show/hide hotkey hint.
    fn title(&self, name: &str, pane: Pane) -> Line<'_> {
        let focused = self.focus == pane;
        let n = pane_index(pane) + 1;
        let filter = self.pane_filter(pane);
        let base = Style::default();
        let mut spans = vec![
            Span::styled(
                format!(" {n} {name} "),
                if focused {
                    base.fg(Color::Cyan).add_modifier(Modifier::BOLD)
                } else {
                    base.fg(Color::Gray)
                },
            ),
        ];
        if !filter.is_empty() {
            spans.push(Span::styled(
                format!("[{filter}] "),
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            ));
        }
        Line::from(spans)
    }

    fn draw_footer(&self, f: &mut Frame, area: Rect) {
        // In filter mode the footer becomes the input line.
        if self.mode == Mode::Filter {
            let pane_name = match self.focus {
                Pane::Process => "process",
                Pane::File => "activity",
                Pane::Commands => "commands",
                Pane::Network => "network",
            };
            let line = Line::from(vec![
                Span::styled(
                    format!(" filter {pane_name} "),
                    Style::default().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::raw(self.filter_draft.clone()),
                Span::styled("▎", Style::default().fg(Color::Yellow)),
                Span::styled("   Enter apply · Esc cancel", Style::default().fg(Color::DarkGray)),
            ]);
            f.render_widget(Paragraph::new(line), area);
            return;
        }

        let key_style = Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD);
        let label_style = Style::default().fg(Color::Gray);
        let mut spans: Vec<Span> = Vec::new();
        for (k, label) in [
            ("Tab", "cycle"), ("1-4", "panes"), ("5", "events"), ("p", "pause"),
            ("/", "filter"), ("h", "help"), ("q", "quit"),
        ] {
            spans.push(Span::styled(format!(" {k} "), key_style));
            spans.push(Span::styled(format!(" {label}  "), label_style));
        }
        f.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn draw_help(&self, f: &mut Frame, area: Rect) {
        let rect = centered_fixed(58, 33, area);
        let block = modal_block(" keybindings ");
        let inner = block.inner(rect);
        f.render_widget(Clear, rect);
        f.render_widget(block, rect);

        let mut lines = logo_lines();
        lines.extend([
            Line::raw(""),
            help_group("Navigation"),
            help_kv("Tab / S-Tab", "cycle panes fwd / back"),
            help_kv("1 2 3 4 5", "show / hide panes & events"),
            help_kv("j k  ↑ ↓", "scroll"),
            help_kv("PgUp PgDn", "scroll by page"),
            help_kv("g / G", "jump to top / bottom"),
            Line::raw(""),
            help_group("Display"),
            help_kv("p", "pause / resume"),
            help_kv("f  /", "filter focused pane"),
            Line::raw(""),
            help_group("Activity glyphs"),
            help_legend("R read  W write  C create  X close  D delete"),
            help_legend("M move  E edit  A multi-edit  $ bash  ⚠ sensitive"),
            Line::raw(""),
            help_group("General"),
            help_kv("h / ? / F1", "toggle this help"),
            help_kv("q / Esc", "quit (confirm)"),
            help_kv("Ctrl-C", "quit immediately"),
            Line::raw(""),
            Line::from(Span::styled(
                "press any key to close",
                Style::default().fg(Color::DarkGray),
            )).alignment(Alignment::Center),
        ]);
        f.render_widget(Paragraph::new(lines), inner);
    }

    fn draw_quit(&self, f: &mut Frame, area: Rect) {
        let rect = centered_fixed(58, 16, area);
        let block = modal_block(" Quit ctrace? ");
        let inner = block.inner(rect);
        f.render_widget(Clear, rect);
        f.render_widget(block, rect);

        let mut lines = logo_lines();
        lines.extend([
            Line::raw(""),
            Line::from(Span::raw("Stop watching this session?")).alignment(Alignment::Center),
            Line::raw(""),
            Line::from(vec![
                key_cap("y"), Span::raw(" Yes"),
                Span::raw("      "),
                key_cap("n"), Span::raw(" No"),
            ]).alignment(Alignment::Center),
            Line::raw(""),
            Line::from(Span::styled(
                "Enter / y  ·  Esc / n",
                Style::default().fg(Color::DarkGray),
            )).alignment(Alignment::Center),
        ]);
        f.render_widget(Paragraph::new(lines), inner);
    }
}

fn pane_index(p: Pane) -> usize {
    match p {
        Pane::Process => 0,
        Pane::File => 1,
        Pane::Commands => 2,
        Pane::Network => 3,
    }
}

/// Render the rounded outer block (cyan when focused, else dim) + the reversed
/// column-header row, returning the area below the header for the body content.
/// Shared by every pane and the EVENTS/s panel so they all frame identically.
fn framed_chrome(f: &mut Frame, area: Rect, title: Line, focused: bool, header: String) -> Rect {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(if focused {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        })
        .title(title);
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

    split[1]
}

/// Dim every cell in `area` so a modal reads as a focused overlay. Runs after
/// the main UI is drawn but before the modal, which then overwrites (un-dims)
/// its own footprint via `Clear`.
fn dim_backdrop(f: &mut Frame, area: Rect) {
    let buf = f.buffer_mut();
    // Only add DIM — keep each cell's own colors so the dashboard just darkens
    // rather than turning a flat near-black.
    let dim = Style::default().add_modifier(Modifier::DIM);
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            buf[(x, y)].set_style(dim);
        }
    }
}

/// Shared modal chrome: cyan rounded border + cyan-bold title and a little
/// breathing room, matching the pane styling so the overlays don't look like a
/// different app.
fn modal_block(title: &str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Cyan))
        .padding(Padding::new(2, 2, 1, 1))
        .title(Line::from(Span::styled(
            title.to_string(),
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        )))
}

/// The ctrace wordmark as centered, cyan-bold lines for modal headers.
fn logo_lines() -> Vec<Line<'static>> {
    LOGO.iter()
        .map(|l| {
            Line::from(Span::styled(
                *l,
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ))
            .alignment(Alignment::Center)
        })
        .collect()
}

/// A footer-style key cap: black text on a cyan chip, matching the bottom bar.
fn key_cap(k: &str) -> Span<'static> {
    Span::styled(
        format!(" {k} "),
        Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD),
    )
}

fn help_legend(text: &str) -> Line<'static> {
    Line::from(vec![
        Span::raw("  "),
        Span::styled(text.to_string(), Style::default().fg(Color::Gray)),
    ])
}

fn help_group(name: &str) -> Line<'static> {
    Line::from(Span::styled(
        name.to_string(),
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
    ))
}

fn help_kv(key: &str, desc: &str) -> Line<'static> {
    Line::from(vec![
        Span::raw("  "),
        Span::styled(format!("{key:<14}"), Style::default().fg(Color::Yellow)),
        Span::styled(desc.to_string(), Style::default().fg(Color::Gray)),
    ])
}

/// A rectangle of fixed size centered within `area`, clamped to fit.
fn centered_fixed(width: u16, height: u16, area: Rect) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::session::Meta;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn test_app() -> App {
        let meta = Meta {
            session_id: "test".into(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            cwd: PathBuf::from("/tmp"),
            argv: vec![],
            claude_pid: 0,
            tracer_pid: 0,
            hostname: "h".into(),
            macos_version: "x".into(),
            ctrace_version: "0".into(),
        };
        let entry = SessionEntry {
            dir: PathBuf::from("/tmp"),
            meta,
            status: "replay".into(),
            events_path: PathBuf::from("/tmp/events.jsonl"),
        };
        App::new(entry)
    }

    fn key(c: char) -> KeyEvent { KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE) }
    fn code(k: KeyCode) -> KeyEvent { KeyEvent::new(k, KeyModifiers::NONE) }

    #[test]
    fn q_opens_quit_confirm_then_cancel_and_confirm() {
        let mut app = test_app();
        app.handle_key(key('q'));
        assert_eq!(app.mode, Mode::QuitConfirm);
        assert!(!app.quit);
        app.handle_key(key('n'));
        assert_eq!(app.mode, Mode::Normal);
        assert!(!app.quit);

        app.handle_key(key('q'));
        app.handle_key(key('y'));
        assert!(app.quit);
    }

    #[test]
    fn ctrl_c_quits_instantly_from_any_mode() {
        let mut app = test_app();
        app.mode = Mode::Help;
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.quit);
    }

    #[test]
    fn help_toggles_and_any_key_dismisses() {
        let mut app = test_app();
        app.handle_key(key('h'));
        assert_eq!(app.mode, Mode::Help);
        app.handle_key(key('j')); // any key closes
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn cannot_hide_last_visible_pane() {
        let mut app = test_app();
        app.toggle_visible(Pane::Process);
        app.toggle_visible(Pane::File);
        app.toggle_visible(Pane::Commands);
        // Only Network left; hiding it must be refused.
        app.toggle_visible(Pane::Network);
        assert!(app.is_visible(Pane::Network));
        assert_eq!(app.visible.iter().filter(|v| **v).count(), 1);
    }

    #[test]
    fn hiding_focused_pane_moves_focus() {
        let mut app = test_app();
        assert_eq!(app.focus, Pane::Process);
        app.toggle_visible(Pane::Process);
        assert!(!app.is_visible(Pane::Process));
        assert_ne!(app.focus, Pane::Process);
        assert!(app.is_visible(app.focus));
    }

    #[test]
    fn tab_skips_hidden_panes() {
        let mut app = test_app();
        app.toggle_visible(Pane::File);     // hide pane 2
        app.toggle_visible(Pane::Commands); // hide pane 3
        // Visible: Process, Network. Tab from Process -> Network.
        app.handle_key(code(KeyCode::Tab));
        assert_eq!(app.focus, Pane::Network);
        app.handle_key(code(KeyCode::Tab));
        assert_eq!(app.focus, Pane::Process);
    }

    #[test]
    fn back_tab_cycles_backwards() {
        let mut app = test_app();
        app.handle_key(code(KeyCode::BackTab));
        assert_eq!(app.focus, Pane::Network);
    }

    #[test]
    fn filter_commit_and_esc_clears() {
        let mut app = test_app();
        app.focus = Pane::Commands;
        app.commands = vec![
            CommandRow { pid: 1, argv: "rg foo".into() },
            CommandRow { pid: 2, argv: "ls -la".into() },
        ];
        // Enter filter mode, type "rg", commit.
        app.handle_key(key('/'));
        assert_eq!(app.mode, Mode::Filter);
        app.handle_key(key('r'));
        app.handle_key(key('g'));
        app.handle_key(code(KeyCode::Enter));
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.filtered_commands().len(), 1);

        // Esc in Normal clears the focused pane's filter (not quit).
        app.handle_key(code(KeyCode::Esc));
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.filtered_commands().len(), 2);
    }

    #[test]
    fn esc_opens_quit_when_no_filter() {
        let mut app = test_app();
        app.handle_key(code(KeyCode::Esc));
        assert_eq!(app.mode, Mode::QuitConfirm);
    }

    #[test]
    fn key_5_toggles_events_panel() {
        let mut app = test_app();
        assert!(app.show_events);
        app.handle_key(key('5'));
        assert!(!app.show_events);
        app.handle_key(key('5'));
        assert!(app.show_events);
    }

    #[test]
    fn key_5_is_not_in_focus_cycle() {
        // `5` must never move focus or be reachable via Tab — it's display-only.
        let mut app = test_app();
        let before = app.focus;
        app.handle_key(key('5'));
        assert_eq!(app.focus, before);
    }

    #[test]
    fn exec_basenames_the_command_column() {
        use crate::event::{Event, EventData, EventKind, ProcessRef};
        use std::sync::Arc;
        let mut app = test_app();
        let ev = Event {
            ts_ns: 1,
            kind: EventKind::Exec,
            pid: 42,
            ppid: 1,
            process: Arc::new(ProcessRef {
                pid: 42,
                comm: "rg".into(),
                image: PathBuf::from("/usr/bin/rg"),
                argv: vec!["/usr/bin/rg".into(), "foo".into()],
            }),
            data: EventData::Exec {
                argv: vec!["/usr/bin/rg".into(), "foo".into()],
                image: PathBuf::from("/usr/bin/rg"),
            },
            flags: 0,
        };
        app.ingest(ev);
        // The tree shows the clean basename, not the raw "/usr/bin/rg" path.
        assert_eq!(app.processes[&42].comm, "rg");
    }

    #[test]
    fn filter_esc_cancels_without_committing() {
        let mut app = test_app();
        app.focus = Pane::Commands;
        app.handle_key(key('/'));
        app.handle_key(key('x'));
        app.handle_key(code(KeyCode::Esc)); // cancel input
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.pane_filter(Pane::Commands).is_empty());
    }
}
