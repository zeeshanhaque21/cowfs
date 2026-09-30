//! Metadata: inode tables, directories, Merkle tree, snapshots. Design: `docs/v1-meta.md`.
//!
//! A [`Meta`] is one redb file holding any number of [`Snapshot`]s. Each snapshot is a persistent,
//! content-addressed B+tree over inode, directory, xattr and chunk-list records, so a snapshot
//! costs one row and its root hash is a Merkle root over everything in it.

mod check;
mod db;
mod error;
mod node;
mod ptree;
mod read;
mod tx;
mod types;
mod walk;

pub use cowfs_store::{BlockId, ChunkRef};
pub use db::{Meta, Options, Snapshot, SyncHook};
pub use error::{Error, Result};
pub use node::NodeId;
pub use tx::Tx;
pub use types::{
    Attr, DirEntry, FileType, Ino, ReadDir, Removed, SetAttr, SnapshotId, SnapshotInfo, Timestamp,
    CHUNKS_PER_SEGMENT, NAME_MAX, ROOT_INO, SYMLINK_MAX, XATTR_MAX,
};
pub use walk::{LiveBlocks, Marker};
