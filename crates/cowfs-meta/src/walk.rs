//! Walking a snapshot for the blocks it references, skipping subtrees already visited.

use crate::node::{Node, NodeId};
use crate::ptree::{NodeSource, RoNodes, TableSource};
use crate::types::{decode_chunks, K_CHUNK};
use crate::Result;
use cowfs_store::BlockId;
use std::collections::{HashSet, VecDeque};

/// Tree nodes already walked. Share one marker across snapshots to skip common subtrees.
#[derive(Debug, Default)]
pub struct Marker {
    seen: HashSet<NodeId>,
}

impl Marker {
    /// An empty marker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of fully walked nodes recorded.
    pub fn len(&self) -> usize {
        self.seen.len()
    }

    /// True when no node has been recorded.
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
}

struct Frame {
    id: NodeId,
    node: std::sync::Arc<Node>,
    idx: usize,
}

/// Iterator over the block ids referenced by a snapshot. See [`Snapshot::live_blocks`](crate::Snapshot::live_blocks).
///
/// A node is added to the marker only after everything below it was yielded, so dropping the
/// iterator early never makes the marker claim blocks that were not seen.
pub struct LiveBlocks<'m> {
    nodes: RoNodes,
    marker: &'m mut Marker,
    stack: Vec<Frame>,
    queue: VecDeque<BlockId>,
    failed: bool,
}

impl std::fmt::Debug for LiveBlocks<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveBlocks").finish_non_exhaustive()
    }
}

impl<'m> LiveBlocks<'m> {
    pub(crate) fn new(nodes: RoNodes, root: NodeId, marker: &'m mut Marker) -> Result<Self> {
        let mut it = Self {
            nodes,
            marker,
            stack: Vec::new(),
            queue: VecDeque::new(),
            failed: false,
        };
        it.push(root)?;
        Ok(it)
    }

    fn push(&mut self, id: NodeId) -> Result<()> {
        if !self.marker.seen.contains(&id) {
            let node = TableSource {
                table: &self.nodes,
                cache: None,
            }
            .node(&id)?;
            self.stack.push(Frame { id, node, idx: 0 });
        }
        Ok(())
    }

    fn step(&mut self) -> Result<Option<BlockId>> {
        loop {
            if let Some(b) = self.queue.pop_front() {
                return Ok(Some(b));
            }
            let Some(top) = self.stack.last_mut() else {
                return Ok(None);
            };
            if top.idx >= top.node.len() {
                let id = top.id;
                self.stack.pop();
                self.marker.seen.insert(id);
                continue;
            }
            let i = top.idx;
            top.idx += 1;
            if top.node.is_leaf() {
                let k = top.node.key(i);
                if k.get(8) == Some(&K_CHUNK) {
                    let refs = decode_chunks(top.node.val(i))?;
                    // the flag, not the id: a hole is not a block and this walker is the only thing
                    // a collector has to trust, so it must not hand out the sentinel
                    self.queue
                        .extend(refs.into_iter().filter_map(|c| (!c.hole).then_some(c.id)));
                }
            } else {
                let child = top.node.child(i);
                self.push(child)?;
            }
        }
    }
}

impl Iterator for LiveBlocks<'_> {
    type Item = Result<BlockId>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        match crate::error::guard(|| self.step()) {
            Ok(b) => b.map(Ok),
            Err(e) => {
                self.failed = true;
                Some(Err(e))
            }
        }
    }
}
