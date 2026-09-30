//! NFSv3 server library, vendored from huggingface/nfsserve 0.11.0. Changes: `PATCHES.md`.
#![allow(non_camel_case_types, clippy::upper_case_acronyms)]

mod context;
mod reply_cache;
mod rpc;
mod rpcwire;
pub mod xdr;

mod mount;
mod mount_handlers;

mod portmap;
mod portmap_handlers;

pub mod nfs;
mod nfs_handlers;
pub use nfs_handlers::take_stats;

pub mod tcp;
pub mod vfs;
