use anyhow::Result;
use std::path::Path;

pub fn run(root: &Path, only: Option<String>) -> Result<()> {
    let (uid, gid) = match (
        std::env::var("SUDO_UID").ok().and_then(|s| s.parse::<u32>().ok()),
        std::env::var("SUDO_GID").ok().and_then(|s| s.parse::<u32>().ok()),
    ) {
        (Some(u), Some(g)) => (u, g),
        _ => {
            eprintln!("fix-perms must be run via sudo so SUDO_UID/SUDO_GID are set");
            return Ok(());
        }
    };
    let sessions = root.join("sessions");
    if !sessions.exists() { return Ok(()); }
    for entry in std::fs::read_dir(&sessions)? {
        let entry = entry?;
        let dir = entry.path();
        if !dir.is_dir() { continue; }
        if let Some(o) = &only {
            if dir.file_name().and_then(|n| n.to_str()) != Some(o.as_str()) { continue; }
        }
        chown_recursive(&dir, uid, gid)?;
        // Mark a still-"live" entry whose tracer is gone as interrupted.
        if std::fs::read_to_string(dir.join("status")).map(|s| s.trim() == "live").unwrap_or(false) {
            if let Ok(text) = std::fs::read_to_string(dir.join("meta.json")) {
                if let Ok(meta) = serde_json::from_str::<crate::trace::session::Meta>(&text) {
                    if !pid_alive(meta.tracer_pid) {
                        std::fs::write(dir.join("status"), "interrupted\n")?;
                    }
                }
            }
        }
    }
    Ok(())
}

fn pid_alive(pid: u32) -> bool {
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    kill(Pid::from_raw(pid as i32), None).is_ok()
}

fn chown_recursive(p: &Path, uid: u32, gid: u32) -> Result<()> {
    use nix::unistd::{chown, Gid, Uid};
    chown(p, Some(Uid::from_raw(uid)), Some(Gid::from_raw(gid)))?;
    if p.is_dir() {
        for entry in std::fs::read_dir(p)? {
            let e = entry?;
            chown_recursive(&e.path(), uid, gid)?;
        }
    }
    Ok(())
}
