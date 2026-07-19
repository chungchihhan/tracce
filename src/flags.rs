//! User-editable flag list: glob patterns matched against command argv and
//! file paths, classified as `Warning` (yellow) or `Critical` (red) in the
//! TUI. Distinct from `sensitive.rs`, which is a fixed, non-configurable
//! list of secret-adjacent paths.

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Warning,
    Critical,
}

pub struct FlagConfig {
    critical: GlobSet,
    warning: GlobSet,
}

#[derive(Debug, Serialize, Deserialize)]
struct RawFlagConfig {
    #[serde(default)]
    critical: Vec<String>,
    #[serde(default)]
    warning: Vec<String>,
}

const DEFAULT_CRITICAL: &[&str] = &["*rm -rf*", "*curl*|*sh*", "*sudo*", "*chmod 777*", "*mkfs*"];
const DEFAULT_WARNING: &[&str] = &["*.env*", "*git push --force*", "*eval*", "*npm publish*"];

impl FlagConfig {
    /// A config that matches nothing — the safe fallback when `flags.json`
    /// is missing/unreadable/invalid, and the default for tests.
    pub fn empty() -> Self {
        FlagConfig {
            critical: GlobSetBuilder::new().build().unwrap(),
            warning: GlobSetBuilder::new().build().unwrap(),
        }
    }

    /// Classify `text` (a full argv string or file path) against the
    /// configured patterns. `critical` is checked first, so text matching
    /// both lists is reported at the higher severity.
    pub fn classify(&self, text: &str) -> Option<Severity> {
        if self.critical.is_match(text) {
            Some(Severity::Critical)
        } else if self.warning.is_match(text) {
            Some(Severity::Warning)
        } else {
            None
        }
    }
}

/// Load `<root>/flags.json`, seeding it with a small default set of patterns
/// if it doesn't exist yet. Never fails: a missing/unreadable/invalid file
/// falls back to an empty config (with a warning on stderr) so a bad edit
/// can't crash the viewer.
pub fn load(root: &Path) -> FlagConfig {
    let path = root.join("flags.json");
    if !path.exists() {
        let seed = RawFlagConfig {
            critical: DEFAULT_CRITICAL.iter().map(|s| s.to_string()).collect(),
            warning: DEFAULT_WARNING.iter().map(|s| s.to_string()).collect(),
        };
        if let Ok(json) = serde_json::to_string_pretty(&seed) {
            if std::fs::create_dir_all(root).is_ok() {
                if let Err(e) = std::fs::write(&path, json) {
                    eprintln!("tracce · warning: couldn't write default {}: {e}", path.display());
                }
            }
        }
        return build(seed.critical, seed.warning);
    }

    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("tracce · warning: couldn't read {}: {e} — flags disabled", path.display());
            return FlagConfig::empty();
        }
    };
    match serde_json::from_str::<RawFlagConfig>(&contents) {
        Ok(raw) => build(raw.critical, raw.warning),
        Err(e) => {
            eprintln!("tracce · warning: couldn't parse {}: {e} — flags disabled", path.display());
            FlagConfig::empty()
        }
    }
}

pub(crate) fn build(critical: Vec<String>, warning: Vec<String>) -> FlagConfig {
    FlagConfig {
        critical: build_set(&critical),
        warning: build_set(&warning),
    }
}

fn build_set(patterns: &[String]) -> GlobSet {
    let mut builder = GlobSetBuilder::new();
    for p in patterns {
        match Glob::new(p) {
            Ok(g) => { builder.add(g); }
            Err(e) => eprintln!("tracce · warning: invalid glob pattern `{p}` in flags.json: {e} — skipping"),
        }
    }
    builder.build().unwrap_or_else(|_| GlobSetBuilder::new().build().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_matches_nothing() {
        let cfg = FlagConfig::empty();
        assert_eq!(cfg.classify("rm -rf /tmp"), None);
    }

    #[test]
    fn classify_matches_warning() {
        let cfg = build(vec![], vec!["*.env*".to_string()]);
        assert_eq!(cfg.classify("/Users/x/project/.env"), Some(Severity::Warning));
        assert_eq!(cfg.classify("/Users/x/project/README.md"), None);
    }

    #[test]
    fn classify_prefers_critical_over_warning() {
        let cfg = build(vec!["*sudo*".to_string()], vec!["*sudo*".to_string()]);
        assert_eq!(cfg.classify("sudo rm -rf /"), Some(Severity::Critical));
    }

    #[test]
    fn glob_star_matches_across_path_separators() {
        // Command argv strings routinely contain '/', e.g.
        // "/usr/bin/curl … | /bin/sh". Globset's default (non-literal_separator)
        // mode must treat '/' like any other character, or realistic command
        // patterns would silently never match.
        let cfg = build(vec!["*curl*|*sh*".to_string()], vec![]);
        assert_eq!(
            cfg.classify("/usr/bin/curl https://example.com/install | /bin/sh"),
            Some(Severity::Critical)
        );
    }

    #[test]
    fn load_seeds_default_file_when_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg = load(tmp.path());
        assert!(tmp.path().join("flags.json").exists());
        assert_eq!(cfg.classify("sudo rm -rf /"), Some(Severity::Critical));
    }

    #[test]
    fn load_reuses_existing_file_without_overwriting() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("flags.json"),
            r#"{"critical": ["*danger*"], "warning": []}"#,
        ).unwrap();
        let cfg = load(tmp.path());
        assert_eq!(cfg.classify("run danger now"), Some(Severity::Critical));
        assert_eq!(cfg.classify("sudo rm -rf /"), None); // default patterns weren't merged in
    }

    #[test]
    fn load_falls_back_to_empty_on_invalid_json() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("flags.json"), "not valid json").unwrap();
        let cfg = load(tmp.path());
        assert_eq!(cfg.classify("sudo rm -rf /"), None);
    }
}
