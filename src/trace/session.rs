use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use super::provider::Provider;

#[derive(Debug, Clone, Copy)]
pub enum SessionStatus {
    Live,
    Done,
    Crashed,
}

impl SessionStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Done => "done",
            Self::Crashed => "crashed",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meta {
    pub session_id: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub cwd: PathBuf,
    pub argv: Vec<String>,
    #[serde(default)]
    pub provider: Provider,
    #[serde(alias = "claude_pid")]
    pub root_pid: u32,
    pub tracer_pid: u32,
    pub hostname: String,
    pub macos_version: String,
    // `alias` keeps sessions recorded before the ctrace→tracce rename readable.
    #[serde(alias = "ctrace_version")]
    pub tracce_version: String,
}

#[derive(Debug)]
pub struct Session {
    id: String,
    dir: PathBuf,
}

impl Session {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn events_path(&self) -> PathBuf {
        self.dir.join("events.jsonl")
    }

    pub fn create(
        root: &Path,
        provider: Provider,
        root_pid: u32,
        tracer_pid: u32,
        argv: &[String],
        cwd: &Path,
    ) -> Result<Self> {
        let started_at = Utc::now();
        let id = format!(
            "{}_{}_{}",
            started_at.format("%Y-%m-%dT%H-%M-%S"),
            cwd.file_name().and_then(|s| s.to_str()).unwrap_or("unknown"),
            root_pid
        );
        let dir = root.join("sessions").join(&id);
        fs::create_dir_all(&dir).with_context(|| format!("create session dir {dir:?}"))?;

        // Touch events.jsonl so views can open it immediately.
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("events.jsonl"))?;

        let meta = Meta {
            session_id: id.clone(),
            started_at,
            ended_at: None,
            cwd: cwd.to_path_buf(),
            argv: argv.to_vec(),
            provider,
            root_pid,
            tracer_pid,
            hostname: hostname(),
            macos_version: macos_version(),
            tracce_version: env!("CARGO_PKG_VERSION").to_string(),
        };
        write_meta(&dir, &meta)?;

        write_status_file(&dir, SessionStatus::Live)?;
        Ok(Session { id, dir })
    }

    pub fn mark_status(&self, status: SessionStatus) -> Result<()> {
        write_status_file(&self.dir, status)?;
        if matches!(status, SessionStatus::Done | SessionStatus::Crashed) {
            // Patch ended_at in meta.json.
            let text = fs::read_to_string(self.dir.join("meta.json"))?;
            let mut m: Meta = serde_json::from_str(&text)?;
            m.ended_at = Some(Utc::now());
            write_meta(&self.dir, &m)?;
        }
        Ok(())
    }

    /// Chown the session dir tree to (uid, gid). Used by trace before exit so view doesn't need sudo.
    pub fn chown_to(&self, uid: u32, gid: u32) -> Result<()> {
        chown_recursive(&self.dir, uid, gid)
    }
}

fn write_meta(dir: &Path, meta: &Meta) -> Result<()> {
    let mut f = File::create(dir.join("meta.json"))?;
    f.write_all(serde_json::to_string_pretty(meta)?.as_bytes())?;
    Ok(())
}

fn write_status_file(dir: &Path, status: SessionStatus) -> Result<()> {
    let path = dir.join("status");
    let mut f = File::create(&path)?;
    writeln!(f, "{}", status.as_str())?;
    Ok(())
}

fn hostname() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

fn macos_version() -> String {
    std::process::Command::new("sw_vers")
        .arg("-productVersion")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

fn chown_recursive(p: &Path, uid: u32, gid: u32) -> Result<()> {
    use nix::unistd::{chown, Gid, Uid};
    chown(p, Some(Uid::from_raw(uid)), Some(Gid::from_raw(gid)))?;
    if p.is_dir() {
        for entry in fs::read_dir(p)? {
            let e = entry?;
            chown_recursive(&e.path(), uid, gid)?;
        }
    }
    Ok(())
}
