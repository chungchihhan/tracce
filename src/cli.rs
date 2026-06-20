use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "tracce",
    version,
    about = "macOS tracer for Claude Code sessions",
    long_about = "Trace what a Claude Code session does on your machine.\n\n\
                  Run `tracce` (or `tracce claude …`) to launch and record claude.\n\
                  Run `tracce attach` in a second terminal to watch a running\n\
                  claude live. Run `tracce view` to replay a recorded session."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Cmd>,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Launch and record a Claude Code session (this is the default when no
    /// subcommand is given). claude owns the terminal, so no TUI is drawn —
    /// replay the recording later with `tracce view`.
    Claude {
        /// Arguments forwarded verbatim to `claude`.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Attach to an already-running claude: record AND render the live TUI in
    /// this terminal. The only mode that shows a live dashboard.
    Attach {
        /// PID of the running claude. Omit to auto-find the running claude
        /// (or pick from a list if several are running).
        pid: Option<u32>,
    },
    /// Launch and record an arbitrary command — a testing hatch for exercising
    /// tracce's features without claude in the loop. e.g. `tracce exec -- npm test`.
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
