# peekaboo

macOS-only kernel-event tracer for Claude Code sessions. Wrap `claude` with
`peekaboo trace claude ...`, then watch what it's doing in a second
terminal with `peekaboo view`.

> Personal/audit tool — requires Full Disk Access for your terminal app.
> peekaboo itself runs as you; it uses `sudo` only to launch `eslogger`.

## Install

```
git clone <this repo>
cd peekaboo
cargo build --release
sudo cp target/release/peekaboo /usr/local/bin/
```

## Usage

```
# Terminal A — peekaboo runs as you; sudo will prompt for password to start eslogger.
peekaboo trace claude --print "explain this repo"

# Terminal B
peekaboo view
peekaboo view --latest
peekaboo view <session-id-prefix>
peekaboo list
```

If a `trace` crashes and leaves files in an odd state, run:

```
peekaboo fix-perms
```

## How it works

- Process and file events come from `/usr/bin/eslogger` (macOS Endpoint Security)
- Network connections come from `lsof -i` polled every 500 ms
- Events are filtered to the descendant tree of the wrapped command
- Bursts (e.g. ripgrep) are coalesced in the live view; raw events still go to JSONL
- Sensitive paths (`.env`, `~/.aws`, `~/.ssh`, `*.pem`, etc.) get a `⚠` glyph

## Limitations

- macOS only
- Network bytes are approximations (poll-based, not kernel-traced)
- Very short-lived connections (< 500 ms) may be missed
- Not a sandbox — peekaboo only observes
