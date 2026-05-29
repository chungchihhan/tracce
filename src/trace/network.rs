use std::collections::HashSet;
use std::net::SocketAddr;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Connection {
    pub pid: u32,
    pub local: SocketAddr,
    pub remote: SocketAddr,
    pub state: String,
}

/// Parse `lsof -i -n -P` output. Skips the header line and any rows that aren't
/// TCP/UDP with a `local->remote` NAME column.
pub fn parse_lsof(text: &str) -> Vec<Connection> {
    let mut out = Vec::new();
    for line in text.lines().skip(1) {
        if let Some(c) = parse_line(line) {
            out.push(c);
        }
    }
    out
}

fn parse_line(line: &str) -> Option<Connection> {
    let mut fields = line.split_whitespace();
    let _command = fields.next()?;
    let pid: u32 = fields.next()?.parse().ok()?;
    let _user   = fields.next()?;
    let _fd     = fields.next()?;
    let _type   = fields.next()?;
    let _device = fields.next()?;
    let _size   = fields.next()?;
    let _node   = fields.next()?;  // "TCP" or "UDP"
    // remaining fields are NAME plus parenthesized state
    let rest: Vec<&str> = fields.collect();
    if rest.is_empty() { return None; }

    // NAME looks like:  192.168.1.5:55321->54.230.93.21:443   or  [::1]:6379->[::1]:60022
    // State is parenthesized at end.
    let name = rest[0];
    let arrow = name.find("->")?;
    let local_str = &name[..arrow];
    let remote_str = &name[arrow + 2..];

    let local = parse_sockaddr(local_str)?;
    let remote = parse_sockaddr(remote_str)?;
    let state = rest.iter().rev().find_map(|tok| {
        let t = tok.trim_matches(|c| c == '(' || c == ')');
        if !t.is_empty() && t.chars().all(|c| c.is_ascii_uppercase() || c == '_') {
            Some(t.to_string())
        } else { None }
    }).unwrap_or_default();

    Some(Connection { pid, local, remote, state })
}

fn parse_sockaddr(s: &str) -> Option<SocketAddr> {
    // Handle bracketed IPv6 like [::1]:6379 vs IPv4 like 192.168.1.5:55321.
    s.parse().ok()
}

/// Return (opened, closed) by symmetric difference. Each pair of (pid, local, remote)
/// is the identity of a connection for this purpose.
pub fn diff_connections(prev: &[Connection], now: &[Connection]) -> (Vec<Connection>, Vec<Connection>) {
    let prev_set: HashSet<&Connection> = prev.iter().collect();
    let now_set: HashSet<&Connection> = now.iter().collect();
    let opens: Vec<Connection> = now.iter().filter(|c| !prev_set.contains(c)).cloned().collect();
    let closes: Vec<Connection> = prev.iter().filter(|c| !now_set.contains(c)).cloned().collect();
    (opens, closes)
}

/// Spawn `lsof` for the given pids and return its stdout text. Errors are returned to the caller.
pub fn run_lsof(pids: &[u32]) -> std::io::Result<String> {
    if pids.is_empty() {
        return Ok(String::new());
    }
    let pid_arg = pids.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
    let out = std::process::Command::new("lsof")
        .args(["-i", "-n", "-P", "-p", &pid_arg])
        .output()?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}
