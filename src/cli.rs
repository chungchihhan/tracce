use clap::{Parser, Subcommand};
use std::path::PathBuf;

use tracce::trace::provider::Provider;

#[derive(Parser, Debug)]
#[command(
    name = "tracce",
    version,
    about = "macOS tracer for Claude Code and Codex sessions",
    long_about = "Trace what a Claude Code or Codex session does on your machine.\n\n\
                  Run `tracce claude …` or `tracce codex …` to launch and record an agent.\n\
                  Run `tracce attach` in a second terminal to watch a running\n\
                  agent live. Run `tracce view` to replay a recorded session."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Cmd>,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Launch and record a Claude Code session. Claude owns the terminal, so no
    /// TUI is drawn — replay the recording later with `tracce view`.
    Claude {
        /// Arguments forwarded verbatim to `claude`.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Launch and record a Codex CLI session. Codex owns the terminal, so no
    /// TUI is drawn — replay the recording later with `tracce view`.
    Codex {
        /// Arguments forwarded verbatim to `codex`.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Attach to an already-running Claude or Codex: record AND render the live TUI in
    /// this terminal. The only mode that shows a live dashboard.
    Attach {
        /// Restrict auto-discovery to one supported agent.
        #[arg(long, value_enum)]
        agent: Option<Provider>,
        /// PID of the running agent. Omit to auto-find Claude/Codex (or pick
        /// from a list if several are running).
        pid: Option<u32>,
    },
    /// Launch and record an arbitrary command — a testing hatch for exercising
    /// tracce's features without an agent in the loop. e.g. `tracce exec -- npm test`.
    Exec {
        /// The command and its arguments to trace.
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        argv: Vec<String>,
    },
    /// Replay a recorded session in the TUI.
    View {
        /// Session id (directory name) or path to events.jsonl. Omit to use picker.
        target: Option<String>,
        /// Open the most recently started session without showing a picker.
        #[arg(long)]
        latest: bool,
        /// Do not tail the file; render a static snapshot.
        #[arg(long)]
        no_follow: bool,
    },
    /// List sessions as a plain text table.
    List,
    /// Export a recorded session to a single shareable `.tracce.tgz` archive.
    Export {
        /// Session id or id-prefix. Omit to use the picker (or the only/live session).
        target: Option<String>,
        /// Output path. Default: ./<session-id>.tracce.tgz
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Export the most recently started session without showing a picker.
        #[arg(long)]
        latest: bool,
    },
    /// Import a `.tracce.tgz` archive into your sessions, ready to `view`.
    Import {
        /// Path to a `.tracce.tgz` produced by `tracce export`.
        file: PathBuf,
        /// Overwrite an existing session with the same id.
        #[arg(long)]
        force: bool,
    },
    /// Reset ownership of session files left root-owned by a crashed trace.
    FixPerms {
        /// Specific session id to fix; omit to fix all.
        session: Option<String>,
    },
}

pub fn parse() -> Cli {
    Cli::parse()
}

#[allow(dead_code)]
pub fn root_dir() -> PathBuf {
    if let Ok(p) = std::env::var("TRACCE_HOME") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").expect("HOME must be set");
    PathBuf::from(home).join(".tracce")
}
