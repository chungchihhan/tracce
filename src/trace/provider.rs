use serde::{Deserialize, Serialize};
use std::fmt;

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
