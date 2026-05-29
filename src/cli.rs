use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "peekaboo", version, about = "macOS tracer for Claude Code sessions")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Cmd,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Run a command under tracing (requires sudo).
    Trace {
        /// The command and its arguments to trace.
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        argv: Vec<String>,
    },
    /// Render a session in the TUI.
    View {
        /// Session id (directory name) or path to events.jsonl. Omit to use picker.
        target: Option<String>,
        /// Open the most recently started live session without showing a picker.
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
    if let Ok(p) = std::env::var("PEEKABOO_HOME") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").expect("HOME must be set");
    PathBuf::from(home).join(".peekaboo")
}
