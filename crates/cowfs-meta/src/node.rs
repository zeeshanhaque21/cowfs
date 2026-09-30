//! Immutable content-addressed B+tree nodes: encoding, validation, zero-copy search.

use crate::{Error, Result};
use std::fmt;

/// Content id of a tree node: BLAKE3 of its encoded bytes.
///
/// An internal node's bytes contain its children's ids, so an id commits to the whole subtree.
/// The id of a snapshot's root node is its Merkle root.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId([u8; 32]);

impl NodeId {
    /// The raw hash bytes.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub(crate) const fn from_bytes(b: [u8; 32]) -> Self {
        Self(b)
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|b| write!(f, "{b:02x}"))
    }
}

impl fmt::Debug for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NodeId({self})")
    }
}

const HEADER: usize = 5;
const TAG_LEAF: u8 = 1;
const TAG_INTERNAL: u8 = 2;

pub(crate) fn hash(bytes: &[u8]) -> NodeId {
    let mut h = blake3::Hasher::new_derive_key("cowfs-meta node v1");
    h.update(bytes);
    NodeId(*h.finalize().as_bytes())
}

/// Encoded size of a node holding entries of the given key and value lengths.
pub(crate) fn encoded_size(entries: usize, payload: usize) -> usize {
    HEADER + 4 * (entries + 1) + payload
}

/// Per-entry encoded cost: length prefix, key, value, offset slot.
pub(crate) fn entry_cost(key: usize, val: usize) -> usize {
    2 + key + val + 4
}

/// Encodes a node. Leaf values are arbitrary bytes; internal values are 32-byte child ids.
pub(crate) fn encode(leaf: bool, entries: &[(&[u8], &[u8])]) -> Vec<u8> {
    let n = entries.len();
    let header = HEADER + 4 * (n + 1);
    let total = header
        + entries
            .iter()
            .map(|(k, v)| 2 + k.len() + v.len())
            .sum::<usize>();
    let mut out = Vec::with_capacity(total);
    out.push(if leaf { TAG_LEAF } else { TAG_INTERNAL });
    out.extend((n as u32).to_le_bytes());
    let mut off = header;
    out.extend((off as u32).to_le_bytes());
    for (k, v) in entries {
        off += 2 + k.len() + v.len();
        out.extend((off as u32).to_le_bytes());
    }
    for (k, v) in entries {
        out.extend((k.len() as u16).to_le_bytes());
        out.extend_from_slice(k);
        out.extend_from_slice(v);
    }
    out
}

fn u32_at(b: &[u8], pos: usize) -> Option<usize> {
    let s = b.get(pos..pos.checked_add(4)?)?;
    Some(u32::from_le_bytes(<[u8; 4]>::try_from(s).ok()?) as usize)
}

/// A validated, decoded-in-place node.
#[derive(Debug)]
pub(crate) struct Node {
    bytes: Vec<u8>,
    n: usize,
    leaf: bool,
}

impl Node {
    pub(crate) fn parse(bytes: Vec<u8>) -> Result<Node> {
        let bad = |why: &str| Error::Corrupt(format!("malformed tree node: {why}"));
        let leaf = match bytes.first() {
            Some(&TAG_LEAF) => true,
            Some(&TAG_INTERNAL) => false,
            _ => return Err(bad("tag")),
        };
        let n = u32_at(&bytes, 1).ok_or_else(|| bad("count"))?;
        let header = n
            .checked_mul(4)
            .and_then(|x| x.checked_add(HEADER + 4))
            .ok_or_else(|| bad("count"))?;
        if header > bytes.len() {
            return Err(bad("header"));
        }
        let mut prev = header;
        if u32_at(&bytes, HEADER) != Some(header) {
            return Err(bad("first offset"));
        }
        for i in 0..n {
            let end = u32_at(&bytes, HEADER + 4 * (i + 1)).ok_or_else(|| bad("offset"))?;
            let len = end.checked_sub(prev).ok_or_else(|| bad("offset order"))?;
            if end > bytes.len() || len < 2 {
                return Err(bad("entry bounds"));
            }
            let klen = usize::from(u16::from_le_bytes([bytes[prev], bytes[prev + 1]]));
            if 2 + klen > len || (!leaf && len - 2 - klen != 32) {
                return Err(bad("entry shape"));
            }
            prev = end;
        }
        if prev != bytes.len() {
            return Err(bad("trailing bytes"));
        }
        Ok(Node { bytes, n, leaf })
    }

    pub(crate) fn len(&self) -> usize {
        self.n
    }

    pub(crate) fn is_leaf(&self) -> bool {
        self.leaf
    }

    fn off(&self, i: usize) -> usize {
        u32_at(&self.bytes, HEADER + 4 * i).unwrap_or(0)
    }

    fn entry(&self, i: usize) -> &[u8] {
        &self.bytes[self.off(i)..self.off(i + 1)]
    }

    fn klen(&self, i: usize) -> usize {
        let e = self.entry(i);
        usize::from(u16::from_le_bytes([e[0], e[1]]))
    }

    pub(crate) fn key(&self, i: usize) -> &[u8] {
        &self.entry(i)[2..2 + self.klen(i)]
    }

    pub(crate) fn val(&self, i: usize) -> &[u8] {
        &self.entry(i)[2 + self.klen(i)..]
    }

    pub(crate) fn child(&self, i: usize) -> NodeId {
        let mut id = [0u8; 32];
        id.copy_from_slice(self.val(i));
        NodeId(id)
    }

    /// Binary search over keys.
    pub(crate) fn search(&self, key: &[u8]) -> std::result::Result<usize, usize> {
        let (mut lo, mut hi) = (0, self.n);
        while lo < hi {
            let mid = (lo + hi) / 2;
            match self.key(mid).cmp(key) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return Ok(mid),
            }
        }
        Err(lo)
    }

    /// Index of the child that covers `key` (entry 0's key is ignored).
    pub(crate) fn find_child(&self, key: &[u8]) -> usize {
        let (mut lo, mut hi) = (1, self.n);
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.key(mid) <= key {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo.saturating_sub(1).min(self.n.saturating_sub(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_leaf_and_internal() {
        let e: Vec<(&[u8], &[u8])> = vec![(b"a", b"1"), (b"b", b""), (b"cc", b"333")];
        let n = Node::parse(encode(true, &e)).unwrap();
        assert_eq!(n.len(), 3);
        assert_eq!(n.key(2), b"cc");
        assert_eq!(n.val(0), b"1");
        assert_eq!(n.search(b"b"), Ok(1));
        assert_eq!(n.search(b"bb"), Err(2));
        let id = [7u8; 32];
        let i: Vec<(&[u8], &[u8])> = vec![(b"", &id), (b"m", &id), (b"t", &id)];
        let n = Node::parse(encode(false, &i)).unwrap();
        assert_eq!(n.find_child(b"a"), 0);
        assert_eq!(n.find_child(b"m"), 1);
        assert_eq!(n.find_child(b"s"), 1);
        assert_eq!(n.find_child(b"z"), 2);
        assert_eq!(n.child(1), NodeId(id));
    }

    #[test]
    fn empty_leaf_parses() {
        let n = Node::parse(encode(true, &[])).unwrap();
        assert_eq!(n.len(), 0);
        assert_eq!(n.search(b"x"), Err(0));
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        let good = encode(true, &[(b"k", b"v")]);
        for cut in 0..good.len() {
            assert!(Node::parse(good[..cut].to_vec()).is_err());
        }
        for i in 0..good.len() {
            for bit in 0..8 {
                let mut b = good.clone();
                b[i] ^= 1 << bit;
                let _ = Node::parse(b);
            }
        }
    }
}
