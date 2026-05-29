use crate::trace::session::Meta;
use anyhow::Result;
use std::path::Path;

pub fn run(root: &Path) -> Result<()> {
    println!("STATUS\tSTARTED\tCWD\tCLAUDE_PID\tEVENTS\tSESSION_ID");
    let sessions_dir = root.join("sessions");
    if !sessions_dir.exists() {
        return Ok(());
    }
    let mut rows: Vec<Row> = Vec::new();
    for entry in std::fs::read_dir(&sessions_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() { continue; }
        let meta_text = match std::fs::read_to_string(path.join("meta.json")) {
            Ok(t) => t, Err(_) => continue,
        };
        let meta: Meta = match serde_json::from_str(&meta_text) {
            Ok(m) => m, Err(_) => continue,
        };
        let status = std::fs::read_to_string(path.join("status"))
            .map(|s| s.trim().to_string()).unwrap_or_else(|_| "?".into());
        let events = count_lines(&path.join("events.jsonl")).unwrap_or(0);
        rows.push(Row { meta, status, events });
    }
    // Live first, then by started_at desc.
    rows.sort_by(|a, b| {
        let live = (b.status == "live").cmp(&(a.status == "live"));
        if live != std::cmp::Ordering::Equal { return live; }
        b.meta.started_at.cmp(&a.meta.started_at)
    });
    for r in rows {
        let cwd_basename = r.meta.cwd.file_name()
            .and_then(|s| s.to_str()).unwrap_or("?");
        println!(
            "{}\t{}\t{}\t{}\t{}\t{}",
            r.status,
            r.meta.started_at.format("%Y-%m-%d %H:%M:%S"),
            cwd_basename,
            r.meta.claude_pid,
            r.events,
            r.meta.session_id,
        );
    }
    Ok(())
}

struct Row { meta: Meta, status: String, events: usize }

fn count_lines(p: &Path) -> std::io::Result<usize> {
    use std::io::{BufRead, BufReader};
    let f = std::fs::File::open(p)?;
    Ok(BufReader::new(f).lines().count())
}
