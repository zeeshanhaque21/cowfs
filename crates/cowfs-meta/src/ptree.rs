//! Persistent content-addressed B+tree over redb tables.
//!
//! Stored nodes are immutable. A transaction edits an in-memory copy of only the nodes on the
//! paths it touches, and `flush` hashes and writes the copies bottom-up.

use crate::node::{self, encode, encoded_size, entry_cost, Node, NodeId};
use crate::{Error, Result};
use redb::{ReadableTable, Table};
use std::collections::HashMap;

pub(crate) type NodesTable<'t> = Table<'t, [u8; 32], &'static [u8]>;
pub(crate) type RefsTable<'t> = Table<'t, [u8; 32], u64>;

/// Read access to stored node bytes by id.
pub(crate) trait NodeSource {
    fn read(&self, id: &NodeId) -> Result<Option<Vec<u8>>>;
}

impl<T: ReadableTable<[u8; 32], &'static [u8]>> NodeSource for T {
    fn read(&self, id: &NodeId) -> Result<Option<Vec<u8>>> {
        Ok(self.get(*id.as_bytes())?.map(|g| g.value().to_vec()))
    }
}

/// Loads a node and verifies its hash against the id it was looked up by.
pub(crate) fn load<S: NodeSource>(src: &S, id: &NodeId) -> Result<Node> {
    let bytes = src
        .read(id)?
        .ok_or_else(|| Error::Corrupt(format!("missing tree node {id}")))?;
    if node::hash(&bytes) != *id {
        return Err(Error::Corrupt(format!("tree node {id} fails its hash")));
    }
    Node::parse(bytes)
}

/// Point lookup in a stored tree.
pub(crate) fn get<S: NodeSource>(src: &S, root: &NodeId, key: &[u8]) -> Result<Option<Vec<u8>>> {
    let mut id = *root;
    loop {
        let n = load(src, &id)?;
        if n.is_leaf() {
            return Ok(n.search(key).ok().map(|i| n.val(i).to_vec()));
        }
        id = n.child(n.find_child(key));
    }
}

/// In-order cursor over a stored tree.
#[derive(Debug)]
pub(crate) struct Cursor {
    stack: Vec<(Node, usize)>,
}

impl Cursor {
    /// Positions the cursor at the first entry whose key is at least `key`.
    pub(crate) fn seek<S: NodeSource>(src: &S, root: &NodeId, key: &[u8]) -> Result<Cursor> {
        let mut stack = Vec::new();
        let mut id = *root;
        loop {
            let n = load(src, &id)?;
            if n.is_leaf() {
                let idx = n.search(key).unwrap_or_else(|i| i);
                stack.push((n, idx));
                return Ok(Cursor { stack });
            }
            let ci = n.find_child(key);
            id = n.child(ci);
            stack.push((n, ci));
        }
    }

    pub(crate) fn next<S: NodeSource>(&mut self, src: &S) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
        loop {
            let Some((node, idx)) = self.stack.last_mut() else {
                return Ok(None);
            };
            if *idx < node.len() {
                let r = (node.key(*idx).to_vec(), node.val(*idx).to_vec());
                *idx += 1;
                return Ok(Some(r));
            }
            self.stack.pop();
            loop {
                let Some((p, pi)) = self.stack.last_mut() else {
                    return Ok(None);
                };
                *pi += 1;
                if *pi < p.len() {
                    let mut id = p.child(*pi);
                    loop {
                        let n = load(src, &id)?;
                        let leaf = n.is_leaf();
                        if !leaf {
                            id = n.child(0);
                        }
                        self.stack.push((n, 0));
                        if leaf {
                            break;
                        }
                    }
                    break;
                }
                self.stack.pop();
            }
        }
    }
}

type Entry = (Vec<u8>, Vec<u8>);

enum Kid {
    Id(NodeId),
    Mem(Box<MNode>),
}

enum MNode {
    Leaf(Vec<Entry>),
    Internal(Vec<(Vec<u8>, Kid)>),
}

impl MNode {
    fn len(&self) -> usize {
        match self {
            MNode::Leaf(e) => e.len(),
            MNode::Internal(e) => e.len(),
        }
    }

    fn size(&self) -> usize {
        match self {
            MNode::Leaf(e) => encoded_size(
                e.len(),
                e.iter()
                    .map(|(k, v)| entry_cost(k.len(), v.len()) - 4)
                    .sum(),
            ),
            MNode::Internal(e) => encoded_size(
                e.len(),
                e.iter().map(|(k, _)| entry_cost(k.len(), 32) - 4).sum(),
            ),
        }
    }
}

impl Kid {
    fn mem<S: NodeSource>(&mut self, src: &S) -> Result<&mut MNode> {
        if let Kid::Id(id) = self {
            let n = load(src, id)?;
            let m = if n.is_leaf() {
                MNode::Leaf(
                    (0..n.len())
                        .map(|i| (n.key(i).to_vec(), n.val(i).to_vec()))
                        .collect(),
                )
            } else {
                MNode::Internal(
                    (0..n.len())
                        .map(|i| (n.key(i).to_vec(), Kid::Id(n.child(i))))
                        .collect(),
                )
            };
            *self = Kid::Mem(Box::new(m));
        }
        match self {
            Kid::Mem(m) => Ok(m),
            Kid::Id(_) => Err(Error::Corrupt("unreachable".into())),
        }
    }

    fn size<S: NodeSource>(&mut self, src: &S) -> Result<usize> {
        Ok(self.mem(src)?.size())
    }
}

/// Splits `v` into pieces of at most `max` encoded bytes (single oversize entries stay alone).
/// `v` keeps the first piece; the rest are returned in order.
fn split_pieces<T>(v: &mut Vec<T>, max: usize, cost: &dyn Fn(&T) -> usize) -> Vec<Vec<T>> {
    let total = |v: &[T]| encoded_size(v.len(), v.iter().map(|e| cost(e) - 4).sum());
    if v.len() < 2 || total(v) <= max {
        return Vec::new();
    }
    let half: usize = v.iter().map(cost).sum::<usize>() / 2;
    let mut acc = 0;
    let mut mid = v.len() - 1;
    for (i, e) in v.iter().enumerate() {
        acc += cost(e);
        if acc >= half {
            mid = i.max(1);
            break;
        }
    }
    let mid = mid.clamp(1, v.len() - 1);
    let mut right = v.split_off(mid);
    let mut out = split_pieces(v, max, cost);
    let more = split_pieces(&mut right, max, cost);
    out.push(right);
    out.extend(more);
    out
}

fn route(es: &[(Vec<u8>, Kid)], key: &[u8]) -> usize {
    es.get(1..)
        .map_or(0, |r| r.partition_point(|(k, _)| k.as_slice() <= key))
}

fn leaf_cost(e: &Entry) -> usize {
    entry_cost(e.0.len(), e.1.len())
}

fn internal_cost(e: &(Vec<u8>, Kid)) -> usize {
    entry_cost(e.0.len(), 32)
}

/// A tree being edited inside a write transaction.
pub(crate) struct MemTree {
    root: Kid,
    max: usize,
}

impl MemTree {
    pub(crate) fn new(root: NodeId, max: usize) -> Self {
        Self {
            root: Kid::Id(root),
            max,
        }
    }

    pub(crate) fn empty(max: usize) -> Self {
        Self {
            root: Kid::Mem(Box::new(MNode::Leaf(Vec::new()))),
            max,
        }
    }

    /// True when the tree was never edited, so its root is still the stored one.
    pub(crate) fn unchanged(&self) -> bool {
        matches!(self.root, Kid::Id(_))
    }

    pub(crate) fn get<S: NodeSource>(&self, src: &S, key: &[u8]) -> Result<Option<Vec<u8>>> {
        get_kid(&self.root, src, key)
    }

    /// First entry with a key at least `key`.
    pub(crate) fn seek_ge<S: NodeSource>(&self, src: &S, key: &[u8]) -> Result<Option<Entry>> {
        seek_kid(&self.root, src, key)
    }

    pub(crate) fn insert<S: NodeSource>(
        &mut self,
        src: &S,
        key: &[u8],
        val: Vec<u8>,
    ) -> Result<()> {
        let extra = insert_kid(&mut self.root, src, key, val, self.max)?;
        if !extra.is_empty() {
            let old = std::mem::replace(&mut self.root, Kid::Id(NodeId::from_bytes([0; 32])));
            let mut es = vec![(Vec::new(), old)];
            es.extend(extra);
            self.root = Kid::Mem(Box::new(MNode::Internal(es)));
        }
        Ok(())
    }

    pub(crate) fn remove<S: NodeSource>(&mut self, src: &S, key: &[u8]) -> Result<bool> {
        if self.get(src, key)?.is_none() {
            return Ok(false);
        }
        remove_kid(&mut self.root, src, key, self.max)?;
        while let MNode::Internal(es) = self.root.mem(src)? {
            match es.len() {
                0 => self.root = Kid::Mem(Box::new(MNode::Leaf(Vec::new()))),
                1 => {
                    let (_, only) = es
                        .pop()
                        .ok_or_else(|| Error::Corrupt("unreachable".into()))?;
                    self.root = only;
                }
                _ => break,
            }
        }
        Ok(true)
    }

    /// Writes every modified node and returns the new root id.
    pub(crate) fn flush(&mut self, w: &mut NodeWriter<'_>) -> Result<NodeId> {
        flush_kid(&mut self.root, w)
    }
}

fn get_kid<S: NodeSource>(kid: &Kid, src: &S, key: &[u8]) -> Result<Option<Vec<u8>>> {
    match kid {
        Kid::Id(id) => get(src, id, key),
        Kid::Mem(m) => match &**m {
            MNode::Leaf(es) => Ok(es
                .binary_search_by(|(k, _)| k.as_slice().cmp(key))
                .ok()
                .map(|i| es[i].1.clone())),
            MNode::Internal(es) => {
                let ci = route(es, key);
                get_kid(&es[ci].1, src, key)
            }
        },
    }
}

fn seek_kid<S: NodeSource>(kid: &Kid, src: &S, key: &[u8]) -> Result<Option<Entry>> {
    match kid {
        Kid::Id(id) => Cursor::seek(src, id, key)?.next(src),
        Kid::Mem(m) => match &**m {
            MNode::Leaf(es) => {
                let i = es.partition_point(|(k, _)| k.as_slice() < key);
                Ok(es.get(i).cloned())
            }
            MNode::Internal(es) => {
                let ci = route(es, key);
                for (_, child) in &es[ci..] {
                    if let Some(e) = seek_kid(child, src, key)? {
                        return Ok(Some(e));
                    }
                }
                Ok(None)
            }
        },
    }
}

fn insert_kid<S: NodeSource>(
    kid: &mut Kid,
    src: &S,
    key: &[u8],
    val: Vec<u8>,
    max: usize,
) -> Result<Vec<(Vec<u8>, Kid)>> {
    let mut out = Vec::new();
    match kid.mem(src)? {
        MNode::Leaf(es) => {
            match es.binary_search_by(|(k, _)| k.as_slice().cmp(key)) {
                Ok(i) => es[i].1 = val,
                Err(i) => es.insert(i, (key.to_vec(), val)),
            }
            for piece in split_pieces(es, max, &leaf_cost) {
                out.push((piece[0].0.clone(), Kid::Mem(Box::new(MNode::Leaf(piece)))));
            }
        }
        MNode::Internal(es) => {
            let ci = route(es, key);
            let extra = insert_kid(&mut es[ci].1, src, key, val, max)?;
            for (i, e) in extra.into_iter().enumerate() {
                es.insert(ci + 1 + i, e);
            }
            for piece in split_pieces(es, max, &internal_cost) {
                out.push((
                    piece[0].0.clone(),
                    Kid::Mem(Box::new(MNode::Internal(piece))),
                ));
            }
        }
    }
    Ok(out)
}

fn remove_kid<S: NodeSource>(kid: &mut Kid, src: &S, key: &[u8], max: usize) -> Result<()> {
    match kid.mem(src)? {
        MNode::Leaf(es) => {
            if let Ok(i) = es.binary_search_by(|(k, _)| k.as_slice().cmp(key)) {
                es.remove(i);
            }
        }
        MNode::Internal(es) => {
            let ci = route(es, key);
            remove_kid(&mut es[ci].1, src, key, max)?;
            fix_child(es, ci, src, max)?;
        }
    }
    Ok(())
}

fn fix_child<S: NodeSource>(
    es: &mut Vec<(Vec<u8>, Kid)>,
    ci: usize,
    src: &S,
    max: usize,
) -> Result<()> {
    let size = es[ci].1.size(src)?;
    if es[ci].1.mem(src)?.len() == 0 {
        es.remove(ci);
        return Ok(());
    }
    if size >= max / 4 {
        return Ok(());
    }
    let sib = if ci + 1 < es.len() {
        ci + 1
    } else if ci > 0 {
        ci - 1
    } else {
        return Ok(());
    };
    let (l, r) = (ci.min(sib), ci.max(sib));
    let lsize = es[l].1.size(src)?;
    let rsize = es[r].1.size(src)?;
    if lsize + rsize - encoded_size(0, 0) > max * 3 / 4 {
        return Ok(());
    }
    let (_, right) = es.remove(r);
    let Kid::Mem(right) = right else {
        return Err(Error::Corrupt("unreachable".into()));
    };
    match (es[l].1.mem(src)?, *right) {
        (MNode::Leaf(a), MNode::Leaf(b)) => a.extend(b),
        (MNode::Internal(a), MNode::Internal(b)) => a.extend(b),
        _ => return Err(Error::Corrupt("sibling nodes differ in level".into())),
    }
    Ok(())
}

fn flush_kid(kid: &mut Kid, w: &mut NodeWriter<'_>) -> Result<NodeId> {
    let m = match kid {
        Kid::Id(id) => return Ok(*id),
        Kid::Mem(m) => m,
    };
    let (bytes, children) = match &mut **m {
        MNode::Leaf(es) => {
            let refs: Vec<(&[u8], &[u8])> = es
                .iter()
                .map(|(k, v)| (k.as_slice(), v.as_slice()))
                .collect();
            (encode(true, &refs), Vec::new())
        }
        MNode::Internal(es) => {
            let mut ids = Vec::with_capacity(es.len());
            for (_, k) in es.iter_mut() {
                ids.push(flush_kid(k, w)?);
            }
            let refs: Vec<(&[u8], &[u8])> = es
                .iter()
                .zip(&ids)
                .map(|((k, _), id)| (k.as_slice(), id.as_bytes().as_slice()))
                .collect();
            (encode(false, &refs), ids)
        }
    };
    let id = w.intern(&bytes, &children)?;
    *kid = Kid::Id(id);
    Ok(id)
}

/// Writes nodes and tracks reference-count changes for one transaction.
pub(crate) struct NodeWriter<'t> {
    pub(crate) nodes: NodesTable<'t>,
    pub(crate) refs: RefsTable<'t>,
    delta: HashMap<NodeId, i64>,
}

impl<'t> NodeWriter<'t> {
    pub(crate) fn new(nodes: NodesTable<'t>, refs: RefsTable<'t>) -> Self {
        Self {
            nodes,
            refs,
            delta: HashMap::new(),
        }
    }

    /// Stores a node if new (counting its children) and returns its id.
    fn intern(&mut self, bytes: &[u8], children: &[NodeId]) -> Result<NodeId> {
        let id = node::hash(bytes);
        if self.nodes.get(*id.as_bytes())?.is_none() {
            self.nodes.insert(*id.as_bytes(), bytes)?;
            for c in children {
                *self.delta.entry(*c).or_default() += 1;
            }
        }
        Ok(id)
    }

    pub(crate) fn add_ref(&mut self, id: NodeId) {
        *self.delta.entry(id).or_default() += 1;
    }

    pub(crate) fn drop_ref(&mut self, id: NodeId) {
        *self.delta.entry(id).or_default() -= 1;
    }

    /// Applies the accumulated count changes and frees nodes whose count reaches zero.
    pub(crate) fn settle(&mut self) -> Result<()> {
        let mut work: Vec<NodeId> = self.delta.keys().copied().collect();
        while let Some(id) = work.pop() {
            let d = self.delta.remove(&id).unwrap_or(0);
            if d == 0 {
                continue;
            }
            let cur = self.refs.get(*id.as_bytes())?.map_or(0, |g| g.value());
            let new = i64::try_from(cur).unwrap_or(i64::MAX) + d;
            if new < 0 {
                return Err(Error::Corrupt(format!(
                    "reference count of {id} underflows"
                )));
            }
            if new > 0 {
                self.refs.insert(*id.as_bytes(), new as u64)?;
                continue;
            }
            let bytes = self
                .nodes
                .remove(*id.as_bytes())?
                .map(|g| g.value().to_vec())
                .ok_or_else(|| Error::Corrupt(format!("freeing missing node {id}")))?;
            self.refs.remove(*id.as_bytes())?;
            let n = Node::parse(bytes)?;
            if !n.is_leaf() {
                for i in 0..n.len() {
                    let c = n.child(i);
                    *self.delta.entry(c).or_default() -= 1;
                    work.push(c);
                }
            }
        }
        Ok(())
    }
}

impl std::fmt::Debug for NodeWriter<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeWriter").finish_non_exhaustive()
    }
}

impl std::fmt::Debug for MemTree {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemTree").finish_non_exhaustive()
    }
}
