use crate::event::Event;
use crate::flags::{FlagConfig, Severity};
use crate::view::discovery::SessionEntry;
use crossterm::event::{self, Event as CtEvent, KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Padding, Paragraph};
use ratatui::Frame;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::time::Duration;

const RATE_BUCKET_NS: u64 = 1_000_000_000;
/// Height of the full-width EVENTS/s chart band (border + bars + x-axis baseline
/// + time labels).
const EVENTS_BAND_H: u16 = 11;
/// Vertical bar glyphs by eighths (0..=8) for the EVENTS/s bar graph.
const BAR8: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// tracce wordmark, shown atop the help and quit modals. All rows are padded to
/// the same width so centered alignment stays flush.
const LOGO: [&str; 6] = [
    "████████╗██████╗  █████╗  ██████╗ ██████╗███████╗",
    "╚══██╔══╝██╔══██╗██╔══██╗██╔════╝██╔════╝██╔════╝",
    "   ██║   ██████╔╝███████║██║     ██║     █████╗  ",
    "   ██║   ██╔══██╗██╔══██║██║     ██║     ██╔══╝  ",
    "   ██║   ██║  ██║██║  ██║╚██████╗╚██████╗███████╗",
    "   ╚═╝   ╚═╝  ╚═╝╚═╝  ╚═╝ ╚═════╝ ╚═════╝╚══════╝",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane { Process, File, Commands, Network, Events }

/// Panes in display + toggle order. Index here is the `1`..`5` key and the
/// position in `App::visible` / `App::filters` / `App::list_state`. `Events` is
/// the full-width EVENTS/s chart band; it's focusable but has no filter or rows
/// (its `list_state` selection is reused as the scrub cursor).
const PANE_ORDER: [Pane; 5] = [Pane::Process, Pane::File, Pane::Commands, Pane::Network, Pane::Events];

/// Selectable seconds-per-bar levels for the EVENTS/s x-axis zoom (Up/Down step
/// through these while the pane is focused). `1` = one bar per second.
const ZOOM_LEVELS: [usize; 6] = [1, 2, 5, 10, 30, 60];

/// Which interaction mode the UI is in. Only `Normal` runs navigation keys; the
/// others are transient overlays/input modes that capture the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode { Normal, Help, QuitConfirm, Filter, Detail, ExportFlash }

/// A frozen snapshot of one row's full fields, shown in the detail modal. Built
/// at the moment Enter is pressed so live updates can't shift it under the user.
struct DetailView {
    title: String,
    rows: Vec<(String, String)>,
}

pub struct App {
    pub session: SessionEntry,
    pub events: Vec<Event>,
    /// Currently focused pane, or `None` for the "follow-all" overview (no
    /// highlight, every pane pinned to its latest). This is the default and one
    /// slot in the Tab cycle.
    pub focus: Option<Pane>,
    pub paused: bool,
    pub dropped: usize,
    pub quit: bool,
    /// Set by `s` to return to the session picker without quitting the process.
    /// `view::run::run` checks this after the render loop exits and, when set,
    /// reopens the picker instead of returning.
    pub switch: bool,
    pub mode: Mode,
    /// Per-pane visibility, indexed by `PANE_ORDER`. At least one is always true.
    /// `Events` (index 4) is the EVENTS/s band, toggled by `5`.
    visible: [bool; 5],
    /// Committed per-pane substring filter (lowercased compare), "" = no filter.
    /// `Events` has no filter (its slot stays empty).
    filters: [String; 5],
    /// Live text being typed while in `Mode::Filter`, not yet committed.
    filter_draft: String,
    /// Per-pane list selection + viewport. `selected == None` means "follow the
    /// latest" (no highlight, pinned to the newest row); `Some(i)` is browse mode
    /// with a highlighted row. For `Events`, the selection is the scrub cursor's
    /// age in displayed bars (0 = now). Indexed parallel to `PANE_ORDER`.
    list_state: [ListState; 5],
    /// Frozen detail snapshot for `Mode::Detail`; `None` outside it.
    detail: Option<DetailView>,
    /// Result message shown in the export flash modal; `None` outside it.
    pub flash: Option<String>,
    /// User-editable command/path flag patterns (yellow/red severity
    /// coloring), loaded once when the session is opened.
    flags: FlagConfig,

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
    /// EVENTS/s x-axis zoom: seconds aggregated into each displayed bar (one of
    /// `ZOOM_LEVELS`). Purely a view transform over `rate_series` — never
    /// persisted, so it behaves identically on live and replayed sessions.
    events_zoom: usize,
}

pub struct ProcInfo { pub pid: u32, pub comm: String, pub ppid: u32, pub event_count: usize, pub last_ts_ns: u64, pub severity: Option<Severity> }
pub struct FileRow { pub pid: u32, pub comm: String, pub op: char, pub path: PathBuf, pub sensitive: bool, pub coalesced: bool, pub ts_ns: u64, pub severity: Option<Severity> }
pub struct CommandRow { pub pid: u32, pub argv: String, pub ts_ns: u64, pub severity: Option<Severity> }
pub struct NetRow { pub host: String, pub conns: usize, pub last_ts_ns: u64 }

impl App {
    pub fn new(session: SessionEntry, flags: FlagConfig) -> Self {
        Self {
            session,
            events: Vec::new(),
            focus: None,
            paused: false,
            dropped: 0,
            quit: false,
            switch: false,
            mode: Mode::Normal,
            visible: [true; 5],
            filters: std::array::from_fn(|_| String::new()),
            filter_draft: String::new(),
            list_state: std::array::from_fn(|_| ListState::default()),
            detail: None,
            flash: None,
            flags,
            processes: HashMap::new(),
            recent_files: Vec::new(),
            commands: Vec::new(),
            network: HashMap::new(),
            sensitive_count: 0,
            rate_history: VecDeque::new(),
            current_bucket: 0,
            bucket_anchor_ns: 0,
            first_event_ns: None,
            last_event_ns: 0,
            events_zoom: 1,
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
            Mode::Detail => self.handle_detail_key(key),
            Mode::ExportFlash => { self.mode = Mode::Normal; self.flash = None; } // any key dismisses
            Mode::Normal => self.handle_normal_key(key),
        }
    }

    /// In the detail modal, Esc / Enter / q return to the dashboard. Other keys
    /// are inert so a stray press can't quit or navigate underneath.
    fn handle_detail_key(&mut self, key: KeyEvent) {
        if matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q')) {
            self.mode = Mode::Normal;
            self.detail = None;
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
                if let Some(p) = self.focus {
                    self.filters[pane_index(p)] = std::mem::take(&mut self.filter_draft);
                }
                self.mode = Mode::Normal;
                self.clamp_selection();
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
                match self.focus {
                    Some(p) if !self.filters[pane_index(p)].is_empty() => {
                        self.filters[pane_index(p)].clear();
                        self.clamp_selection();
                    }
                    _ => self.mode = Mode::QuitConfirm,
                }
            }
            (KeyCode::Char('h'), _) | (KeyCode::Char('?'), _) | (KeyCode::F(1), _) => {
                self.mode = Mode::Help;
            }
            // `e` exports the session being viewed to ./<id>.tracce.tgz.
            (KeyCode::Char('e'), _) => self.export_session(),
            // `s` returns to the session picker (e.g. to switch to another live
            // session) without a confirm prompt — it's non-destructive, unlike quit.
            (KeyCode::Char('s'), _) => self.switch = true,
            // `/` opens filter on the focused row pane (Events isn't filterable).
            (KeyCode::Char('/'), _) => {
                if let Some(p) = self.focus {
                    if p != Pane::Events {
                        self.filter_draft = self.filters[pane_index(p)].clone();
                        self.mode = Mode::Filter;
                    }
                }
            }
            // `f` drops the focused pane's highlight back to follow-latest.
            (KeyCode::Char('f'), _) => self.sel_follow(),
            (KeyCode::Tab, _) => self.cycle_focus(1),
            (KeyCode::BackTab, _) => self.cycle_focus(-1),
            (KeyCode::Char('1'), _) => self.toggle_visible(Pane::Process),
            (KeyCode::Char('2'), _) => self.toggle_visible(Pane::File),
            (KeyCode::Char('3'), _) => self.toggle_visible(Pane::Commands),
            (KeyCode::Char('4'), _) => self.toggle_visible(Pane::Network),
            (KeyCode::Char('5'), _) => self.toggle_visible(Pane::Events),
            (KeyCode::Char('p'), _) => self.paused = !self.paused,
            // Selection moves in "age" units: +1 = older, -1 = newer. For the
            // horizontal EVENTS/s chart Left/Right scrub the cursor (Left = older,
            // Right = newer) while Up/Down zoom the time axis instead of scrubbing.
            (KeyCode::Char('j'), _) | (KeyCode::Down, _) => {
                if self.focus == Some(Pane::Events) { self.zoom_events(1); } else { self.sel_move(1); }
            }
            (KeyCode::Char('k'), _) | (KeyCode::Up, _) => {
                if self.focus == Some(Pane::Events) { self.zoom_events(-1); } else { self.sel_move(-1); }
            }
            (KeyCode::Left, _)  => self.sel_move(if self.focus == Some(Pane::Events) { 1 } else { -1 }),
            (KeyCode::Right, _) => self.sel_move(if self.focus == Some(Pane::Events) { -1 } else { 1 }),
            (KeyCode::PageDown, _)                        => self.sel_move(10),
            (KeyCode::PageUp, _)                          => self.sel_move(-10),
            (KeyCode::Char('g'), _) | (KeyCode::Home, _)  => self.sel_follow(),
            (KeyCode::Char('G'), _) | (KeyCode::End, _)   => self.sel_end(),
            (KeyCode::Enter, _)                           => self.open_detail(),
            _ => {}
        }
    }

    fn is_visible(&self, p: Pane) -> bool { self.visible[pane_index(p)] }

    /// Toggle a pane's visibility. Hiding the last visible pane is a no-op;
    /// hiding the focused pane drops focus to the follow-all state.
    fn toggle_visible(&mut self, p: Pane) {
        let i = pane_index(p);
        if self.visible[i] {
            if self.visible.iter().filter(|v| **v).count() <= 1 { return; }
            self.visible[i] = false;
            if self.focus == Some(p) { self.focus = None; }
        } else {
            self.visible[i] = true;
        }
    }

    /// Move focus by `dir` around the ring `None → <visible panes> → None`. The
    /// `None` slot is the follow-all overview (no highlight anywhere).
    fn cycle_focus(&mut self, dir: i32) {
        let mut ring: Vec<Option<Pane>> = vec![None];
        ring.extend(PANE_ORDER.into_iter().filter(|p| self.is_visible(*p)).map(Some));
        let n = ring.len() as i32;
        let cur = ring.iter().position(|f| *f == self.focus).unwrap_or(0) as i32;
        let next = (((cur + dir) % n) + n) % n;
        self.focus = ring[next as usize];
    }

    fn pane_filter(&self, p: Pane) -> &str { &self.filters[pane_index(p)] }

    /// Number of selectable items in the focused pane (0 if no pane is focused).
    /// For `Events` this is the number of displayed bars at the current zoom —
    /// the range the scrub cursor can move over.
    fn focus_len(&self) -> usize {
        match self.focus {
            Some(Pane::Process) => self.filtered_proc_rows().len(),
            Some(Pane::File) => self.filtered_files().len(),
            Some(Pane::Commands) => self.filtered_commands().len(),
            Some(Pane::Network) => self.filtered_network().len(),
            Some(Pane::Events) => self.zoomed_len(),
            None => 0,
        }
    }

    /// Step the EVENTS/s x-axis zoom through `ZOOM_LEVELS`. `step > 0` zooms out
    /// (more seconds per bar), `step < 0` zooms in. The displayed-bar count
    /// changes, so re-clamp the scrub cursor afterwards.
    fn zoom_events(&mut self, step: i32) {
        let cur = ZOOM_LEVELS.iter().position(|z| *z == self.events_zoom).unwrap_or(0) as i32;
        let next = (cur + step).clamp(0, ZOOM_LEVELS.len() as i32 - 1) as usize;
        self.events_zoom = ZOOM_LEVELS[next];
        self.clamp_selection();
    }

    fn focus_selected(&self) -> Option<usize> {
        self.focus.and_then(|p| self.list_state[pane_index(p)].selected())
    }

    /// Move the focused pane's selection by `delta` rows. From follow mode
    /// (`None`), a downward move enters browse mode at the top; an upward move
    /// stays in follow mode. Moving up off the top row returns to follow mode
    /// (highlight off, pinned to newest).
    fn sel_move(&mut self, delta: i64) {
        let Some(p) = self.focus else { return };
        let len = self.focus_len();
        if len == 0 { return; }
        let st = &mut self.list_state[pane_index(p)];
        let next = match st.selected() {
            None => if delta > 0 { Some(0) } else { None },
            Some(i) => {
                if delta < 0 && i == 0 {
                    None
                } else {
                    Some((i as i64 + delta).clamp(0, len as i64 - 1) as usize)
                }
            }
        };
        st.select(next);
    }

    /// `f` / `g` / Home: resume following the latest (drop the highlight/cursor).
    fn sel_follow(&mut self) {
        if let Some(p) = self.focus {
            self.list_state[pane_index(p)].select(None);
        }
    }

    /// `G` / End: jump to and highlight the last (oldest) row.
    fn sel_end(&mut self) {
        let Some(p) = self.focus else { return };
        let len = self.focus_len();
        if len > 0 {
            self.list_state[pane_index(p)].select(Some(len - 1));
        }
    }

    /// Re-clamp the focused pane's selection after its row count shrinks (e.g. a
    /// filter was applied/cleared) so it never points past the end.
    fn clamp_selection(&mut self) {
        let Some(p) = self.focus else { return };
        let len = self.focus_len();
        let st = &mut self.list_state[pane_index(p)];
        match st.selected() {
            Some(_) if len == 0 => st.select(None),
            Some(i) if i >= len => st.select(Some(len.saturating_sub(1))),
            _ => {}
        }
    }

    /// Keep a browsing selection glued to the same logical row when a new row is
    /// inserted at the front (ACTIVITY / COMMANDS). Skipped in follow mode
    /// (`None`) and when a filter is active (front-insert may not match, so the
    /// index shift is unreliable).
    fn glue_selection(&mut self, pane: Pane, len: usize) {
        let i = pane_index(pane);
        if !self.filters[i].is_empty() { return; }
        if let Some(sel) = self.list_state[i].selected() {
            self.list_state[i].select(Some((sel + 1).min(len.saturating_sub(1))));
        }
    }

    /// Export the session being viewed to ./<id>.tracce.tgz and show a flash.
    fn export_session(&mut self) {
        let out =
            std::path::PathBuf::from(format!("{}.tracce.tgz", self.session.meta.session_id));
        let msg = match crate::bundle::export(&self.session, &out) {
            Ok(n) => format!("exported -> {} ({} bytes)", out.display(), n),
            Err(e) => format!("export failed: {e:#}"),
        };
        self.flash = Some(msg);
        self.mode = Mode::ExportFlash;
    }

    /// Open the detail modal for the focused pane's selected row (or the top row
    /// / now-bucket when still following). No-op if nothing is focused/available.
    fn open_detail(&mut self) {
        let Some(p) = self.focus else { return };
        let idx = self.focus_selected().unwrap_or(0);
        if let Some(dv) = self.build_detail(p, idx) {
            self.detail = Some(dv);
            self.mode = Mode::Detail;
        }
    }

    /// Snapshot the full (untruncated) fields of one row for the detail modal.
    fn build_detail(&self, pane: Pane, idx: usize) -> Option<DetailView> {
        match pane {
            Pane::Process => {
                let rows = self.filtered_proc_rows();
                let (pid, _) = rows.get(idx)?;
                let p = self.processes.get(pid)?;
                Some(DetailView {
                    title: " Process detail ".into(),
                    rows: vec![
                        ("Command".into(), p.comm.clone()),
                        ("PID".into(), p.pid.to_string()),
                        ("Parent".into(), p.ppid.to_string()),
                        ("Events".into(), p.event_count.to_string()),
                        ("Severity".into(), severity_label(p.severity)),
                        ("Last seen".into(), self.rel_time(p.last_ts_ns)),
                    ],
                })
            }
            Pane::File => {
                let rows = self.filtered_files();
                let r = rows.get(idx)?;
                Some(DetailView {
                    title: " Activity detail ".into(),
                    rows: vec![
                        ("Operation".into(), format!("{} ({})", op_name(r.op), r.op)),
                        ("Path".into(), r.path.display().to_string()),
                        ("Process".into(), format!("{} (pid {})", r.comm, r.pid)),
                        ("Sensitive".into(), if r.sensitive { "⚠  yes".into() } else { "no".into() }),
                        ("Severity".into(), severity_label(r.severity)),
                        ("Burst".into(), if r.coalesced { "yes (coalesced)".into() } else { "no".into() }),
                        ("When".into(), self.rel_time(r.ts_ns)),
                    ],
                })
            }
            Pane::Commands => {
                let rows = self.filtered_commands();
                let r = rows.get(idx)?;
                Some(DetailView {
                    title: " Command detail ".into(),
                    rows: vec![
                        ("PID".into(), r.pid.to_string()),
                        ("Argv".into(), r.argv.clone()),
                        ("Severity".into(), severity_label(r.severity)),
                        ("When".into(), self.rel_time(r.ts_ns)),
                    ],
                })
            }
            Pane::Network => {
                let rows = self.filtered_network();
                let r = rows.get(idx)?;
                Some(DetailView {
                    title: " Network detail ".into(),
                    rows: vec![
                        ("Host".into(), r.host.clone()),
                        ("Connections".into(), r.conns.to_string()),
                        ("Last seen".into(), self.rel_time(r.last_ts_ns)),
                    ],
                })
            }
            Pane::Events => {
                // `idx` is the cursor's age in displayed bars (0 = now); each bar
                // spans `events_zoom` seconds.
                let z = self.events_zoom.max(1);
                let series = self.zoomed_series();
                let pos = series.len().checked_sub(1 + idx)?;
                let rate = *series.get(pos)?;
                let newest = idx * z;            // seconds-ago of the bar's newest edge
                // Approximate the bar's wall time: now, minus its newest edge.
                let ts = self.last_event_ns.saturating_sub(newest as u64 * RATE_BUCKET_NS);
                let age = if z == 1 {
                    if idx == 0 { "now".into() } else { format!("{newest}s ago") }
                } else {
                    format!("{}–{}s ago", newest, newest + z - 1)
                };
                Some(DetailView {
                    title: " Events/s detail ".into(),
                    rows: vec![
                        ("Rate".into(), format!("{rate} events/s")),
                        ("Interval".into(), format!("{z}s/bar")),
                        ("Age".into(), age),
                        ("When".into(), self.rel_time(ts)),
                        ("Recorded".into(), format!("{}s of history", self.rate_series_len())),
                    ],
                })
            }
        }
    }

    /// The events-per-second series, oldest first, with the in-progress bucket
    /// appended as the latest ("now") sample.
    fn rate_series(&self) -> Vec<u64> {
        self.rate_history.iter().copied().chain(std::iter::once(self.current_bucket)).collect()
    }

    /// `rate_series` aggregated into displayed bars of `events_zoom` seconds each
    /// (rounded mean events/s, so the y-axis stays "/s"). Grouped from the newest
    /// sample so the last bar always covers "now"; oldest bar first. With zoom 1
    /// it equals `rate_series`.
    fn zoomed_series(&self) -> Vec<u64> {
        let z = self.events_zoom.max(1);
        let series = self.rate_series();
        if z == 1 { return series; }
        let mut out = Vec::with_capacity(series.len().div_ceil(z));
        let mut i = series.len();
        while i > 0 {
            let start = i.saturating_sub(z);
            let chunk = &series[start..i];
            let len = chunk.len() as u64;
            out.push((chunk.iter().sum::<u64>() + len / 2) / len);
            i = start;
        }
        out.reverse();
        out
    }

    /// Number of displayed bars at the current zoom.
    fn zoomed_len(&self) -> usize {
        self.rate_series_len().div_ceil(self.events_zoom.max(1))
    }

    fn rate_series_len(&self) -> usize {
        self.rate_history.len() + 1
    }

    /// Format an event timestamp as time-into-the-session, e.g. `+1:23`.
    fn rel_time(&self, ts_ns: u64) -> String {
        let Some(start) = self.first_event_ns else { return "—".into() };
        let secs = ts_ns.saturating_sub(start) / 1_000_000_000;
        format!("+{}:{:02}", secs / 60, secs % 60)
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
        // Retain the full per-second history for the session so the EVENTS/s
        // chart can scrub arbitrarily far back (one u64 per second is cheap).
        while ev.ts_ns >= self.bucket_anchor_ns.saturating_add(RATE_BUCKET_NS) {
            self.rate_history.push_back(self.current_bucket);
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
                last_ts_ns: ev.ts_ns,
                severity: None,
            });
            info.event_count += 1;
            info.last_ts_ns = ev.ts_ns;
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
                info.severity = self.flags.classify(&argv.join(" "));
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
                    let severity = self.flags.classify(&joined);
                    self.commands.insert(0, CommandRow { pid: ev.pid, argv: joined, ts_ns: ev.ts_ns, severity });
                    if self.commands.len() > 500 { self.commands.truncate(500); }
                    self.glue_selection(Pane::Commands, self.commands.len());
                }
            }
            Fork { .. } | Exit { .. } => {}
            File { op, path, .. } => {
                let comm = self.processes.get(&ev.pid)
                    .map(|p| p.comm.clone())
                    .unwrap_or_else(|| ev.process.comm.clone());
                let severity = self.flags.classify(&path.display().to_string());
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
                    severity,
                    ts_ns: ev.ts_ns,
                });
                if self.recent_files.len() > 200 { self.recent_files.truncate(200); }
                self.glue_selection(Pane::File, self.recent_files.len());
            }
            NetOpen { host, remote, .. } => {
                let key = host.clone().unwrap_or_else(|| remote.ip().to_string());
                let row = self.network.entry(key.clone())
                    .or_insert(NetRow { host: key, conns: 0, last_ts_ns: ev.ts_ns });
                row.conns += 1;
                row.last_ts_ns = ev.ts_ns;
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

    pub fn draw(&mut self, f: &mut Frame) {
        let area = f.area();
        let events_on = self.is_visible(Pane::Events);
        let mut constraints = vec![Constraint::Length(1), Constraint::Min(3)];
        if events_on { constraints.push(Constraint::Length(EVENTS_BAND_H)); }
        constraints.push(Constraint::Length(1)); // footer
        let v = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(area);
        self.draw_header(f, v[0]);
        self.draw_main(f, v[1]);
        if events_on {
            let focused = self.focus == Some(Pane::Events);
            self.draw_events(f, v[2], focused);
        }
        self.draw_footer(f, v[v.len() - 1]);

        match self.mode {
            Mode::Help => { dim_backdrop(f, area); self.draw_help(f, area); }
            Mode::QuitConfirm => { dim_backdrop(f, area); self.draw_quit(f, area); }
            Mode::Detail => { dim_backdrop(f, area); self.draw_detail(f, area); }
            Mode::ExportFlash => { dim_backdrop(f, area); self.draw_flash(f, area); }
            _ => {}
        }
    }

    /// Lay out the row panes. Process owns the left column; Activity / Commands /
    /// Network stack in the right column, splitting height equally among whichever
    /// are visible. A hidden column lets the other span full width. (EVENTS/s is a
    /// separate full-width band drawn by `draw`, not part of this grid.)
    fn draw_main(&mut self, f: &mut Frame, area: Rect) {
        let right: Vec<Pane> = [Pane::File, Pane::Commands, Pane::Network]
            .into_iter().filter(|p| self.is_visible(*p)).collect();
        let process_vis = self.is_visible(Pane::Process);

        let (proc_area, right_area) = if process_vis && !right.is_empty() {
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
        if right.is_empty() { return; }

        let n = right.len() as u32;
        let constraints: Vec<Constraint> = (0..n).map(|_| Constraint::Ratio(1, n)).collect();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(a);
        for (i, pane) in right.iter().enumerate() {
            match pane {
                Pane::File => self.draw_files(f, chunks[i]),
                Pane::Commands => self.draw_commands(f, chunks[i]),
                Pane::Network => self.draw_network(f, chunks[i]),
                Pane::Process | Pane::Events => {}
            }
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
            Span::styled("tracce ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
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

    /// The EVENTS/s band: a full-width vertical bar graph of events-per-second,
    /// `events_zoom` seconds per bar. When focused, Left/Right scrub a cyan cursor
    /// column (the title shows the value under it) and Up/Down zoom the time axis;
    /// otherwise the title shows the live rate. The visible window is anchored to
    /// "now" and pans left to keep the cursor in view.
    fn draw_events(&self, f: &mut Frame, area: Rect, focused: bool) {
        // borders().inner() without needing the title built first.
        let inner = Rect {
            x: area.x + 1,
            y: area.y + 1,
            width: area.width.saturating_sub(2),
            height: area.height.saturating_sub(2),
        };
        if inner.width < 6 || inner.height < 2 { return; }

        let z = self.events_zoom.max(1);
        let series = self.zoomed_series();
        let n = series.len();
        let cursor_age = self.list_state[pane_index(Pane::Events)].selected();
        // Cursor position in series coordinates (oldest = 0). None → pin to now.
        let cursor_pos = cursor_age.map(|age| n.saturating_sub(1 + age));

        // Layout: a 4-wide y-label gutter, then a 1-col y-axis, then the bars.
        // The bottom two inner rows are the x-axis baseline and its time labels.
        const GUTTER: u16 = 5;
        if inner.height < 4 { return; }
        let plot_rows = inner.height - 2;
        let bars_x = inner.x + GUTTER;
        let bars_w = inner.width.saturating_sub(GUTTER) as usize;

        // Visible window of `bars_w` buckets, anchored right (now), panned left
        // only far enough to keep the cursor visible.
        let mut lo = n.saturating_sub(bars_w);
        if let Some(cp) = cursor_pos {
            if cp < lo { lo = cp; }
        }
        let win = &series[lo..n.min(lo + bars_w)];
        let peak = win.iter().copied().max().unwrap_or(0).max(1);

        // Title: live rate while following, or the value under the cursor; + peak.
        let readout = match cursor_age {
            Some(age) => {
                let v = cursor_pos.and_then(|cp| series.get(cp)).copied().unwrap_or(0);
                let when = if age == 0 { "now".to_string() } else { format!("{}s ago", age * z) };
                format!("cursor {v}/s · {when} ")
            }
            None => format!("{}/s ", self.current_rate()),
        };
        let title = Line::from(vec![
            Span::styled(
                " 5 EVENTS/s ",
                if focused { Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD) }
                else { Style::default().fg(Color::Gray) },
            ),
            Span::styled(readout, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::styled(format!("· {z}s/bar · peak {peak} "), Style::default().fg(Color::White)),
        ]);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(if focused {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::DarkGray)
            })
            .title(title);
        f.render_widget(block, area);

        // Right-align the bars so the newest bar sits under "now"; until the
        // history fills the window, the empty columns are on the left.
        let draw_offset = bars_w.saturating_sub(win.len()) as u16;

        // Coordinate frame: y-axis column at the gutter edge, x-axis baseline on
        // the row below the bars, time labels on the last inner row.
        let axis_x = bars_x - 1;
        let baseline_y = inner.y + plot_rows;
        let xlabel_y = baseline_y + 1;
        let right_edge = inner.x + inner.width;
        let newest_x = bars_x + draw_offset + win.len().saturating_sub(1) as u16;
        let dim = Style::default().fg(Color::DarkGray);
        // Structural axis lines (y-axis, baseline, ticks) read as white so the
        // chart frame is clearly visible; the interior gridlines and the numeric
        // labels stay dim/faint.
        let axis = Style::default().fg(Color::White);
        let last = plot_rows.saturating_sub(1).max(1);

        let buf = f.buffer_mut();

        // Y-axis line + faint gridlines at the interior ticks, drawn BEFORE the
        // bars so the bars paint over them.
        for b in 0..plot_rows {
            buf[(axis_x, inner.y + b)].set_symbol("│").set_style(axis);
        }
        // Four ticks from 0 (bottom) to peak (top), at integer rows.
        let tick_rows: [(u16, u64); 4] = std::array::from_fn(|k| {
            let k = k as u16;
            let row_from_top = last - (k * last) / 3;
            (row_from_top, peak * k as u64 / 3)
        });
        for &(row_from_top, _) in &tick_rows {
            if row_from_top == 0 || row_from_top == last { continue; } // 0/peak need no gridline
            let gy = inner.y + row_from_top;
            for cx in bars_x..right_edge {
                let cell = &mut buf[(cx, gy)];
                if cell.symbol() == " " || cell.symbol().is_empty() {
                    cell.set_symbol("┈").set_style(dim);
                }
            }
        }

        // Bars: each column spans `z` seconds; height ∝ rate, sub-cell via eighths.
        for (ci, &v) in win.iter().enumerate() {
            let x = bars_x + draw_offset + ci as u16;
            if x >= right_edge { break; }
            let is_cursor = Some(lo + ci) == cursor_pos;
            let total_e = (v * plot_rows as u64 * 8 / peak) as i64;
            for b in 0..plot_rows {
                let cell_e = (total_e - b as i64 * 8).clamp(0, 8) as usize;
                let y = inner.y + (plot_rows - 1 - b);
                if is_cursor {
                    // Solid cyan column so the cursor is visible at any height.
                    buf[(x, y)]
                        .set_symbol(&BAR8[cell_e].to_string())
                        .set_style(Style::default().fg(Color::Black).bg(Color::Cyan));
                } else if cell_e > 0 {
                    buf[(x, y)]
                        .set_symbol(&BAR8[cell_e].to_string())
                        .set_style(Style::default().fg(Color::Green));
                }
            }
        }

        // Y-axis tick marks + value labels (right-aligned in the 4-col gutter).
        for &(row_from_top, val) in &tick_rows {
            let gy = inner.y + row_from_top;
            buf[(axis_x, gy)].set_symbol("├").set_style(axis);
            let label = format!("{val:>4}");
            for (i, ch) in label.chars().enumerate() {
                let lx = inner.x + i as u16;
                if lx < axis_x {
                    buf[(lx, gy)].set_symbol(&ch.to_string()).set_style(axis);
                }
            }
        }

        // X-axis baseline with the origin corner.
        buf[(axis_x, baseline_y)].set_symbol("└").set_style(axis);
        for cx in bars_x..right_edge {
            buf[(cx, baseline_y)].set_symbol("─").set_style(axis);
        }

        // X-axis time ticks: up to 4, anchored at "now" and stepping left. Each is
        // labeled with its age in seconds (bars-from-now × zoom), centered under
        // the tick and clamped so labels neither overflow nor overlap.
        let step = (newest_x.saturating_sub(bars_x) / 3).max(1);
        let mut occupied_left = right_edge;
        for k in 0..4u16 {
            let tx = newest_x.saturating_sub(step * k);
            if k > 0 && tx <= axis_x { break; }
            buf[(tx, baseline_y)].set_symbol("┴").set_style(axis);
            let age = (newest_x - tx) as usize * z;
            let label = if age == 0 { "now".to_string() } else { format!("-{age}s") };
            let len = label.chars().count() as u16;
            let mut sx = tx.saturating_sub(len / 2).max(inner.x);
            if sx + len > right_edge { sx = right_edge.saturating_sub(len); }
            if sx + len <= occupied_left {
                for (i, ch) in label.chars().enumerate() {
                    buf[(sx + i as u16, xlabel_y)].set_symbol(&ch.to_string()).set_style(axis);
                }
                occupied_left = sx;
            }
        }
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

    fn draw_processes(&mut self, f: &mut Frame, area: Rect) {
        // Per-row layout: "! PPPPP  <prefix><comm padded to fill>  EEEEEE"
        //                  2   5    2  variable      to flush      2  6
        const SEV_W: usize = 2; // severity glyph + separator
        const PID_W: usize = 5;
        const SEP: usize = 2;
        const EV_W: usize = 6;
        let total_w = area.width as usize;
        // Reserve room for the static columns; remainder is the comm budget.
        // We compute comm_at_depth_zero so the column header aligns with the
        // widest available comm — deeper rows just borrow from the comm budget.
        let base_comm_w = total_w
            .saturating_sub(SEV_W + PID_W + SEP + SEP + EV_W + 2 /* borders */)
            .max(8);

        let header = format!(
            "{:sev_w$}{:>w_pid$}  {:<w_comm$}  {:>w_ev$}",
            "", "PID", "COMMAND", "EV",
            sev_w = SEV_W, w_pid = PID_W, w_comm = base_comm_w, w_ev = EV_W,
        );
        let body = self.framed_pane(f, area, "PROCESS TREE", Pane::Process, header);

        let rows = self.filtered_proc_rows();
        let items: Vec<ListItem> = rows.iter()
            .map(|(pid, prefix)| {
                let p = &self.processes[pid];
                let prefix_cols = prefix.chars().count();
                let comm_w = base_comm_w.saturating_sub(prefix_cols).max(4);
                ListItem::new(format!(
                    "{} {:>w_pid$}  {}{:<w_comm$}  {:>w_ev$}",
                    severity_glyph(p.severity), p.pid, prefix, truncate(&p.comm, comm_w), p.event_count,
                    w_pid = PID_W, w_comm = comm_w, w_ev = EV_W,
                )).style(severity_style(p.severity))
            }).collect();
        let focused = self.focus == Some(Pane::Process);
        render_rows(f, body, items, &mut self.list_state[pane_index(Pane::Process)], focused, true);
    }

    fn draw_files(&mut self, f: &mut Frame, area: Rect) {
        let body = self.framed_pane(f, area, "ACTIVITY", Pane::File,
            format!("      {:>5} {:<8} {}", "PID", "COMM", "PATH / CMD"));
        // Prefix is "X G ! PPPPP CCCCCCCC " = 1+1+1+1+1+1+5+1+8+1 = 21 cols
        const PREFIX_COLS: usize = 21;
        let path_cols = (body.width as usize).saturating_sub(PREFIX_COLS).max(1);
        let items: Vec<ListItem> = self.filtered_files().into_iter()
            .map(|r| {
                let glyph = if r.sensitive { "⚠" } else { " " };
                let suffix = if r.coalesced { " (burst)" } else { "" };
                let style = if r.sensitive && r.severity.is_none() {
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
                } else { Style::default() };
                ListItem::new(Line::from(vec![
                    Span::raw(format!("{} {} {} {:>5} {:<8} ", r.op, glyph, severity_glyph(r.severity), r.pid, truncate(&r.comm, 8))),
                    Span::styled(format!("{}{}", truncate(&r.path.display().to_string(), path_cols), suffix), style),
                ])).style(severity_style(r.severity))
            }).collect();
        let focused = self.focus == Some(Pane::File);
        render_rows(f, body, items, &mut self.list_state[pane_index(Pane::File)], focused, false);
    }

    fn draw_commands(&mut self, f: &mut Frame, area: Rect) {
        let body = self.framed_pane(f, area, "COMMANDS", Pane::Commands,
            format!("  {:>5}  {}", "PID", "ARGV"));
        const PREFIX_COLS: usize = 9; // 1 severity glyph + 1 sep + 5 pid + 2 sep
        let argv_cols = (body.width as usize).saturating_sub(PREFIX_COLS).max(1);
        let items: Vec<ListItem> = self.filtered_commands().into_iter()
            .map(|c| ListItem::new(format!(
                "{} {:>5}  {}", severity_glyph(c.severity), c.pid, truncate(&c.argv, argv_cols),
            )).style(severity_style(c.severity)))
            .collect();
        let focused = self.focus == Some(Pane::Commands);
        render_rows(f, body, items, &mut self.list_state[pane_index(Pane::Commands)], focused, false);
    }

    fn draw_network(&mut self, f: &mut Frame, area: Rect) {
        // Host column flexes with width so the CONNS count is never clipped —
        // matters because NETWORK gets narrow when the EVENTS/s panel shares its
        // row.
        const CONNS_W: usize = 5;
        let host_w = (area.width as usize).saturating_sub(2 + CONNS_W + 2).max(6);
        let body = self.framed_pane(f, area, "NETWORK", Pane::Network,
            format!("{:<host_w$}  {:>CONNS_W$}", "HOST", "CONNS"));
        let items: Vec<ListItem> = self.filtered_network().into_iter()
            .map(|n| ListItem::new(format!(
                "{:<host_w$}  {:>CONNS_W$}", truncate(&n.host, host_w), n.conns,
            )))
            .collect();
        let focused = self.focus == Some(Pane::Network);
        render_rows(f, body, items, &mut self.list_state[pane_index(Pane::Network)], focused, false);
    }

    /// Render the rounded outer block + the column-header row for a focusable
    /// pane, returning the area below the header where the list should draw.
    fn framed_pane(&self, f: &mut Frame, area: Rect, name: &str, pane: Pane, header: String) -> Rect {
        framed_chrome(f, area, self.title(name, pane), self.focus == Some(pane), header)
    }

    /// Pane title: " <n> NAME " plus a " [filter] " suffix when one is active.
    /// The leading digit doubles as the show/hide hotkey hint.
    fn title(&self, name: &str, pane: Pane) -> Line<'_> {
        let focused = self.focus == Some(pane);
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
                Some(Pane::Process) => "process",
                Some(Pane::File) => "activity",
                Some(Pane::Commands) => "commands",
                Some(Pane::Network) => "network",
                Some(Pane::Events) | None => "",
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
            ("Tab", "focus"), ("1-5", "show"), ("↵", "detail"), ("f", "follow"),
            ("/", "filter"), ("p", "pause"), ("e", "export"), ("s", "switch"),
            ("h", "help"), ("q", "quit"),
        ] {
            spans.push(Span::styled(format!(" {k} "), key_style));
            spans.push(Span::styled(format!(" {label}  "), label_style));
        }
        f.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn draw_help(&self, f: &mut Frame, area: Rect) {
        let rect = centered_fixed(58, 34, area);
        let block = modal_block(" keybindings ");
        let inner = block.inner(rect);
        f.render_widget(Clear, rect);
        f.render_widget(block, rect);

        let mut lines = logo_lines();
        lines.extend([
            Line::raw(""),
            help_group("Navigation"),
            help_kv("Tab / S-Tab", "cycle focus (incl. follow-all)"),
            help_kv("1 2 3 4 5", "show / hide panes & events"),
            help_kv("←↓↑→  j k", "move selection (rows)"),
            help_kv("← →  ·  ↑ ↓", "chart: scrub cursor · zoom time axis"),
            help_kv("g / G  ·  f", "follow latest / oldest · follow"),
            help_kv("Enter", "open detail (Esc closes)"),
            Line::raw(""),
            help_group("Display"),
            help_kv("p", "pause / resume"),
            help_kv("/", "filter focused pane"),
            help_kv("e", "export this session to ./<id>.tracce.tgz"),
            help_kv("s", "switch session (back to the picker)"),
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
        let rect = centered_fixed(64, 22, area);
        // Roomier padding than the shared modal helper for a calmer quit prompt.
        let block = modal_block(" Quit tracce? ").padding(Padding::new(4, 4, 2, 2));
        let inner = block.inner(rect);
        f.render_widget(Clear, rect);
        f.render_widget(block, rect);

        // Extra blank lines above and below the logo give it room to breathe.
        let mut lines = vec![Line::raw(""), Line::raw("")];
        lines.extend(logo_lines());
        lines.extend([
            Line::raw(""),
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

    /// The export flash: a small centered modal showing where the bundle landed
    /// (or why it failed). Any key dismisses it back to the dashboard.
    fn draw_flash(&self, f: &mut Frame, area: Rect) {
        let rect = centered_fixed(72, 9, area);
        let block = modal_block(" export ");
        let inner = block.inner(rect);
        f.render_widget(Clear, rect);
        f.render_widget(block, rect);
        let msg = self.flash.clone().unwrap_or_default();
        let lines = vec![
            Line::raw(""),
            Line::from(Span::raw(msg)).alignment(Alignment::Center),
            Line::raw(""),
            Line::from(Span::styled("press any key", Style::default().fg(Color::DarkGray)))
                .alignment(Alignment::Center),
        ];
        f.render_widget(Paragraph::new(lines), inner);
    }

    /// The row-detail modal: a label/value list of the frozen snapshot, framed
    /// like help/quit (no logo — it's content-focused). Esc/Enter/q close it.
    ///
    /// Long values are wrapped by hand and continuation lines are indented to the
    /// value column, so a wrapped path/argv stays under itself rather than
    /// sliding back under the label.
    fn draw_detail(&self, f: &mut Frame, area: Rect) {
        let Some(dv) = &self.detail else { return };
        const MODAL_W: u16 = 66;
        const LABEL_W: usize = 13;
        // Inner content width once borders (2) and padding (2 each side) are removed.
        let value_w = (MODAL_W as usize).saturating_sub(2 + 4 + LABEL_W).max(8);

        let mut lines: Vec<Line> = Vec::new();
        for (k, v) in &dv.rows {
            let wrapped = wrap_text(v, value_w);
            for (i, chunk) in wrapped.iter().enumerate() {
                let label = if i == 0 { format!("{k:<LABEL_W$}") } else { " ".repeat(LABEL_W) };
                lines.push(Line::from(vec![
                    Span::styled(label, Style::default().fg(Color::Gray)),
                    Span::raw(chunk.clone()),
                ]));
            }
        }
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            key_cap("Esc"), Span::styled(" close", Style::default().fg(Color::DarkGray)),
        ]).alignment(Alignment::Center));

        // Size to the wrapped line count, plus padding (2) and borders (2).
        let h = lines.len() as u16 + 4;
        let rect = centered_fixed(MODAL_W, h, area);
        let block = modal_block(&dv.title);
        let inner = block.inner(rect);
        f.render_widget(Clear, rect);
        f.render_widget(block, rect);
        f.render_widget(Paragraph::new(lines), inner);
    }
}

fn pane_index(p: Pane) -> usize {
    match p {
        Pane::Process => 0,
        Pane::File => 1,
        Pane::Commands => 2,
        Pane::Network => 3,
        Pane::Events => 4,
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

/// Render a pane's rows into `area` with the shared selection highlight. In
/// follow mode (`selected == None`) the viewport is pinned to the newest rows;
/// in browse mode `ListState` scrolls to the highlighted row.
///
/// `follow_bottom` picks which end is "newest": most panes insert the newest row
/// at the front, so following pins to the top (offset 0). The process tree is
/// pid-sorted (root at the top, newest spawns at the bottom), so it follows the
/// bottom instead — otherwise new processes fall below the fold and the pane
/// looks frozen once the tree outgrows its height.
fn render_rows(f: &mut Frame, area: Rect, items: Vec<ListItem>, state: &mut ListState, focused: bool, follow_bottom: bool) {
    if state.selected().is_none() {
        *state.offset_mut() = if follow_bottom {
            items.len().saturating_sub(area.height as usize)
        } else {
            0
        };
    }
    // The focused pane gets the bright cyan selection bar; a pane that retains a
    // selection after focus moves away gets a muted grey bar instead, so it's
    // clear which pane an Enter would act on.
    let highlight = if focused {
        Style::default().bg(Color::Cyan).fg(Color::Black).add_modifier(Modifier::BOLD)
    } else {
        Style::default().bg(Color::DarkGray)
    };
    let list = List::new(items).highlight_style(highlight);
    f.render_stateful_widget(list, area, state);
}

/// Full word for an ACTIVITY op glyph, for the detail modal.
fn op_name(op: char) -> &'static str {
    match op {
        'R' => "read", 'W' => "write", 'C' => "create", 'X' => "close",
        'D' => "delete", 'M' => "move", 'E' => "edit", 'A' => "multi-edit",
        '$' => "bash", _ => "?",
    }
}

/// Glyph for a row's user-flag severity: none, warning, or critical.
fn severity_glyph(sev: Option<Severity>) -> &'static str {
    match sev {
        Some(Severity::Critical) => "‼",
        Some(Severity::Warning) => "!",
        None => " ",
    }
}

/// Background tint for a row's user-flag severity, applied as the whole
/// ListItem's style so unselected flagged rows get a colored background band.
/// (The selection highlight always wins over this on the selected row — see
/// the flagged-commands design doc.)
fn severity_style(sev: Option<Severity>) -> Style {
    match sev {
        Some(Severity::Critical) => Style::default().bg(Color::Red).fg(Color::White),
        Some(Severity::Warning) => Style::default().bg(Color::Yellow).fg(Color::Black),
        None => Style::default(),
    }
}

/// Human-readable severity for the detail modal.
fn severity_label(sev: Option<Severity>) -> String {
    match sev {
        Some(Severity::Critical) => "‼ critical".into(),
        Some(Severity::Warning) => "! warning".into(),
        None => "none".into(),
    }
}

/// Dim every cell in `area` so a modal reads as a focused overlay. Runs after
/// the main UI is drawn but before the modal, which then overwrites (un-dims)
/// its own footprint via `Clear`.
fn dim_backdrop(f: &mut Frame, area: Rect) {
    let buf = f.buffer_mut();
    // Only add DIM — keep each cell's own colors so the dashboard just darkens
    // rather than turning a flat near-black.
    let dim = Style::default().add_modifier(Modifier::DIM);
    // Reversed cells (the pane column-header bars) are the exception: layering
    // DIM on faint+reverse+default-colors renders as a solid black bar on most
    // terminals. Resolve those to a concrete dim-gray bar so they read as
    // "dimmed" like everything else instead of blacking out.
    let dim_reversed = Style::default()
        .fg(Color::Black)
        .bg(Color::DarkGray)
        .remove_modifier(Modifier::REVERSED);
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let cell = &mut buf[(x, y)];
            if cell.modifier.contains(Modifier::REVERSED) {
                cell.set_style(dim_reversed);
            } else if cell.fg == Color::DarkGray {
                // Already the dim color (e.g. unfocused pane borders). Layering
                // DIM on top crushes it to near-black, so leave it as-is.
            } else {
                cell.set_style(dim);
            }
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

/// The tracce wordmark as centered, cyan-bold lines for modal headers.
pub(crate) fn logo_lines() -> Vec<Line<'static>> {
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
pub(crate) fn key_cap(k: &str) -> Span<'static> {
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

/// Wrap `s` to `width` columns, breaking on spaces where possible and
/// hard-splitting any single token longer than `width` (e.g. a long path).
/// Returns at least one line.
fn wrap_text(s: &str, width: usize) -> Vec<String> {
    if width == 0 { return vec![s.to_string()]; }
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let push_token = |lines: &mut Vec<String>, cur: &mut String, token: &str| {
        // Hard-split a token that can't fit on its own line.
        let mut rest = token;
        while rest.chars().count() > width {
            let head: String = rest.chars().take(width).collect();
            if !cur.is_empty() { lines.push(std::mem::take(cur)); }
            lines.push(head);
            rest = &rest[rest.char_indices().nth(width).map(|(i, _)| i).unwrap_or(rest.len())..];
        }
        if cur.is_empty() {
            cur.push_str(rest);
        } else if cur.chars().count() + 1 + rest.chars().count() <= width {
            cur.push(' ');
            cur.push_str(rest);
        } else {
            lines.push(std::mem::take(cur));
            cur.push_str(rest);
        }
    };
    for token in s.split(' ') {
        push_token(&mut lines, &mut cur, token);
    }
    if !cur.is_empty() || lines.is_empty() { lines.push(cur); }
    lines
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
        test_app_with(FlagConfig::empty())
    }

    fn test_app_with(flags: FlagConfig) -> App {
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
            tracce_version: "0".into(),
        };
        let entry = SessionEntry {
            dir: PathBuf::from("/tmp"),
            meta,
            status: "replay".into(),
            events_path: PathBuf::from("/tmp/events.jsonl"),
        };
        App::new(entry, flags)
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
    fn s_requests_switch_without_confirm() {
        let mut app = test_app();
        app.handle_key(key('s'));
        assert!(app.switch);
        assert!(!app.quit);
        assert_eq!(app.mode, Mode::Normal); // no confirm modal — non-destructive
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
        app.toggle_visible(Pane::Network);
        // Only Events left; hiding it must be refused.
        app.toggle_visible(Pane::Events);
        assert!(app.is_visible(Pane::Events));
        assert_eq!(app.visible.iter().filter(|v| **v).count(), 1);
    }

    #[test]
    fn default_focus_is_follow_all() {
        let app = test_app();
        assert_eq!(app.focus, None);
    }

    #[test]
    fn process_tree_follows_newest_at_bottom() {
        // A wide tree (root pid 1, children 2..=30) far taller than the pane.
        // The tree is pid-sorted, so the newest process is the last row. In
        // follow mode the pane must scroll to keep it in view, not pin the root.
        let mut app = test_app();
        for pid in 1u32..=30 {
            app.processes.insert(pid, ProcInfo {
                pid,
                comm: format!("C{pid:02}X"),
                ppid: if pid == 1 { 0 } else { 1 },
                event_count: 0,
                last_ts_ns: 0,
                severity: None,
            });
        }
        assert!(app.focus.is_none(), "test relies on follow-all (no selection)");

        let backend = ratatui::backend::TestBackend::new(40, 10);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| app.draw_processes(f, f.area())).unwrap();
        let text: String = term.backend().buffer().content()
            .iter().map(|c| c.symbol()).collect();

        assert!(text.contains("C30X"), "newest process (bottom of tree) must be visible");
        assert!(!text.contains("C01X"), "root must scroll out of view when following the newest");
    }

    #[test]
    fn hiding_focused_pane_drops_to_follow_all() {
        let mut app = test_app();
        app.focus = Some(Pane::Process);
        app.toggle_visible(Pane::Process);
        assert!(!app.is_visible(Pane::Process));
        assert_eq!(app.focus, None);
    }

    #[test]
    fn tab_cycles_through_empty_slot_and_skips_hidden() {
        let mut app = test_app();
        app.toggle_visible(Pane::File);     // hide pane 2
        app.toggle_visible(Pane::Commands); // hide pane 3
        app.toggle_visible(Pane::Events);   // hide pane 5
        // Visible: Process, Network. Ring: (none) → Process → Network → (none).
        assert_eq!(app.focus, None);
        app.handle_key(code(KeyCode::Tab));
        assert_eq!(app.focus, Some(Pane::Process));
        app.handle_key(code(KeyCode::Tab));
        assert_eq!(app.focus, Some(Pane::Network));
        app.handle_key(code(KeyCode::Tab));
        assert_eq!(app.focus, None); // back to follow-all
    }

    #[test]
    fn back_tab_from_empty_lands_on_last_pane() {
        let mut app = test_app();
        // All panes visible; ring ends with Events.
        app.handle_key(code(KeyCode::BackTab));
        assert_eq!(app.focus, Some(Pane::Events));
    }

    #[test]
    fn f_returns_focused_pane_to_follow() {
        let mut app = test_app();
        app.focus = Some(Pane::Commands);
        app.commands = vec![CommandRow { pid: 1, argv: "a".into(), ts_ns: 0, severity: None }];
        app.handle_key(code(KeyCode::Down));
        assert_eq!(app.focus_selected(), Some(0));
        app.handle_key(key('f'));
        assert_eq!(app.focus_selected(), None);
    }

    #[test]
    fn filter_commit_and_esc_clears() {
        let mut app = test_app();
        app.focus = Some(Pane::Commands);
        app.commands = vec![
            CommandRow { pid: 1, argv: "rg foo".into(), ts_ns: 0, severity: None },
            CommandRow { pid: 2, argv: "ls -la".into(), ts_ns: 0, severity: None },
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
    fn key_5_toggles_events_band() {
        let mut app = test_app();
        assert!(app.is_visible(Pane::Events));
        app.handle_key(key('5'));
        assert!(!app.is_visible(Pane::Events));
        app.handle_key(key('5'));
        assert!(app.is_visible(Pane::Events));
    }

    #[test]
    fn events_chart_scrubs_and_opens_detail() {
        let mut app = test_app();
        app.first_event_ns = Some(0);
        app.last_event_ns = 3_000_000_000;
        app.rate_history.extend([1u64, 4, 2]); // + current_bucket => series len 4
        app.focus = Some(Pane::Events);
        assert_eq!(app.focus_selected(), None); // follow: pinned to now
        app.handle_key(code(KeyCode::Left)); // scrub one step back
        assert_eq!(app.focus_selected(), Some(0));
        app.handle_key(code(KeyCode::Left));
        assert_eq!(app.focus_selected(), Some(1));
        // Enter builds a detail for the cursor's bucket.
        app.handle_key(code(KeyCode::Enter));
        assert_eq!(app.mode, Mode::Detail);
        assert!(app.detail.is_some());
    }

    #[test]
    fn events_up_down_zoom_the_time_axis() {
        let mut app = test_app();
        app.rate_history.extend([1u64, 4, 2]); // + current_bucket(0) => series [1,4,2,0]
        app.focus = Some(Pane::Events);
        assert_eq!(app.events_zoom, 1);

        // Down zooms out through ZOOM_LEVELS; Up zooms back in. Up at level 1 is a
        // no-op (clamped), never an accidental scrub.
        app.handle_key(code(KeyCode::Up));
        assert_eq!(app.events_zoom, 1);
        app.handle_key(code(KeyCode::Down));
        assert_eq!(app.events_zoom, 2);

        // At 2s/bar the 4-second series collapses to two bars (rounded means,
        // grouped from "now": [2,0]->1, [1,4]->3) and the cursor range halves.
        assert_eq!(app.zoomed_series(), vec![3, 1]);
        assert_eq!(app.zoomed_len(), 2);

        app.handle_key(code(KeyCode::Up));
        assert_eq!(app.events_zoom, 1);
        assert_eq!(app.zoomed_series(), vec![1, 4, 2, 0]);
    }

    #[test]
    fn zoom_out_reclamps_the_scrub_cursor() {
        let mut app = test_app();
        app.rate_history.extend([1u64, 4, 2]); // series len 4
        app.focus = Some(Pane::Events);
        app.handle_key(code(KeyCode::End)); // jump to oldest bar (idx 3 at 1s/bar)
        assert_eq!(app.focus_selected(), Some(3));
        app.handle_key(code(KeyCode::Down)); // 2s/bar => only 2 bars now
        assert_eq!(app.events_zoom, 2);
        assert_eq!(app.focus_selected(), Some(1)); // clamped into range
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
    fn exec_command_matching_critical_pattern_is_flagged() {
        use crate::event::{Event, EventData, EventKind, ProcessRef};
        use std::sync::Arc;

        let cfg = crate::flags::build(vec!["*sudo*".to_string()], vec![]);
        let mut app = test_app_with(cfg);
        let ev = Event {
            ts_ns: 1,
            kind: EventKind::Exec,
            pid: 55,
            ppid: 1,
            process: Arc::new(ProcessRef {
                pid: 55,
                comm: "sudo".into(),
                image: PathBuf::from("/usr/bin/sudo"),
                argv: vec!["/usr/bin/sudo".into(), "rm".into(), "-rf".into(), "/tmp/x".into()],
            }),
            data: EventData::Exec {
                argv: vec!["/usr/bin/sudo".into(), "rm".into(), "-rf".into(), "/tmp/x".into()],
                image: PathBuf::from("/usr/bin/sudo"),
            },
            flags: 0,
        };
        app.ingest(ev);
        assert_eq!(app.commands[0].severity, Some(Severity::Critical));
        assert_eq!(app.processes[&55].severity, Some(Severity::Critical));
    }

    #[test]
    fn file_event_matching_warning_pattern_is_flagged() {
        use crate::event::{Event, EventData, EventKind, FileOp, ProcessRef};
        use std::sync::Arc;

        let cfg = crate::flags::build(vec![], vec!["*.env*".to_string()]);
        let mut app = test_app_with(cfg);
        let ev = Event {
            ts_ns: 1,
            kind: EventKind::Open,
            pid: 9,
            ppid: 1,
            process: Arc::new(ProcessRef {
                pid: 9, comm: "cat".into(), image: PathBuf::from("/bin/cat"), argv: vec![],
            }),
            data: EventData::File { op: FileOp::Open, path: PathBuf::from("/x/y/.env"), size: None },
            flags: 0,
        };
        app.ingest(ev);
        assert_eq!(app.recent_files[0].severity, Some(Severity::Warning));
    }

    #[test]
    fn unflagged_events_have_no_severity() {
        use crate::event::{Event, EventData, EventKind, ProcessRef};
        use std::sync::Arc;

        let mut app = test_app(); // FlagConfig::empty()
        let ev = Event {
            ts_ns: 1, kind: EventKind::Exec, pid: 1, ppid: 0,
            process: Arc::new(ProcessRef {
                pid: 1, comm: "echo".into(), image: PathBuf::from("/bin/echo"), argv: vec!["/bin/echo".into()],
            }),
            data: EventData::Exec { argv: vec!["/bin/echo".into()], image: PathBuf::from("/bin/echo") },
            flags: 0,
        };
        app.ingest(ev);
        assert_eq!(app.commands[0].severity, None);
        assert_eq!(app.processes[&1].severity, None);
    }

    #[test]
    fn wrap_text_breaks_on_spaces_and_hard_splits_long_tokens() {
        assert_eq!(wrap_text("a b c", 10), vec!["a b c"]);
        assert_eq!(wrap_text("hello world foo", 5), vec!["hello", "world", "foo"]);
        // A long token with no spaces is hard-split.
        assert_eq!(wrap_text("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        assert!(wrap_text("", 5) == vec![""]);
    }

    #[test]
    fn down_enters_browse_and_up_off_top_returns_to_follow() {
        let mut app = test_app();
        app.focus = Some(Pane::Commands);
        app.commands = vec![
            CommandRow { pid: 1, argv: "a".into(), ts_ns: 0, severity: None },
            CommandRow { pid: 2, argv: "b".into(), ts_ns: 0, severity: None },
            CommandRow { pid: 3, argv: "c".into(), ts_ns: 0, severity: None },
        ];
        assert_eq!(app.focus_selected(), None); // follow: no highlight
        app.handle_key(code(KeyCode::Down));
        assert_eq!(app.focus_selected(), Some(0));
        app.handle_key(code(KeyCode::Down));
        assert_eq!(app.focus_selected(), Some(1));
        app.handle_key(code(KeyCode::Up));
        assert_eq!(app.focus_selected(), Some(0));
        app.handle_key(code(KeyCode::Up)); // off the top → back to follow
        assert_eq!(app.focus_selected(), None);
    }

    #[test]
    fn enter_opens_detail_and_esc_closes() {
        let mut app = test_app();
        app.focus = Some(Pane::Commands);
        app.commands = vec![CommandRow { pid: 7, argv: "rg foo".into(), ts_ns: 0, severity: None }];
        app.handle_key(code(KeyCode::Enter));
        assert_eq!(app.mode, Mode::Detail);
        assert!(app.detail.is_some());
        app.handle_key(code(KeyCode::Esc));
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.detail.is_none());
    }

    #[test]
    fn enter_on_empty_pane_is_noop() {
        let mut app = test_app();
        app.focus = Some(Pane::Commands); // no rows
        app.handle_key(code(KeyCode::Enter));
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.detail.is_none());
    }

    #[test]
    fn selection_sticks_to_row_as_new_events_arrive() {
        use crate::event::{Event, EventData, EventKind, ProcessRef};
        use std::sync::Arc;
        let mut app = test_app();
        app.focus = Some(Pane::Commands);
        app.commands = vec![
            CommandRow { pid: 1, argv: "old1".into(), ts_ns: 0, severity: None },
            CommandRow { pid: 2, argv: "old2".into(), ts_ns: 0, severity: None },
        ];
        app.handle_key(code(KeyCode::Down));
        app.handle_key(code(KeyCode::Down)); // highlight "old2" at index 1
        assert_eq!(app.focus_selected(), Some(1));
        // A new command lands at the front; the highlight follows its logical row.
        let ev = Event {
            ts_ns: 5, kind: EventKind::Exec, pid: 9, ppid: 1,
            process: Arc::new(ProcessRef {
                pid: 9, comm: "new".into(), image: PathBuf::from("/bin/new"),
                argv: vec!["/bin/new".into()],
            }),
            data: EventData::Exec { argv: vec!["/bin/new".into()], image: PathBuf::from("/bin/new") },
            flags: 0,
        };
        app.ingest(ev);
        assert_eq!(app.focus_selected(), Some(2));
    }

    #[test]
    fn filter_esc_cancels_without_committing() {
        let mut app = test_app();
        app.focus = Some(Pane::Commands);
        app.handle_key(key('/'));
        app.handle_key(key('x'));
        app.handle_key(code(KeyCode::Esc)); // cancel input
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.pane_filter(Pane::Commands).is_empty());
    }

    #[test]
    fn e_key_exports_focused_session_and_flashes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let id = "2026-05-28T22-04-31_demo_4711";
        let dir = tmp.path().join("sessions").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("events.jsonl"), "{}\n").unwrap();
        std::fs::write(dir.join("status"), "done\n").unwrap();
        std::fs::write(dir.join("meta.json"), format!(r#"{{"session_id":"{id}","started_at":"2026-05-28T22:04:31Z","ended_at":null,"cwd":"/tmp/demo","argv":["claude"],"claude_pid":1,"tracer_pid":2,"hostname":"h","macos_version":"15","tracce_version":"0.1"}}"#)).unwrap();
        let entry = crate::view::discovery::entry_for_dir(&dir).unwrap();

        let mut app = App::new(entry, FlagConfig::empty());
        // Export writes the default ./<id>.tracce.tgz into cwd; point cwd at tmp
        // so the artifact lands there and is cleaned up with the TempDir. cwd is
        // process-global and tests run in parallel, so a CwdGuard restores it
        // even if the assertions below panic, and a mutex serializes the swap.
        {
            let _lock = CWD_LOCK.lock().unwrap_or_else(|p| p.into_inner());
            let _cwd = CwdGuard::enter(tmp.path());
            app.handle_key(key('e'));

            assert_eq!(app.mode, Mode::ExportFlash);
            let msg = app.flash.clone().expect("flash message set");
            assert!(msg.contains("exported"), "got: {msg}");
            assert!(tmp.path().join(format!("{id}.tracce.tgz")).exists());
        }
        // Any key dismisses.
        app.handle_key(code(KeyCode::Esc));
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.flash.is_none());
    }

    /// Serializes the process-global cwd swap across parallel tests.
    static CWD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Restores the previous working directory on drop, even on panic.
    struct CwdGuard(PathBuf);
    impl CwdGuard {
        fn enter(dir: &std::path::Path) -> Self {
            let prev = std::env::current_dir().unwrap();
            std::env::set_current_dir(dir).unwrap();
            CwdGuard(prev)
        }
    }
    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.0);
        }
    }
}
