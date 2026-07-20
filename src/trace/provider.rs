use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::Path;

/// The agent or command at the root of a trace. The tracing and viewer
/// pipeline is shared across providers; this value selects launch,
/// discovery, and optional intent enrichment behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Claude,
    Codex,
    #[value(skip)]
    Other,
}

impl Default for Provider {
    /// Existing recordings predate provider metadata and were Claude-focused.
    fn default() -> Self { Self::Claude }
}

impl Provider {
    /// Infer a provider from the root executable in a legacy recording.
    ///
    /// Provider metadata was added after the first session format, so older
    /// recordings need the command line as a fallback. An unknown executable
    /// is an ordinary command rather than Claude.
    pub fn from_argv(argv: &[String]) -> Option<Self> {
        let executable = argv.first()?;
        let basename = Path::new(executable).file_name()?.to_str()?;
        Some(match basename {
            "claude" => Self::Claude,
            "codex" | "codex-cli" => Self::Codex,
            _ => Self::Other,
        })
    }

    pub fn command(self) -> Option<&'static str> {
        match self {
            Self::Claude => Some("claude"),
            Self::Codex => Some("codex"),
            Self::Other => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::Codex => "Codex",
            Self::Other => "Command",
        }
    }

    pub fn is_agent(self) -> bool {
        matches!(self, Self::Claude | Self::Codex)
    }
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}
