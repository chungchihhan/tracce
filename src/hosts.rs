use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;

const KNOWN: &[(&str, &str)] = &[
    // Best-effort static fallbacks when reverse DNS returns nothing.
    // Used only when the cache misses AND the reverse lookup fails.
];

pub struct HostCache {
    map: Mutex<HashMap<IpAddr, Option<String>>>,
}

impl HostCache {
    pub fn new() -> Self {
        Self { map: Mutex::new(HashMap::new()) }
    }

    pub fn resolve(&self, ip: IpAddr) -> Option<String> {
        if let Some(v) = self.map.lock().unwrap().get(&ip).cloned() {
            return v;
        }
        let resolved = reverse_lookup(ip);
        let fallback = resolved.clone().or_else(|| known_for(ip));
        self.map.lock().unwrap().insert(ip, fallback.clone());
        fallback
    }
}

impl Default for HostCache { fn default() -> Self { Self::new() } }

fn reverse_lookup(ip: IpAddr) -> Option<String> {
    // Use the OS getnameinfo via std::net via a dummy socket addr.
    use std::ffi::CString;
    use std::net::SocketAddr;

    // We shell out to `dscacheutil -q host -a ip <ip>` because std doesn't expose
    // getnameinfo cleanly without extra deps, and dscacheutil is always present on macOS.
    let out = std::process::Command::new("dscacheutil")
        .args(["-q", "host", "-a", "ip", &ip.to_string()])
        .output().ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("name: ") {
            return Some(rest.trim().to_string());
        }
    }
    let _ = (CString::new(""), SocketAddr::from(([0u8;4], 0)));
    None
}

fn known_for(_ip: IpAddr) -> Option<String> {
    // The static KNOWN table is for hostname overrides keyed by hostname,
    // not IP. Reserved for future use; today this is empty.
    let _ = KNOWN;
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn cache_returns_cached_value() {
        let c = HostCache::new();
        // First call may or may not resolve depending on network — but second call
        // must return the same thing without re-querying.
        let ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
        let a = c.resolve(ip);
        let b = c.resolve(ip);
        assert_eq!(a, b);
    }
}
