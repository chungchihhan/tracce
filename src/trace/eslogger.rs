use crate::event::{Event, EventData, EventKind, FileOp, ProcessRef};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;

// Endpoint Security event type numbers we care about. See:
// https://developer.apple.com/documentation/endpointsecurity/es_event_type_t
const ES_EVENT_TYPE_NOTIFY_EXEC: u32   = 9;
const ES_EVENT_TYPE_NOTIFY_OPEN: u32   = 14;
const ES_EVENT_TYPE_NOTIFY_FORK: u32   = 11;
const ES_EVENT_TYPE_NOTIFY_EXIT: u32   = 10;
const ES_EVENT_TYPE_NOTIFY_CLOSE: u32  = 15;
const ES_EVENT_TYPE_NOTIFY_CREATE: u32 = 8;
const ES_EVENT_TYPE_NOTIFY_WRITE: u32  = 16;

#[derive(Deserialize)]
struct RawEnvelope {
    event_type: u32,
    process: RawProc,
    #[serde(default)]
    event: serde_json::Value,
    time: RawTime,
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

#[derive(Deserialize)]
struct RawTime { tv_sec: i64, tv_nsec: i64 }

/// Parse a single eslogger JSON line into an `Event`.
/// Returns `Ok(None)` for events we don't track (unknown event_type).
pub fn parse_line(line: &str) -> Result<Option<Event>> {
    let env: RawEnvelope = serde_json::from_str(line)
        .with_context(|| format!("eslogger line: {}", &line[..line.len().min(120)]))?;

    let ts_ns = (env.time.tv_sec as u64) * 1_000_000_000 + env.time.tv_nsec as u64;
    let pid = env.process.audit_token.pid;
    let ppid = env.process.ppid;
    let image = PathBuf::from(&env.process.executable.path);
    let comm = image.file_name().and_then(|s| s.to_str()).unwrap_or("?").to_string();

    let (kind, data) = match env.event_type {
        ES_EVENT_TYPE_NOTIFY_EXEC => {
            let target = env.event.get("exec").and_then(|e| e.get("target"));
            let argv: Vec<String> = target
                .and_then(|t| t.get("args"))
                .and_then(|a| serde_json::from_value::<Vec<String>>(a.clone()).ok())
                .unwrap_or_default();
            let image_path = target
                .and_then(|t| t.get("executable"))
                .and_then(|e| e.get("path"))
                .and_then(|p| p.as_str())
                .map(PathBuf::from)
                .unwrap_or_else(|| image.clone());
            (EventKind::Exec, EventData::Exec { argv, image: image_path })
        }
        ES_EVENT_TYPE_NOTIFY_FORK => {
            let child_pid = env.event.get("fork")
                .and_then(|f| f.get("child"))
                .and_then(|c| c.get("audit_token"))
                .and_then(|a| a.get("pid"))
                .and_then(|p| p.as_u64())
                .unwrap_or(0) as u32;
            (EventKind::Fork, EventData::Fork { child_pid })
        }
        ES_EVENT_TYPE_NOTIFY_EXIT => {
            let code = env.event.get("exit")
                .and_then(|e| e.get("stat"))
                .and_then(|s| s.as_i64())
                .unwrap_or(0) as i32;
            (EventKind::Exit, EventData::Exit { code })
        }
        ES_EVENT_TYPE_NOTIFY_OPEN   => file_event(&env, "open",   FileOp::Open,   EventKind::Open),
        ES_EVENT_TYPE_NOTIFY_CLOSE  => file_event(&env, "close",  FileOp::Close,  EventKind::Close),
        ES_EVENT_TYPE_NOTIFY_CREATE => file_event(&env, "create", FileOp::Create, EventKind::Create),
        ES_EVENT_TYPE_NOTIFY_WRITE  => file_event(&env, "write",  FileOp::Write,  EventKind::Write),
        _ => return Ok(None),
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

fn file_event(env: &RawEnvelope, key: &str, op: FileOp, kind: EventKind) -> (EventKind, EventData) {
    let path = env.event.get(key)
        .and_then(|e| e.get("file"))
        .and_then(|f| f.get("path"))
        .and_then(|p| p.as_str())
        .map(PathBuf::from)
        .or_else(|| env.event.get(key)
            .and_then(|e| e.get("target"))
            .and_then(|t| t.get("path"))
            .and_then(|p| p.as_str())
            .map(PathBuf::from))
        .unwrap_or_default();
    (kind, EventData::File { op, path, size: None })
}
