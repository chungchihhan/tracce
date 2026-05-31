use std::collections::{HashMap, HashSet};

#[derive(Debug, Default)]
pub struct PidTree {
    root: u32,
    pids: HashSet<u32>,
    parents: HashMap<u32, u32>,
}

impl PidTree {
    pub fn new(root: u32) -> Self {
        let mut pids = HashSet::new();
        pids.insert(root);
        Self { root, pids, parents: HashMap::new() }
    }

    pub fn root(&self) -> u32 { self.root }
    pub fn contains(&self, pid: u32) -> bool { self.pids.contains(&pid) }
    pub fn len(&self) -> usize { self.pids.len() }

    /// Record that `child` was forked from `parent`. If `parent` is in the tree,
    /// `child` joins.
    pub fn on_fork(&mut self, parent: u32, child: u32) {
        if self.pids.contains(&parent) {
            self.pids.insert(child);
            self.parents.insert(child, parent);
        }
    }

    /// Record exec — does not change membership but may be useful for argv updates elsewhere.
    pub fn on_exec(&mut self, _pid: u32) {}

    pub fn pids(&self) -> Vec<u32> {
        self.pids.iter().copied().collect()
    }

    pub fn on_exit(&mut self, pid: u32) {
        if pid == self.root {
            // Root exits → conceptually the tree is dead, but keep set so late
            // events from descendants can still be matched up until cleanup.
        }
        self.pids.remove(&pid);
        self.parents.remove(&pid);
    }

    /// Seed the tree with all current descendants of `root` by walking the live process table.
    /// Best-effort: silently no-op on sysctl failure.
    pub fn seed_descendants(&mut self) {
        use std::collections::HashMap;
        let snapshot = match list_processes() {
            Ok(s) => s,
            Err(_) => return,
        };
        // Build pid -> ppid map.
        let parent_of: HashMap<u32, u32> = snapshot.into_iter().collect();
        // BFS from root.
        let mut frontier: Vec<u32> = parent_of
            .iter()
            .filter_map(|(pid, ppid)| if *ppid == self.root { Some(*pid) } else { None })
            .collect();
        while let Some(p) = frontier.pop() {
            if self.pids.insert(p) {
                // Walk further down: any process whose parent is p.
                for (child, &child_ppid) in parent_of.iter() {
                    if child_ppid == p {
                        frontier.push(*child);
                    }
                }
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn list_processes() -> std::io::Result<Vec<(u32, u32)>> {
    // Use /bin/ps -A -o pid=,ppid= which is available on all macOS systems.
    // The trailing '=' suppresses the column header.
    let out = std::process::Command::new("/bin/ps")
        .args(["-A", "-o", "pid=,ppid="])
        .output()?;
    let mut v = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let mut it = line.split_whitespace();
        if let (Some(p), Some(pp)) = (it.next(), it.next()) {
            if let (Ok(p), Ok(pp)) = (p.parse::<u32>(), pp.parse::<u32>()) {
                v.push((p, pp));
            }
        }
    }
    Ok(v)
}

#[cfg(not(target_os = "macos"))]
fn list_processes() -> std::io::Result<Vec<(u32, u32)>> {
    Ok(Vec::new())
}

/// Snapshot all current descendants of `root`, returning `(pid, ppid, comm)`
/// in BFS order from the root. Used by the poll-only trace mode to synthesize
/// Exec events for processes we'd otherwise learn about via eslogger.
///
/// BFS ordering matters: the aggregator only adds a pid to its tree if the
/// pid's parent is already in the tree, so parents must be emitted first.
#[cfg(target_os = "macos")]
pub fn list_descendants_with_comm(root: u32) -> std::io::Result<Vec<(u32, u32, String)>> {
    use std::collections::VecDeque;
    let out = std::process::Command::new("/bin/ps")
        .args(["-A", "-o", "pid=,ppid=,command="])
        .output()?;
    // pid -> (ppid, full command). `command=` gives executable + argv, which
    // is what we want for display — `comm=` only shows the binary name and
    // wraps mismatched names in parens like "(git)".
    let mut all: HashMap<u32, (u32, String)> = HashMap::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        // split_whitespace handles ps's right-aligned numeric columns (which
        // emit leading spaces for short pids) and collapses interior runs.
        let mut parts = line.split_whitespace();
        if let (Some(p), Some(pp), Some(first)) = (parts.next(), parts.next(), parts.next()) {
            if let (Ok(p), Ok(pp)) = (p.parse::<u32>(), pp.parse::<u32>()) {
                // Strip the path from argv[0] so "git status" fits the column
                // instead of "/usr/bin/git status". Note: macOS ps wraps
                // zombie processes' comms as "(name)" — we keep the parens
                // as a visual signal that the process has exited.
                let bn = std::path::Path::new(first).file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or(first);
                let rest: Vec<&str> = parts.collect();
                let cmd = if rest.is_empty() { bn.to_string() } else { format!("{bn} {}", rest.join(" ")) };
                all.insert(p, (pp, cmd));
            }
        }
    }
    let mut visited: HashSet<u32> = HashSet::from([root]);
    let mut queue: VecDeque<u32> = VecDeque::from([root]);
    let mut ordered: Vec<(u32, u32, String)> = Vec::new();
    while let Some(p) = queue.pop_front() {
        for (&child, (pp, comm)) in &all {
            if *pp == p && visited.insert(child) {
                ordered.push((child, *pp, comm.clone()));
                queue.push_back(child);
            }
        }
    }
    Ok(ordered)
}

#[cfg(not(target_os = "macos"))]
pub fn list_descendants_with_comm(_root: u32) -> std::io::Result<Vec<(u32, u32, String)>> {
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_descendants_finds_live_children() {
        // Spawn a child process, then call seed_descendants from the parent.
        use std::process::Command;
        let child = Command::new("sleep").arg("10").spawn().unwrap();
        let our_pid = std::process::id();
        let mut tree = PidTree::new(our_pid);
        tree.seed_descendants();
        assert!(tree.contains(child.id()),
            "tree {tree:?} should contain spawned child {}", child.id());
        // Clean up
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(child.id() as i32),
            nix::sys::signal::Signal::SIGKILL,
        );
    }

    #[test]
    fn root_is_in_tree() {
        let t = PidTree::new(100);
        assert!(t.contains(100));
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn child_of_root_joins() {
        let mut t = PidTree::new(100);
        t.on_fork(100, 200);
        assert!(t.contains(200));
    }

    #[test]
    fn grandchild_joins_via_child() {
        let mut t = PidTree::new(100);
        t.on_fork(100, 200);
        t.on_fork(200, 300);
        assert!(t.contains(300));
    }

    #[test]
    fn unrelated_fork_ignored() {
        let mut t = PidTree::new(100);
        t.on_fork(999, 800);
        assert!(!t.contains(800));
    }

    #[test]
    fn exit_removes_from_tree() {
        let mut t = PidTree::new(100);
        t.on_fork(100, 200);
        t.on_exit(200);
        assert!(!t.contains(200));
    }
}
