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
    // `dig -x` first, `dscacheutil` only as a fallback.
    //
    // `dscacheutil -q host -a ip <ip>` does not perform a PTR lookup. It
    // reports what DirectoryService already has cached, so for outbound
    // connections — where nothing has populated that cache — it prints
    // nothing even when a PTR record exists. Measured on macOS 15:
    //
    //     $ dscacheutil -q host -a ip 3.175.227.27
    //     (no output)
    //     $ dig +short -x 3.175.227.27
    //     server-3-175-227-27.nrt12.r.cloudfront.net.
    //
    // The practical effect was that `net_open.host` came back null for every
    // remote address. `dig` ships with macOS at /usr/bin/dig; keeping
    // dscacheutil as a fallback means a host without dig behaves no worse
    // than before rather than losing a path that sometimes hits the cache.
    if let Some(name) = dig_ptr(ip) {
        return Some(name);
    }
    dscacheutil_host(ip)
}

/// PTR lookup via `dig`. Bounded to one try with a 1s timeout: resolution
/// happens on the event-aggregation path, and an unreachable resolver must
/// not stall it. `HostCache` memoises the result (including the miss), so
/// each address pays this at most once per session.
fn dig_ptr(ip: IpAddr) -> Option<String> {
    let out = std::process::Command::new("dig")
        .args(["+short", "+tries=1", "+time=1", "-x", &ip.to_string()])
        .output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    for line in text.lines() {
        let line = line.trim();
        if !line.is_empty() {
            // dig prints FQDNs with the root label, e.g. "one.one.one.one."
            return Some(line.trim_end_matches('.').to_string());
        }
    }
    None
}

/// Cache-only lookup. Kept because it costs nothing when `dig` is absent and
/// occasionally answers for addresses DirectoryService has already seen.
fn dscacheutil_host(ip: IpAddr) -> Option<String> {
    let out = std::process::Command::new("dscacheutil")
        .args(["-q", "host", "-a", "ip", &ip.to_string()])
        .output().ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("name: ") {
            return Some(rest.trim().to_string());
        }
    }
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

    #[test]
    #[ignore = "network-dependent"]
    fn resolves_known_ip() {
        let c = HostCache::new();
        let ip: IpAddr = "1.1.1.1".parse().unwrap();
        let resolved = c.resolve(ip);
        // 1.1.1.1 should resolve to *something* (one.one.one.one usually)
        eprintln!("resolved 1.1.1.1 -> {:?}", resolved);
        // Don't hard-assert — different DNS servers return different names.
    }

    #[test]
    #[ignore = "network-dependent"]
    fn dig_ptr_returns_a_bare_hostname() {
        // Which name comes back depends on the resolver, so assert on shape
        // rather than value: no root label, no surrounding whitespace, and
        // never an empty string in place of a miss.
        let ip: IpAddr = "1.1.1.1".parse().unwrap();
        match dig_ptr(ip) {
            Some(name) => {
                eprintln!("dig_ptr 1.1.1.1 -> {name}");
                assert!(!name.is_empty(), "empty string should be a None");
                assert!(!name.ends_with('.'), "root label not stripped: {name}");
                assert_eq!(name, name.trim(), "hostname not trimmed: {name:?}");
            }
            None => eprintln!("dig_ptr 1.1.1.1 -> None (resolver unavailable)"),
        }
    }
}
