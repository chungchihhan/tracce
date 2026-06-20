# ctrace

macOS-only kernel-event tracer for Claude Code sessions. Either launch claude
under ctrace and replay the recording later, or `ctrace attach` onto a
claude that's already running and watch it live in a second terminal.

> Personal/audit tool — requires Full Disk Access for your terminal app.
> ctrace itself runs as you; it uses `sudo` only to launch `eslogger`.
> eslogger is the default event source — ctrace prompts for sudo on start and
> degrades to poll-only (no file events) if you decline.

## Install

```
git clone <this repo>
cd ctrace
cargo build --release
sudo cp target/release/ctrace /usr/local/bin/
```

## Usage

### Watch a running claude, live (recommended)

```
# Terminal A — start claude however you normally do
claude

# Terminal B — attach and render the live dashboard.
# sudo prompts once to start eslogger. With no pid, ctrace finds the running
# claude (or lets you pick if several are running).
ctrace attach
ctrace attach <pid>
```

`attach` records to disk *and* draws the live TUI, since claude is in its own
terminal. It's the only mode that shows a live dashboard.

### Launch + record (replay later)

```
# claude owns this terminal, so no TUI is drawn — the run is recorded to disk.
ctrace                              # trace `claude` with no extra args
ctrace claude --print "explain this repo"

ctrace exec -- npm test             # testing hatch: trace any command

# Replay a recorded session in the TUI:
ctrace view                         # picker
ctrace view --latest
ctrace view <session-id-prefix>
ctrace list
```

If a trace crashes and leaves files in an odd state, run:

```
ctrace fix-perms
```

## The dashboard

`attach` and `view` render a live grid of panels:

- **PROCESS TREE** (`1`) — the descendant process tree by pid, with each command name and its event count
- **ACTIVITY** (`2`) — recent file operations (`R` read · `W` write · `C` create · `X` close · `D` delete · `M` move · `E` edit · `A` multi-edit · `$` bash), with a `⚠` on sensitive paths and `(burst)` on coalesced bursts
- **COMMANDS** (`3`) — exec'd command lines (full argv)
- **NETWORK** (`4`) — remote hosts and their connection counts
- **EVENTS/s** (`5`) — a full-width bar graph (bottom) of the events-per-second rate, with a labeled Y-axis (0 → peak, with gridlines) and an X-axis time scale (`−Ns` … `now`)

By default nothing is focused and every pane follows the latest events. `Tab` cycles focus through the panes and back to that follow-all state. In a focused row pane, the arrows (or `j`/`k`) move a highlighted selection and `Enter` opens a detail view (full path / argv / host, untruncated, plus when it happened). On the focused EVENTS/s graph, **Left/Right scrub** a cyan cursor along time and **Up/Down zoom the time axis** (1 → 2 → 5 → 10 → 30 → 60 seconds per bar). `f` returns the pane to following the latest; `Esc` closes the detail.

Keys: `Tab` / `Shift-Tab` cycle focus (incl. follow-all) · `1`–`5` show/hide panels · arrows or `j`/`k` move selection · on the graph ←/→ scrub and ↑/↓ zoom the time axis · `f` follow latest · `g`/`G` follow / jump to oldest · `Enter` detail · `/` filter the focused pane · `p` pause · `h` (or `?`) help · `q` / `Esc` quit · `Ctrl-C` quit immediately.

## How it works

- Process and file events come from `/usr/bin/eslogger` (macOS Endpoint Security), on by default
- claude's own tool calls (Edit/Write/Read, and the exact Bash command) are read from its session transcript and shown alongside the kernel events
- Network connections come from `lsof -i` polled every 500 ms
- Events are filtered to the descendant tree of the traced process
- Bursts (e.g. ripgrep) are coalesced in the live view; raw events still go to JSONL
- Sensitive paths (`.env`, `~/.aws`, `~/.ssh`, `*.pem`, etc.) get a `⚠` glyph
- If eslogger can't start (sudo declined), ctrace degrades to poll-only: process tree + network + claude tool calls, but no file open/write/delete events

## Security & trust

ctrace asks for `sudo` on start, which is a fair thing to be cautious about —
especially for a tool whose whole job is auditing what software does. Here's
exactly what that privilege buys and where it stops:

- **Only one thing runs as root:** `/usr/bin/eslogger`, Apple's own signed
  binary, with a fixed argument list:

  ```
  sudo /usr/bin/eslogger exec fork exit open close create write unlink rename
  ```

  Those are the kernel event types it subscribes to. macOS Endpoint Security is
  a privileged API, so reading these events requires root — there's no
  unprivileged path to them.

- **ctrace itself never runs as root.** Your ctrace process stays as you; it
  just reads the event stream that the root `eslogger` child writes to a pipe.

- **No user input reaches the privileged command.** The argument list is
  hardcoded, the paths are absolute, and there's no shell — nothing you type
  (PIDs, paths, anything) is ever interpolated into the command run as root.

- **No standing privilege.** `eslogger` is a transient child, killed when the
  trace ends. ctrace installs no daemon, no setuid binary, and makes no changes
  to your sudoers — every trace prompts (or reuses your normal sudo cache).

- **Read-only by design.** eslogger *observes*; it cannot block or modify
  anything. ctrace is not a sandbox (see below).

- **Auditable.** It's open source, and the entire sudo invocation lives in one
  function — `bring_up_eslogger` in `src/trace/run.rs`. Read it.

- **You can decline.** Say no to the prompt and ctrace degrades to poll-only
  (process tree + network + claude tool calls), with no file events.

## Limitations

- macOS only
- Network bytes are approximations (poll-based, not kernel-traced)
- Very short-lived connections (< 500 ms) may be missed
- `attach` captures from the attach moment forward — it can't replay what claude did before you attached
- Not a sandbox — ctrace only observes
