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

    pub fn on_exit(&mut self, pid: u32) {
        if pid == self.root {
            // Root exits → conceptually the tree is dead, but keep set so late
            // events from descendants can still be matched up until cleanup.
        }
        self.pids.remove(&pid);
        self.parents.remove(&pid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
