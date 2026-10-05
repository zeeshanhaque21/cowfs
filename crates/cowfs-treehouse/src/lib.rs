//! The cowfs treehouse companion: mode (a) transparent mount checks and a `return` wrapper that
//! closes issue #20, and mode (b) snapshot-native slots built on a warm base.
//!
//! Contracts: `docs/v1-control-api.md` for the daemon, `docs/v1-treehouse.md` for treehouse
//! 3.1.0 and the gap list, `docs/spikes/5-treehouse-process-detection.md` for holder detection and
//! `docs/spikes/6-artifact-byte-identity.md` for the path rules mode (b) follows.
//!
//! Everything here goes through the control API and the `treehouse` binary. Nothing imports a
//! cowfs crate that is still being built, so the whole companion is testable against
//! `cowfs-ctl`'s `StubHandler` today.

mod ctl;
mod error;
mod holders;
mod mode_a;
mod mode_b;
mod naming;
mod report;
mod th;

pub use ctl::{poll_busy, Daemon, BUSY_POLL, DEFAULT_BUSY_TIMEOUT};
pub use error::{
    Env, Error, Result, EXIT_BUSY, EXIT_ERROR, EXIT_INTERRUPTED, EXIT_NOT_RUNNING, EXIT_OK,
    EXIT_TIMEOUT, EXIT_USAGE,
};
pub use holders::{
    alive, describe, nfs_entries, protected_ancestry, signalable, terminate, wait_for_nfs_clear,
    GRACE, POLL,
};
pub use mode_a::{
    doctor, flock_works, hardlink_works, pool_slots, return_slot, setup, AppleDouble, Doctor,
    ReturnOptions, ReturnOutcome, Setup,
};
pub use mode_b::{
    base_promote, base_status, get, hooks_install, plan_git_link, rewrite_git_link, run_build,
    user_config_path_for, worktree_git_dir, Acquired, BaseRefresh, BaseRefreshed, BaseStatus,
    Canonical, CowfsMaterialiser, GitLink, HookAction, LeaseGuard, Materialiser, PromoteOptions,
    Provision, RecordingMaterialiser, DEFAULT_NFS_TIMEOUT, DEFAULT_TREEHOUSE_TIMEOUT,
};
pub use naming::{
    assert_in_pool, base_snapshot, empty_snapshot, from_client_error, from_ctl_error,
    main_repo_root, main_snapshot, pool_id, pool_id_in_pool, pool_id_of_slot_path, pool_root_dir,
    pool_root_of, resolve_commit, short6, slot_of, slot_snapshot, validate_slot,
};
pub use report::{Check, Report};
pub use th::{default_bin, implicit_root, Lease, PoolEntry, Treehouse};

use std::path::PathBuf;

mod cli;
pub use cli::{run, Cli, Command};

/// Connects to the daemon described by `env`.
pub fn connect(env: &Env) -> Result<Daemon> {
    Daemon::connect(env.socket.as_deref(), env.timeout)
}

/// The treehouse binary from `--treehouse-bin`, falling back to the environment.
pub fn treehouse_bin(explicit: Option<PathBuf>) -> PathBuf {
    explicit.unwrap_or_else(default_bin)
}
