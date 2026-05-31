use crate::event::{Event, EventData, EventKind, FileOp, ProcessRef};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Deserialize)]
struct RawEnvelope {
    process: RawProc,
    #[serde(default)]
    event: serde_json::Value,
    #[serde(default)]
    time: Option<DateTime<Utc>>,
}

#[derive(Deserialize)]
struct RawProc {
    audit_token: AuditToken,
    ppid: u32,
    executable: RawExe,
}

#[derive(Deserialize)]
struct AuditToken { pid: u32 }

#[derive(Deserialize)]
struct RawExe { path: String }

/// Parse a single eslogger JSON line into an `Event`.
/// Returns `Ok(None)` for events we don't track (unrecognized event payload).
pub fn parse_line(line: &str) -> Result<Option<Event>> {
    let env: RawEnvelope = serde_json::from_str(line)
        .with_context(|| format!("eslogger line: {}", &line[..line.len().min(120)]))?;

    let ts_ns: u64 = env.time
        .and_then(|dt| dt.timestamp_nanos_opt())
        .and_then(|n| u64::try_from(n).ok())
        .unwrap_or(0);
    let pid = env.process.audit_token.pid;
    let ppid = env.process.ppid;
    let image = PathBuf::from(&env.process.executable.path);
    let comm = image.file_name().and_then(|s| s.to_str()).unwrap_or("?").to_string();

    let evt = &env.event;

    let (kind, data) = if let Some(exec) = evt.get("exec") {
        // args at exec.args (real eslogger fork+exec) OR exec.target.args (fallback)
        let argv: Vec<String> = exec.get("args")
            .or_else(|| exec.get("target").and_then(|t| t.get("args")))
            .and_then(|a| serde_json::from_value::<Vec<String>>(a.clone()).ok())
            .unwrap_or_default();
        // image: prefer target.executable.path, fall back to outer process.executable.path
        let image_path = exec.get("target")
            .and_then(|t| t.get("executable"))
            .and_then(|e| e.get("path"))
            .and_then(|p| p.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| image.clone());
        (EventKind::Exec, EventData::Exec { argv, image: image_path })
    } else if let Some(fork) = evt.get("fork") {
        let child_pid = fork.get("child")
            .and_then(|c| c.get("audit_token"))
            .and_then(|a| a.get("pid"))
            .and_then(|p| p.as_u64())
            .unwrap_or(0) as u32;
        (EventKind::Fork, EventData::Fork { child_pid })
    } else if let Some(exit) = evt.get("exit") {
        let code = exit.get("stat").and_then(|s| s.as_i64()).unwrap_or(0) as i32;
        (EventKind::Exit, EventData::Exit { code })
    } else if let Some(open) = evt.get("open") {
        let path = open.get("file")
            .and_then(|f| f.get("path"))
            .and_then(|p| p.as_str())
            .map(PathBuf::from)
            .unwrap_or_default();
        (EventKind::Open, EventData::File { op: FileOp::Open, path, size: None })
    } else if let Some(close) = evt.get("close") {
        let path = close.get("target")
            .and_then(|t| t.get("path"))
            .and_then(|p| p.as_str())
            .map(PathBuf::from)
            .unwrap_or_default();
        (EventKind::Close, EventData::File { op: FileOp::Close, path, size: None })
    } else if let Some(create) = evt.get("create") {
        // create.destination.new_path.path OR create.destination.existing_file.path
        let path = create.get("destination")
            .and_then(|d| d.get("new_path").or_else(|| d.get("existing_file")))
            .and_then(|p| p.get("path"))
            .and_then(|p| p.as_str())
            .map(PathBuf::from)
            .unwrap_or_default();
        (EventKind::Create, EventData::File { op: FileOp::Create, path, size: None })
    } else if let Some(write) = evt.get("write") {
        let path = write.get("target")
            .and_then(|t| t.get("path"))
            .and_then(|p| p.as_str())
            .map(PathBuf::from)
            .unwrap_or_default();
        (EventKind::Write, EventData::File { op: FileOp::Write, path, size: None })
    } else if let Some(unlink) = evt.get("unlink") {
        let path = unlink.get("target")
            .and_then(|t| t.get("path"))
            .and_then(|p| p.as_str())
            .map(PathBuf::from)
            .unwrap_or_default();
        (EventKind::Unlink, EventData::File { op: FileOp::Delete, path, size: None })
    } else if let Some(rename) = evt.get("rename") {
        // We surface the destination path — that's the file's final identity
        // and the one a sensitive-path matcher should evaluate. The ES schema
        // either reports an existing-file destination or a (dir + filename) pair.
        let dest = rename.get("destination").and_then(|d| {
            d.get("existing_file")
                .and_then(|e| e.get("path"))
                .and_then(|p| p.as_str())
                .map(PathBuf::from)
                .or_else(|| {
                    let dir = d.get("new_path")
                        .and_then(|n| n.get("dir"))
                        .and_then(|d| d.get("path"))
                        .and_then(|p| p.as_str())?;
                    let name = d.get("new_path")
                        .and_then(|n| n.get("filename"))
                        .and_then(|p| p.as_str())?;
                    Some(PathBuf::from(dir).join(name))
                })
        }).unwrap_or_default();
        (EventKind::Rename, EventData::File { op: FileOp::Rename, path: dest, size: None })
    } else {
        return Ok(None);
    };

    Ok(Some(Event {
        ts_ns,
        kind,
        pid,
        ppid,
        process: Arc::new(ProcessRef {
            pid,
            comm,
            image,
            argv: Vec::new(),
        }),
        data,
        flags: 0,
    }))
}
