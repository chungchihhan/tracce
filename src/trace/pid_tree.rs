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
