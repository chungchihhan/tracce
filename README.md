<div align="center">

### `tracce` - See exactly what Claude Code and Codex do on your machine
<img width="1624" height="1061" alt="image" src="https://github.com/user-attachments/assets/747303a2-9927-418b-aa63-98db200f5cd1" />
<img width="1624" height="1061" alt="image" src="https://github.com/user-attachments/assets/3d194072-2048-467a-aa7f-87c53f5f0abf" />

A macOS kernel-event tracer for Claude Code and Codex sessions — every process,
file, and network connection, live in your terminal or replayed later.

![platform](https://img.shields.io/badge/platform-macOS%2013%2B-000000?logo=apple&logoColor=white)
![built with Rust](https://img.shields.io/badge/built%20with-Rust-CE412B?logo=rust&logoColor=white)
![install](https://img.shields.io/badge/install-Homebrew-FBB040?logo=homebrew&logoColor=white)
![license](https://img.shields.io/badge/license-MIT-blue)
![release](https://img.shields.io/github/v/release/chungchihhan/tracce?color=success&label=release)

</div>

<!-- Tip: drop a terminal recording here once you have one, e.g. ![demo](docs/demo.gif) -->

---

## Why tracce

- **Live process tree** — every descendant of the traced process, with per-process event counts
- **File activity** — opens, writes, creates, deletes, renames — with a `⚠` on sensitive paths (`.env`, `~/.ssh`, `*.pem`, …)
- **Agent intent, too** — reads supported Claude/Codex tool calls and commands from local session history, shown next to kernel truth when available
- **Network** — remote hosts and their connection counts
- **Events/s** — a live bar graph with a real time axis you can scrub and zoom
- **Record & replay** — every session is written to JSONL, so you can `tracce view` it later
- **Minimal privilege** — only Apple's signed `eslogger` runs as root, with a hardcoded argument list; tracce itself never does
- **Your own danger list** — maintain glob patterns in `~/.tracce/flags.json`; matching commands/paths get flagged yellow (warning) or red (critical)

> [!NOTE]
> Personal/audit tool, macOS only. Your terminal app needs **Full Disk Access**.
> tracce runs as you and uses `sudo` only to launch `eslogger` (the default event
> source). Decline the prompt and it degrades to poll-only — no file events.

## Quick start

```sh
brew install chungchihhan/tap/tracce

# Terminal A — start a traced agent
tracce claude # or: tracce codex

# Terminal B — watch it live (and replay anytime later)
tracce view
```

## Install

<details open>
<summary><b>Homebrew</b> (recommended)</summary>

```sh
brew install chungchihhan/tap/tracce
```

Builds from source via the tap, so it works on Apple Silicon and Intel with no
code-signing prompts. Homebrew pulls in the Rust toolchain automatically; you
just need the Xcode Command Line Tools. Upgrade with `brew upgrade tracce`.
</details>

<details>
<summary><b>cargo</b> (needs Rust)</summary>

```sh
cargo install --git https://github.com/chungchihhan/tracce
```
</details>

<details>
<summary><b>From source</b></summary>

```sh
git clone https://github.com/chungchihhan/tracce
cd tracce
cargo build --release
sudo cp target/release/tracce /usr/local/bin/
```
</details>

## Usage

The dashboard always runs in a second terminal — both Claude Code and Codex are
terminal-first interfaces and can't share one with the board. There are two
ways to get there.

**Launch Claude or Codex under tracce, watch with `view`** (recommended):

```sh
# Terminal A — start a traced Claude Code session (claude owns this terminal)
tracce claude
# or:
tracce codex

# Terminal B — choose a session from the picker
tracce view
```

**Attach to an agent that's already running:**

```sh
# Terminal B — sudo prompts once to start eslogger. With no pid, tracce finds
# a running Claude/Codex process (or lets you pick if several are running).
tracce attach
tracce attach <pid>
tracce attach --agent codex
```

**Replay & inspect recordings** — every run is saved, so `view` works after the fact too:

```sh
tracce view                            # always opens the picker to choose a session
tracce view --latest                   # skips the picker, opens the newest session (live or not)
tracce view <session-id-prefix>         # skips the picker, opens that session directly
tracce list                            # sessions as a text table
tracce exec -- npm test                # testing hatch: trace any command
```

**Share a session** — a session is a self-contained folder, so export bundles it
into one compressed file you can send; import drops it back in, ready to `view`:

```sh
tracce export <session-id-prefix>      # writes ./tracce-exports/<id>.tracce.tgz
tracce export --latest -o run.tracce.tgz
tracce import run.tracce.tgz           # unpacks into ~/.tracce, then: tracce view <id>
```

You can also press `e` in the dashboard or the session picker to export the
session you're looking at into `./tracce-exports/`.

In the session picker, press `d` to delete a completed or interrupted recording;
tracce asks for confirmation before permanently removing it.

If a trace is interrupted and leaves files in an odd state, run `tracce fix-perms`.

## The dashboard

`attach` and `view` render a live grid of panels:

| Pane | Key | Shows |
|---|:---:|---|
| **PROCESS TREE** | `1` | the descendant process tree by pid, with each command name and its event count |
| **ACTIVITY** | `2` | recent file ops (`R` read · `W` write · `C` create · `X` close · `D` delete · `M` move · `E` edit · `A` multi-edit · `$` bash), `⚠` sensitive · `(burst)` coalesced |
| **COMMANDS** | `3` | exec'd command lines (full argv) |
| **NETWORK** | `4` | remote hosts and connection counts |
| **EVENTS/s** | `5` | full-width bar graph of the events-per-second rate, with a labeled Y-axis (0 → peak) and an X-axis time scale (`−Ns` … `now`) |

By default nothing is focused and every pane follows the latest events. `Tab` cycles
focus through the panes and back to that follow-all state. In a focused row pane, the
arrows (or `j`/`k`) move a highlighted selection and `Enter` opens a detail view (full
path / argv / host, untruncated, plus when it happened). On the focused EVENTS/s graph,
**Left/Right scrub** a cyan cursor along time and **Up/Down zoom the time axis**
(1 → 2 → 5 → 10 → 30 → 60 seconds per bar).

### Keys

| Key | Action |
|---|---|
| `Tab` / `Shift-Tab` | cycle focus (incl. follow-all) |
| `1`–`5` | show / hide panels |
| `↑` `↓` or `j` `k` | move selection |
| `←` `→` *(graph)* | scrub the time cursor |
| `↑` `↓` *(graph)* | zoom the time axis |
| `Enter` | open row detail |
| `/` | filter the focused pane |
| `t` / `g` / `G` | tail latest / follow latest / oldest |
| `f` | show flagged rows in the focused pane |
| `p` | pause |
| `e` | export this session to `./tracce-exports/<id>.tracce.tgz` |
| `s` | switch session (back to the picker) |
| `h` or `?` | help |
| `q` / `Esc` | quit  ·  `Ctrl-C` quits immediately |

### Flagging your own commands & paths

Maintain your own list of glob patterns at `~/.tracce/flags.json` — any
command (argv) or file path matching a pattern gets colored in PROCESS TREE,
ACTIVITY, and COMMANDS: yellow for `warning`, red for `critical`. The file is
seeded with a small default set on first run and is yours to edit freely:

```json
{
  "critical": ["*rm -rf*", "*curl*|*sh*", "*sudo*", "*chmod 777*", "*mkfs*"],
  "warning": ["*.env*", "*git push --force*", "*eval*", "*npm publish*"]
}
```

Patterns are glob-style and match the *whole* argv/path string, so a
"contains this anywhere" pattern needs leading and trailing `*` (e.g. `*sudo*`
matches `sudo rm -rf /`, but `sudo*` would not). Edits take effect the next
time you open or switch to a session — not live mid-session.

## How it works

- Process and file events come from `/usr/bin/eslogger` (macOS Endpoint Security), on by default
- supported Claude/Codex tool calls are read from local session history and shown alongside the kernel events when that history is available
- Network connections come from `lsof -i` polled every 500 ms
- Events are filtered to the descendant tree of the traced process
- Bursts (e.g. ripgrep) are coalesced in the live view; raw events still go to JSONL
- Sensitive paths (`.env`, `~/.aws`, `~/.ssh`, `*.pem`, etc.) get a `⚠` glyph
- If eslogger can't start (sudo declined), tracce degrades to poll-only: process tree + network + agent tool calls, but no file open/write/delete events

## Security & trust

tracce asks for `sudo` on start, which is a fair thing to be cautious about —
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

- **tracce itself never runs as root.** Your tracce process stays as you; it
  just reads the event stream that the root `eslogger` child writes to a pipe.

- **No user input reaches the privileged command.** The argument list is
  hardcoded, the paths are absolute, and there's no shell — nothing you type
  (PIDs, paths, anything) is ever interpolated into the command run as root.

- **No standing privilege.** `eslogger` is a transient child, killed when the
  trace ends. tracce installs no daemon, no setuid binary, and makes no changes
  to your sudoers — every trace prompts (or reuses your normal sudo cache).

- **Read-only by design.** eslogger *observes*; it cannot block or modify
  anything. tracce is not a sandbox (see below).

- **Auditable.** It's open source, and the entire sudo invocation lives in one
  function — `bring_up_eslogger` in `src/trace/run.rs`. Read it.

- **You can decline.** Say no to the prompt and tracce degrades to poll-only
  (process tree + network + agent tool calls), with no file events.

## Limitations

- macOS only
- Network bytes are approximations (poll-based, not kernel-traced)
- Very short-lived connections (< 500 ms) may be missed
- `attach` captures from the attach moment forward — it can't replay what an agent did before you attached
- Not a sandbox — tracce only observes

## License

[MIT](LICENSE) © Chih-han Chung
