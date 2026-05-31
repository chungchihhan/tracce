use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

pub const FLAG_SENSITIVE: u32 = 1 << 0;
pub const FLAG_COALESCED: u32 = 1 << 1;
pub const FLAG_DENIED: u32   = 1 << 2;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessRef {
    pub pid: u32,
    pub comm: String,
    pub image: PathBuf,
    pub argv: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Exec, Fork, Exit,
    Open, Write, Create, Close, Unlink, Rename,
    Edit, MultiEdit, Bash,
    NetOpen, NetClose,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FileOp { Open, Write, Create, Close, Delete, Rename, Edit, MultiEdit, Bash }

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NetProto { Tcp, Udp }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "data_kind", rename_all = "snake_case")]
pub enum EventData {
    Exec     { argv: Vec<String>, image: PathBuf },
    Fork     { child_pid: u32 },
    Exit     { code: i32 },
    File     { op: FileOp, path: PathBuf, size: Option<u64> },
    NetOpen  { remote: SocketAddr, local: SocketAddr, host: Option<String>, proto: NetProto },
    NetClose { remote: SocketAddr, bytes_in: u64, bytes_out: u64 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub ts_ns: u64,
    pub kind: EventKind,
    pub pid: u32,
    pub ppid: u32,
    #[serde(rename = "proc")]
    pub process: Arc<ProcessRef>,
    pub data: EventData,
    pub flags: u32,
}
