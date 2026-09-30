//! Persistent content-addressed B+tree over redb tables.
//!
//! Stored nodes are immutable. Edits happen on in-memory copies of the nodes on the touched paths
//! (`MemTree`); memory nodes are `Arc`-shared so a savepoint is one `clone`. `write` hashes and
//! stores the copies bottom-up without changing the tree, and the caller calls `reset` once the
//! enclosing transaction has committed.

use crate::db::NODES;
use crate::error::guard;
use crate::node::{self, encode, encoded_size, entry_cost, Node, NodeId};
use crate::{Error, Result};
use redb::{ReadOnlyTable, ReadableDatabase, ReadableTable, Table};
use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::{Arc, Mutex};

pub(crate) type NodesTable<'t> = Table<'t, [u8; 32], &'static [u8]>;
pub(crate) type RefsTable<'t> = Table<'t, [u8; 32], u64>;
pub(crate) type RoNodes = ReadOnlyTable<[u8; 32], &'static [u8]>;
pub(crate) type Entry = (Vec<u8>, Vec<u8>);

/// Read access to stored nodes by id.
pub(crate) trait NodeSource {
    fn read(&self, id: &NodeId) -> Result<Option<Vec<u8>>>;

    /// Loads, hash-verifies and parses a node (through the cache when there is one).
    fn node(&self, id: &NodeId) -> Result<Arc<Node>>;
}

fn verified(bytes: Vec<u8>, id: &NodeId) -> Result<Arc<Node>> {
    if node::hash(&bytes) != *id {
        return Err(Error::Corrupt(format!("tree node {id} fails its hash")));
    }
    Ok(Arc::new(Node::parse(bytes)?))
}

fn load_through(
    cache: &NodeCache,
    id: &NodeId,
    read: impl FnOnce() -> Result<Option<Vec<u8>>>,
) -> Result<Arc<Node>> {
    if let Some(n) = cache.get(id) {
        return Ok(n);
    }
    let bytes = read()?.ok_or_else(|| Error::Corrupt(format!("missing tree node {id}")))?;
    let n = verified(bytes, id)?;
    cache.put(*id, n.clone());
    Ok(n)
}

#[derive(Default)]
pub(crate) struct IdHasher(u64);

impl Hasher for IdHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, b: &[u8]) {
        if let Some(h) = b.get(..8).and_then(|s| <[u8; 8]>::try_from(s).ok()) {
            self.0 = u64::from_le_bytes(h);
        }
    }
}

type IdMap<V> = HashMap<NodeId, V, BuildHasherDefault<IdHasher>>;

const SHARDS: usize = 16;

struct Slot {
    id: NodeId,
    node: Arc<Node>,
    hot: bool,
}

#[derive(Default)]
struct Shard {
    map: IdMap<usize>,
    slots: Vec<Slot>,
    hand: usize,
}

/// Bounded cache of verified nodes with CLOCK eviction. Nodes are immutable and named by their
/// hash, so an entry never goes stale.
pub(crate) struct NodeCache {
    shards: Vec<Mutex<Shard>>,
    per_shard: usize,
}

impl NodeCache {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            shards: (0..SHARDS).map(|_| Mutex::new(Shard::default())).collect(),
            per_shard: capacity.div_ceil(SHARDS),
        }
    }

    fn shard(&self, id: &NodeId) -> std::sync::MutexGuard<'_, Shard> {
        let i = usize::from(id.as_bytes()[31]) % SHARDS;
        self.shards[i].lock().unwrap_or_else(|e| e.into_inner())
    }

    #[cfg(test)]
    pub(crate) fn clear_for_test(&self) {
        for s in &self.shards {
            *s.lock().unwrap_or_else(|e| e.into_inner()) = Shard::default();
        }
    }

    pub(crate) fn get(&self, id: &NodeId) -> Option<Arc<Node>> {
        if self.per_shard == 0 {
            return None;
        }
        let mut s = self.shard(id);
        let i = *s.map.get(id)?;
        s.slots[i].hot = true;
        Some(s.slots[i].node.clone())
    }

    pub(crate) fn put(&self, id: NodeId, node: Arc<Node>) {
        if self.per_shard == 0 {
            return;
        }
        let cap = self.per_shard;
        let mut s = self.shard(&id);
        if let Some(&i) = s.map.get(&id) {
            s.slots[i].hot = true;
            return;
        }
        if s.slots.len() < cap {
            let i = s.slots.len();
            s.slots.push(Slot {
                id,
                node,
                hot: false,
            });
            s.map.insert(id, i);
            return;
        }
        loop {
            let h = s.hand;
            s.hand = (h + 1) % cap;
            if s.slots[h].hot {
                s.slots[h].hot = false;
                continue;
            }
            let old = s.slots[h].id;
            s.map.remove(&old);
            s.slots[h] = Slot {
                id,
                node,
                hot: false,
            };
            s.map.insert(id, h);
            return;
        }
    }
}

impl std::fmt::Debug for NodeCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeCache").finish_non_exhaustive()
    }
}

/// Reads nodes from the latest committed state, opening a redb read view only on a cache miss.
pub(crate) struct Lazy<'a> {
    db: &'a redb::Database,
    cache: &'a NodeCache,
    table: OnceCell<RoNodes>,
}

impl<'a> Lazy<'a> {
    pub(crate) fn new(db: &'a redb::Database, cache: &'a NodeCache) -> Self {
        Self {
            db,
            cache,
            table: OnceCell::new(),
        }
    }

    fn table(&self) -> Result<&RoNodes> {
        if let Some(t) = self.table.get() {
            return Ok(t);
        }
        let t = guard(|| Ok(self.db.begin_read()?.open_table(NODES)?))?;
        Ok(self.table.get_or_init(|| t))
    }
}

impl NodeSource for Lazy<'_> {
    fn read(&self, id: &NodeId) -> Result<Option<Vec<u8>>> {
        let t = self.table()?;
        guard(|| Ok(t.get(*id.as_bytes())?.map(|g| g.value().to_vec())))
    }

    fn node(&self, id: &NodeId) -> Result<Arc<Node>> {
        load_through(self.cache, id, || self.read(id))
    }
}

/// Node source over an open read table (used by `check` and the block walker).
pub(crate) struct TableSource<'a> {
    pub(crate) table: &'a RoNodes,
    pub(crate) cache: Option<&'a NodeCache>,
}

impl NodeSource for TableSource<'_> {
    fn read(&self, id: &NodeId) -> Result<Option<Vec<u8>>> {
        Ok(self.table.get(*id.as_bytes())?.map(|g| g.value().to_vec()))
    }

    fn node(&self, id: &NodeId) -> Result<Arc<Node>> {
        match self.cache {
            Some(c) => load_through(c, id, || self.read(id)),
            None => {
                let bytes = self
                    .read(id)?
                    .ok_or_else(|| Error::Corrupt(format!("missing tree node {id}")))?;
                verified(bytes, id)
            }
        }
    }
}

/// Point lookup in a stored tree.
fn get_stored(src: &dyn NodeSource, root: &NodeId, key: &[u8]) -> Result<Option<Vec<u8>>> {
    let mut id = *root;
    loop {
        let n = src.node(&id)?;
        if n.is_leaf() {
            return Ok(n.search(key).ok().map(|i| n.val(i).to_vec()));
        }
        id = n.child(n.find_child(key));
    }
}

/// In-order cursor over a stored tree.
#[derive(Debug)]
pub(crate) struct Cursor {
    stack: Vec<(Arc<Node>, usize)>,
}

impl Cursor {
    /// Positions the cursor at the first entry whose key is at least `key`.
    pub(crate) fn seek(src: &dyn NodeSource, root: &NodeId, key: &[u8]) -> Result<Cursor> {
        let mut stack = Vec::new();
        let mut id = *root;
        loop {
            let n = src.node(&id)?;
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

    pub(crate) fn next(&mut self, src: &dyn NodeSource) -> Result<Option<Entry>> {
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
                        let n = src.node(&id)?;
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

#[derive(Clone)]
enum Kid {
    Id(NodeId),
    Mem(Arc<MNode>),
}

#[derive(Clone)]
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
    fn mem(&mut self, src: &dyn NodeSource) -> Result<&mut MNode> {
        if let Kid::Id(id) = self {
            let n = src.node(id)?;
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
            *self = Kid::Mem(Arc::new(m));
        }
        match self {
            Kid::Mem(m) => Ok(Arc::make_mut(m)),
            Kid::Id(_) => Err(Error::Corrupt("unreachable".into())),
        }
    }

    fn size(&mut self, src: &dyn NodeSource) -> Result<usize> {
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

/// A tree being edited in memory. Cloning is a savepoint: it shares every memory node.
#[derive(Clone)]
pub(crate) struct MemTree {
    root: Kid,
    max: usize,
    edits: u64,
    dirty_bytes: usize,
}

impl MemTree {
    pub(crate) fn new(root: NodeId, max: usize) -> Self {
        Self {
            root: Kid::Id(root),
            max,
            edits: 0,
            dirty_bytes: 0,
        }
    }

    pub(crate) fn empty(max: usize) -> Self {
        Self {
            root: Kid::Mem(Arc::new(MNode::Leaf(Vec::new()))),
            max,
            edits: 0,
            dirty_bytes: 0,
        }
    }

    /// True when the tree differs from its stored root.
    pub(crate) fn is_dirty(&self) -> bool {
        matches!(self.root, Kid::Mem(_))
    }

    pub(crate) fn edits(&self) -> u64 {
        self.edits
    }

    pub(crate) fn dirty_bytes(&self) -> usize {
        self.dirty_bytes
    }

    pub(crate) fn get(&self, src: &dyn NodeSource, key: &[u8]) -> Result<Option<Vec<u8>>> {
        get_kid(&self.root, src, key)
    }

    /// First entry with a key at least `key`.
    pub(crate) fn seek_ge(&self, src: &dyn NodeSource, key: &[u8]) -> Result<Option<Entry>> {
        let mut out = Vec::new();
        scan_kid(&self.root, src, key, &[], 1, &mut out)?;
        Ok(out.pop())
    }

    /// Up to `limit` entries with keys at least `from` that start with `prefix`, in key order.
    pub(crate) fn scan(
        &self,
        src: &dyn NodeSource,
        from: &[u8],
        prefix: &[u8],
        limit: usize,
    ) -> Result<Vec<Entry>> {
        let mut out = Vec::new();
        scan_kid(&self.root, src, from, prefix, limit, &mut out)?;
        Ok(out)
    }

    pub(crate) fn insert(&mut self, src: &dyn NodeSource, key: &[u8], val: Vec<u8>) -> Result<()> {
        self.edits += 1;
        self.dirty_bytes += key.len() + val.len() + 8;
        let extra = insert_kid(&mut self.root, src, key, val, self.max)?;
        if !extra.is_empty() {
            let old = std::mem::replace(&mut self.root, Kid::Id(NodeId::from_bytes([0; 32])));
            let mut es = vec![(Vec::new(), old)];
            es.extend(extra);
            self.root = Kid::Mem(Arc::new(MNode::Internal(es)));
        }
        Ok(())
    }

    pub(crate) fn remove(&mut self, src: &dyn NodeSource, key: &[u8]) -> Result<bool> {
        if self.get(src, key)?.is_none() {
            return Ok(false);
        }
        self.edits += 1;
        self.dirty_bytes += key.len() + 8;
        remove_kid(&mut self.root, src, key, self.max)?;
        while let MNode::Internal(es) = self.root.mem(src)? {
            match es.len() {
                0 => self.root = Kid::Mem(Arc::new(MNode::Leaf(Vec::new()))),
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

    /// The root id this tree would have if written now. Writes nothing.
    pub(crate) fn root_id(&self) -> NodeId {
        dry_kid(&self.root)
    }

    /// Stores every modified node and returns the new root id. The tree is left as it was, so a
    /// failed transaction loses nothing; call `reset` after the commit succeeds.
    pub(crate) fn write(&self, w: &mut NodeWriter<'_>) -> Result<NodeId> {
        write_kid(&self.root, w)
    }

    /// Makes `root` the stored root and drops the in-memory copies.
    pub(crate) fn reset(&mut self, root: NodeId) {
        self.root = Kid::Id(root);
        self.dirty_bytes = 0;
    }
}

fn get_kid(kid: &Kid, src: &dyn NodeSource, key: &[u8]) -> Result<Option<Vec<u8>>> {
    match kid {
        Kid::Id(id) => get_stored(src, id, key),
        Kid::Mem(m) => match &**m {
            MNode::Leaf(es) => Ok(es
                .binary_search_by(|(k, _)| k.as_slice().cmp(key))
                .ok()
                .map(|i| es[i].1.clone())),
            MNode::Internal(es) => get_kid(&es[route(es, key)].1, src, key),
        },
    }
}

/// Appends entries at or after `from` to `out` while they start with `prefix` and `out` is shorter
/// than `limit`. Returns true when the scan is finished (limit or prefix range left behind).
fn scan_kid(
    kid: &Kid,
    src: &dyn NodeSource,
    from: &[u8],
    prefix: &[u8],
    limit: usize,
    out: &mut Vec<Entry>,
) -> Result<bool> {
    match kid {
        Kid::Id(id) => {
            let mut cur = Cursor::seek(src, id, from)?;
            while out.len() < limit {
                match cur.next(src)? {
                    Some((k, v)) if k.starts_with(prefix) => out.push((k, v)),
                    Some(_) => return Ok(true),
                    None => return Ok(false),
                }
            }
            Ok(true)
        }
        Kid::Mem(m) => match &**m {
            MNode::Leaf(es) => {
                let start = es.partition_point(|(k, _)| k.as_slice() < from);
                for (k, v) in &es[start..] {
                    if out.len() >= limit {
                        return Ok(true);
                    }
                    if !k.starts_with(prefix) {
                        return Ok(true);
                    }
                    out.push((k.clone(), v.clone()));
                }
                Ok(out.len() >= limit)
            }
            MNode::Internal(es) => {
                for (_, child) in &es[route(es, from)..] {
                    if out.len() >= limit || scan_kid(child, src, from, prefix, limit, out)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
        },
    }
}

fn insert_kid(
    kid: &mut Kid,
    src: &dyn NodeSource,
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
                out.push((piece[0].0.clone(), Kid::Mem(Arc::new(MNode::Leaf(piece)))));
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
                    Kid::Mem(Arc::new(MNode::Internal(piece))),
                ));
            }
        }
    }
    Ok(out)
}

fn remove_kid(kid: &mut Kid, src: &dyn NodeSource, key: &[u8], max: usize) -> Result<()> {
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

fn fix_child(
    es: &mut Vec<(Vec<u8>, Kid)>,
    ci: usize,
    src: &dyn NodeSource,
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
    let right = Arc::try_unwrap(right).unwrap_or_else(|a| (*a).clone());
    match (es[l].1.mem(src)?, right) {
        (MNode::Leaf(a), MNode::Leaf(b)) => a.extend(b),
        (MNode::Internal(a), MNode::Internal(b)) => a.extend(b),
        _ => return Err(Error::Corrupt("sibling nodes differ in level".into())),
    }
    Ok(())
}

fn encode_mem(
    m: &MNode,
    mut child_id: impl FnMut(&Kid) -> Result<NodeId>,
) -> Result<(Vec<u8>, Vec<NodeId>)> {
    match m {
        MNode::Leaf(es) => {
            let refs: Vec<(&[u8], &[u8])> = es
                .iter()
                .map(|(k, v)| (k.as_slice(), v.as_slice()))
                .collect();
            Ok((encode(true, &refs), Vec::new()))
        }
        MNode::Internal(es) => {
            let mut ids = Vec::with_capacity(es.len());
            for (_, k) in es {
                ids.push(child_id(k)?);
            }
            let refs: Vec<(&[u8], &[u8])> = es
                .iter()
                .zip(&ids)
                .map(|((k, _), id)| (k.as_slice(), id.as_bytes().as_slice()))
                .collect();
            Ok((encode(false, &refs), ids))
        }
    }
}

fn dry_kid(kid: &Kid) -> NodeId {
    match kid {
        Kid::Id(id) => *id,
        Kid::Mem(m) => match encode_mem(m, |k| Ok(dry_kid(k))) {
            Ok((bytes, _)) => node::hash(&bytes),
            Err(_) => NodeId::from_bytes([0; 32]),
        },
    }
}

fn write_kid(kid: &Kid, w: &mut NodeWriter<'_>) -> Result<NodeId> {
    match kid {
        Kid::Id(id) => Ok(*id),
        Kid::Mem(m) => {
            let (bytes, children) = encode_mem(m, |k| write_kid(k, w))?;
            w.intern(&bytes, &children)
        }
    }
}

/// Writes nodes and tracks reference-count changes for one transaction.
pub(crate) struct NodeWriter<'t> {
    pub(crate) nodes: NodesTable<'t>,
    pub(crate) refs: RefsTable<'t>,
    delta: HashMap<NodeId, i64>,
    cache: Arc<NodeCache>,
}

impl<'t> NodeWriter<'t> {
    pub(crate) fn new(nodes: NodesTable<'t>, refs: RefsTable<'t>, cache: Arc<NodeCache>) -> Self {
        Self {
            nodes,
            refs,
            delta: HashMap::new(),
            cache,
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
            self.cache.put(id, Arc::new(Node::parse(bytes.to_vec())?));
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
    ///
    /// Frees are resolved in memory first, so an untouched child that gains a reference from a new
    /// parent and loses one from the freed old parent nets to zero and is never written.
    pub(crate) fn settle(&mut self) -> Result<()> {
        let mut freed: HashSet<NodeId> = HashSet::new();
        let mut work: Vec<NodeId> = self
            .delta
            .iter()
            .filter(|(_, d)| **d < 0)
            .map(|(id, _)| *id)
            .collect();
        while let Some(id) = work.pop() {
            let d = self.delta.get(&id).copied().unwrap_or(0);
            if d >= 0 || freed.contains(&id) {
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
                continue;
            }
            let bytes = self
                .nodes
                .remove(*id.as_bytes())?
                .map(|g| g.value().to_vec())
                .ok_or_else(|| Error::Corrupt(format!("freeing missing node {id}")))?;
            self.refs.remove(*id.as_bytes())?;
            self.delta.remove(&id);
            freed.insert(id);
            let n = Node::parse(bytes)?;
            if !n.is_leaf() {
                for i in 0..n.len() {
                    let c = n.child(i);
                    *self.delta.entry(c).or_default() -= 1;
                    work.push(c);
                }
            }
        }
        for (id, d) in std::mem::take(&mut self.delta) {
            if d == 0 {
                continue;
            }
            let cur = self.refs.get(*id.as_bytes())?.map_or(0, |g| g.value());
            let new = i64::try_from(cur).unwrap_or(i64::MAX) + d;
            let new = u64::try_from(new)
                .ok()
                .filter(|n| *n > 0)
                .ok_or_else(|| Error::Corrupt(format!("reference count of {id} left invalid")))?;
            self.refs.insert(*id.as_bytes(), new)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u8) -> NodeId {
        let mut b = [0u8; 32];
        b[0] = n;
        NodeId::from_bytes(b)
    }

    fn node() -> Arc<Node> {
        Arc::new(Node::parse(encode(true, &[])).unwrap())
    }

    #[test]
    fn clock_evicts_cold_entries_and_keeps_hot_ones() {
        let cache = NodeCache::new(SHARDS * 4);
        for n in 0..4 {
            cache.put(id(n), node());
        }
        assert!(cache.get(&id(0)).is_some());
        cache.put(id(9), node());
        assert!(cache.get(&id(0)).is_some(), "the hot entry was evicted");
        assert!(cache.get(&id(1)).is_none(), "the first cold entry stays");
        for n in 10..100 {
            cache.put(id(n), node());
        }
        let held = (0..100u8).filter(|n| cache.get(&id(*n)).is_some()).count();
        assert!(held <= 4, "cache grew past its bound: {held}");
        assert!(held >= 3, "cache emptied itself: {held}");
    }
}
