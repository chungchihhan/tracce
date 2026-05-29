use crate::trace::session::Meta;
use anyhow::Result;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct SessionEntry {
    pub dir: PathBuf,
    pub meta: Meta,
    pub status: String,
    pub events_path: PathBuf,
}

pub fn discover(root: &Path) -> Result<Vec<SessionEntry>> {
    let sessions = root.join("sessions");
    if !sessions.exists() { return Ok(Vec::new()); }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&sessions)? {
        let entry = entry?;
        let dir = entry.path();
        if !dir.is_dir() { continue; }
        let meta_text = match std::fs::read_to_string(dir.join("meta.json")) { Ok(t) => t, Err(_) => continue };
        let meta: Meta = match serde_json::from_str(&meta_text) { Ok(m) => m, Err(_) => continue };
        let status = std::fs::read_to_string(dir.join("status"))
            .map(|s| s.trim().to_string()).unwrap_or_else(|_| "?".into());
        let events_path = dir.join("events.jsonl");
        out.push(SessionEntry { dir, meta, status, events_path });
    }
    out.sort_by(|a, b| {
        let live = (b.status == "live").cmp(&(a.status == "live"));
        if live != std::cmp::Ordering::Equal { return live; }
        b.meta.started_at.cmp(&a.meta.started_at)
    });
    Ok(out)
}
