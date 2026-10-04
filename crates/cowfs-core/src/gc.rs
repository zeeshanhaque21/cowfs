//! The real reference side for `cowfs-gc`: exact pins and a barrier that holds the reference gate.
//!
//! Design: `docs/gc-core-integration.md`.

use std::sync::Arc;
use std::time::Duration;

use cowfs_gc::{Barrier, ExtraRoots, Gc, GcReport, Held, Options, RootsError};
use cowfs_store::BlockId;

use crate::gate::Gate;
use crate::{ControlError, Core};

/// How long a barrier waits for readers to leave before it gives up and the pack is kept.
const PATIENCE: Duration = Duration::from_secs(2);

/// `Core` as the collector's [`ExtraRoots`]. Holds a `Core` clone, so [`Core::close`] is `Stale`
/// until this is dropped.
#[derive(Clone, Debug)]
pub struct CoreRoots {
    core: Core,
}

impl CoreRoots {
    /// The mount these roots answer for.
    pub fn core(&self) -> &Core {
        &self.core
    }
}

struct CoreBarrier {
    gate: Arc<Gate>,
}

impl Barrier for CoreBarrier {
    fn take(&mut self) -> Option<Box<dyn Held>> {
        self.gate
            .take(PATIENCE)
            .map(|hold| Box::new(hold) as Box<dyn Held>)
    }
}

impl ExtraRoots for CoreRoots {
    fn pinned_blocks(&self) -> Result<Vec<BlockId>, RootsError> {
        self.core.pinned_blocks().map_err(|e| match e {
            ControlError::Busy => RootsError::Busy,
            _ => RootsError::Unavailable,
        })
    }

    fn reference_barrier(&self) -> Result<Option<Box<dyn Barrier>>, RootsError> {
        Ok(Some(Box::new(CoreBarrier {
            gate: Arc::clone(&self.core.inner.gate),
        })))
    }
}

/// A collector over a mount's own store and metadata, wired to its reference side.
///
/// Holds a `Core` clone: drop it before [`Core::close`].
#[derive(Debug)]
pub struct Collector {
    gc: Gc,
    roots: CoreRoots,
}

impl Collector {
    /// One cycle. A dry run if the options said so.
    pub fn collect(&self) -> cowfs_gc::Result<GcReport> {
        self.gc.collect(Some(&self.roots))
    }

    /// The collector, for `cancel`, `set_progress` and the hints.
    pub fn gc(&self) -> &Gc {
        &self.gc
    }

    /// The reference side, for a test that wraps it.
    pub fn roots(&self) -> &CoreRoots {
        &self.roots
    }
}

impl Core {
    /// A collector over this mount's store and metadata. Its state lives in `<root>/gc`.
    pub fn collector(&self, opts: Options) -> cowfs_gc::Result<Collector> {
        let gc = Gc::open(
            self.inner.root.join("gc"),
            self.inner.blocks.store_arc(),
            Arc::new(self.inner.meta.clone()),
            opts,
        )?;
        Ok(Collector {
            gc,
            roots: CoreRoots { core: self.clone() },
        })
    }

    /// The reference side alone, for a caller that builds its own [`Gc`] or wraps the roots.
    pub fn gc_roots(&self) -> CoreRoots {
        CoreRoots { core: self.clone() }
    }
}
