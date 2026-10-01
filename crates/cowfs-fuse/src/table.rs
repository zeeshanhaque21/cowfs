//! The adapter's mirror of what the kernel holds: which inodes it has a lookup reference to
//! and under which names. It gives `..` a real inode number, lets the `Invalidator` find
//! cached names, and detects a `Vfs` that reuses inode numbers. Pure, no kernel access.

use std::collections::HashMap;

use cowfs_vfs::{FileKind, Ino, ROOT_INO};

/// Names remembered per inode. Further hardlink aliases are not tracked, so
/// `Invalidator::invalidate_children` may miss them.
const MAX_NAMES: usize = 8;

struct Node {
    kind: FileKind,
    nlookup: u64,
    parent: Ino,
    names: Vec<(Ino, Vec<u8>)>,
}

/// Inodes the kernel holds a reference to, with the names they were reached by.
#[derive(Default)]
pub(crate) struct Table {
    nodes: HashMap<Ino, Node>,
    dentries: HashMap<(Ino, Vec<u8>), Ino>,
}

/// A `Vfs` handed out an inode number that contradicts what the adapter already knows.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Reused(pub Ino);

impl Table {
    /// Records one more kernel reference to `ino`, reached as `name` in `parent`.
    /// `fresh` marks a just created inode (`create`, `mkdir`, `symlink`): it cannot share a
    /// number with an inode the kernel still references. `paranoid` turns that, and a change
    /// of kind under one number, into an error.
    pub(crate) fn reference(
        &mut self,
        parent: Ino,
        name: &[u8],
        ino: Ino,
        kind: FileKind,
        fresh: bool,
        paranoid: bool,
    ) -> Result<(), Reused> {
        if let Some(n) = self.nodes.get(&ino) {
            let reused = n.nlookup > 0 && (fresh || n.kind != kind);
            if reused && paranoid {
                return Err(Reused(ino));
            }
        }
        self.unname(parent, name);
        let node = self.nodes.entry(ino).or_insert_with(|| Node {
            kind,
            nlookup: 0,
            parent,
            names: Vec::new(),
        });
        node.kind = kind;
        node.nlookup += 1;
        if kind == FileKind::Directory || node.names.is_empty() {
            node.parent = parent;
        }
        if node.names.len() < MAX_NAMES
            && !node.names.iter().any(|(p, n)| *p == parent && n == name)
        {
            node.names.push((parent, name.to_vec()));
            self.dentries.insert((parent, name.to_vec()), ino);
        }
        Ok(())
    }

    /// The kernel dropped `count` references to `ino`.
    pub(crate) fn forget(&mut self, ino: Ino, count: u64) {
        let Some(n) = self.nodes.get_mut(&ino) else {
            return;
        };
        n.nlookup = n.nlookup.saturating_sub(count);
        if n.nlookup == 0 {
            if let Some(n) = self.nodes.remove(&ino) {
                for (p, name) in n.names {
                    self.dentries.remove(&(p, name));
                }
            }
        }
    }

    /// The name `name` in `parent` no longer exists (`unlink`, `rmdir`, or replaced).
    pub(crate) fn unname(&mut self, parent: Ino, name: &[u8]) {
        let Some(ino) = self.dentries.remove(&(parent, name.to_vec())) else {
            return;
        };
        if let Some(n) = self.nodes.get_mut(&ino) {
            n.names.retain(|(p, nm)| !(*p == parent && nm == name));
        }
    }

    /// `name` in `parent` was renamed to `new_name` in `new_parent`, replacing any target.
    pub(crate) fn rename(&mut self, parent: Ino, name: &[u8], new_parent: Ino, new_name: &[u8]) {
        self.unname(new_parent, new_name);
        let Some(ino) = self.dentries.remove(&(parent, name.to_vec())) else {
            return;
        };
        let Some(n) = self.nodes.get_mut(&ino) else {
            return;
        };
        for e in &mut n.names {
            if e.0 == parent && e.1 == name {
                *e = (new_parent, new_name.to_vec());
            }
        }
        if n.kind == FileKind::Directory || n.names.len() == 1 {
            n.parent = new_parent;
        }
        self.dentries.insert((new_parent, new_name.to_vec()), ino);
    }

    /// The directory that holds `ino`, as last seen. The root is its own parent, and an
    /// inode the adapter has no reference to falls back to itself.
    pub(crate) fn parent_of(&self, ino: Ino) -> Ino {
        if ino == ROOT_INO {
            return ROOT_INO;
        }
        self.nodes.get(&ino).map_or(ino, |n| n.parent)
    }

    /// True when the kernel references `ino` but no name of it is known: it was unlinked.
    pub(crate) fn is_unnamed(&self, ino: Ino) -> bool {
        self.nodes.get(&ino).is_some_and(|n| n.names.is_empty())
    }

    /// Outstanding kernel references to `ino`.
    #[cfg(test)]
    pub(crate) fn nlookup(&self, ino: Ino) -> u64 {
        self.nodes.get(&ino).map_or(0, |n| n.nlookup)
    }

    /// Every (name, inode) the kernel may have cached under `parent`.
    pub(crate) fn children(&self, parent: Ino) -> Vec<(Vec<u8>, Ino)> {
        self.dentries
            .iter()
            .filter(|((p, _), _)| *p == parent)
            .map(|((_, n), i)| (n.clone(), *i))
            .collect()
    }

    /// Every inode referenced and every cached name.
    pub(crate) fn snapshot(&self) -> (Vec<Ino>, Vec<(Ino, Vec<u8>)>) {
        (
            self.nodes.keys().copied().collect(),
            self.dentries.keys().cloned().collect(),
        )
    }

    /// Number of inodes referenced.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.nodes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use FileKind::{Directory as D, Regular as F};

    fn t() -> Table {
        Table::default()
    }

    #[test]
    fn forget_drops_the_node_and_its_names() {
        let mut t = t();
        t.reference(1, b"a", 5, F, false, false).unwrap();
        t.reference(1, b"a", 5, F, false, false).unwrap();
        assert_eq!(t.nlookup(5), 2);
        t.forget(5, 1);
        assert_eq!(t.children(1), [(b"a".to_vec(), 5)]);
        t.forget(5, 1);
        assert_eq!((t.len(), t.children(1).len()), (0, 0));
        t.forget(5, 1);
    }

    #[test]
    fn parent_follows_directory_renames() {
        let mut t = t();
        t.reference(1, b"d", 2, D, true, false).unwrap();
        t.reference(1, b"e", 3, D, true, false).unwrap();
        t.reference(2, b"sub", 4, D, true, false).unwrap();
        assert_eq!((t.parent_of(1), t.parent_of(2), t.parent_of(4)), (1, 1, 2));
        t.rename(2, b"sub", 3, b"moved");
        assert_eq!(t.parent_of(4), 3);
        assert_eq!(t.children(2).len(), 0);
        assert_eq!(t.children(3), [(b"moved".to_vec(), 4)]);
        t.rename(1, b"d", 3, b"moved");
        assert_eq!(t.parent_of(2), 3);
        assert_eq!(t.parent_of(99), 99);
    }

    #[test]
    fn unname_and_replacement_keep_the_mirror_consistent() {
        let mut t = t();
        t.reference(1, b"a", 5, F, false, false).unwrap();
        t.unname(1, b"a");
        assert!(t.children(1).is_empty());
        assert!(t.is_unnamed(5) && !t.is_unnamed(6));
        t.reference(1, b"a", 5, F, false, false).unwrap();
        t.reference(1, b"a", 6, F, false, false).unwrap();
        let (_, names) = t.snapshot();
        assert_eq!(names, [(1, b"a".to_vec())]);
    }

    #[test]
    fn hardlinks_track_each_name_up_to_a_cap() {
        let mut t = t();
        for i in 0..20u8 {
            t.reference(1, &[b'n', i], 5, F, false, false).unwrap();
        }
        assert_eq!(t.children(1).len(), MAX_NAMES);
        assert_eq!(t.nlookup(5), 20);
    }

    #[test]
    fn paranoid_detects_a_reused_number() {
        let mut t = t();
        t.reference(1, b"f", 5, F, true, true).unwrap();
        assert_eq!(t.reference(1, b"g", 5, F, true, true), Err(Reused(5)));
        assert_eq!(t.reference(1, b"g", 5, D, false, true), Err(Reused(5)));
        assert!(
            t.reference(1, b"h", 5, F, false, true).is_ok(),
            "a hardlink is not reuse"
        );
        t.forget(5, 2);
        assert!(
            t.reference(1, b"g", 5, F, true, true).is_ok(),
            "reuse after forget is fine"
        );
        assert!(
            t.reference(1, b"g", 5, F, true, false).is_ok(),
            "only when paranoid"
        );
    }
}
