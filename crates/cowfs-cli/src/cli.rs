use clap::{Parser, Subcommand};
use clap_complete::Shell;
use std::path::PathBuf;

const AFTER_HELP: &str = "Exit codes:
  0    success
  1    the daemon returned an error, or another failure
  2    usage error
  3    the daemon is not running
  4    the daemon did not answer in time (see --timeout)
  130  interrupted (Ctrl-C)

Default socket: $XDG_RUNTIME_DIR/cowfs/control.sock, else <temp dir>/cowfs-<uid>/control.sock.
Environment: COWFS_SOCKET (like --socket), COWFS_TIMEOUT (like --timeout); empty means unset.";

/// The cowfs command line.
#[derive(Debug, Parser)]
#[command(name = "cowfs", version, about, after_help = AFTER_HELP)]
pub struct Cli {
    /// Control socket path (default: per-user, see below)
    #[arg(long, global = true, value_name = "PATH")]
    pub socket: Option<PathBuf>,
    /// Seconds without any reply before giving up (default 30; progress resets it)
    #[arg(long, global = true, value_name = "SECS", value_parser = clap::value_parser!(u64).range(1..))]
    pub timeout: Option<u64>,
    /// Print the response data as one JSON line on stdout; an error is `{"error": {...}}` on
    /// stdout too, and progress is `{"progress": {...}}` lines on stderr
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub command: Command,
}

/// Top-level commands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the control server
    Serve {
        /// Block store directory
        #[arg(long)]
        store: PathBuf,
        /// Mount point
        #[arg(long)]
        mount: PathBuf,
        /// Use the in-memory stub backend
        #[arg(long)]
        stub: bool,
        /// Pause between stub progress events, for testing cancellation
        #[arg(long, hide = true, default_value_t = 0, requires = "stub")]
        stub_delay_ms: u64,
        /// Make the stub ignore cancellation, for testing forced shutdown
        #[arg(long, hide = true, requires = "stub")]
        stub_ignore_cancel: bool,
    },
    /// Store and mount paths, counts and uptime
    Status,
    /// Manage snapshots
    Snapshot {
        #[command(subcommand)]
        command: SnapshotCommand,
    },
    /// Garbage-collect unreferenced blocks
    Gc {
        /// Report what would be freed without freeing it
        #[arg(long)]
        dry_run: bool,
    },
    /// Verify every block and snapshot
    Fsck,
    /// Ingest a directory into a new snapshot and verify it by hash
    Import {
        /// Directory to ingest, never written to
        dir: PathBuf,
        /// Name of the new snapshot (default: the directory's own name)
        #[arg(long, visible_alias = "store-name")]
        name: Option<String>,
    },
    /// Manage warm bases
    Base {
        #[command(subcommand)]
        command: BaseCommand,
    },
    /// List processes holding a snapshot directory (cwd, open file or lock)
    Ps {
        /// Snapshot name
        snapshot: String,
    },
    /// Where and how the store is mounted
    MountInfo,
    /// Stop the daemon
    Shutdown,
    /// Print a shell completion script
    Completions {
        /// Target shell
        shell: Shell,
    },
}

/// `cowfs snapshot ...`
#[derive(Debug, Subcommand)]
pub enum SnapshotCommand {
    /// List snapshots
    List,
    /// Create a snapshot: an O(1) clone of --from, or of the empty tree
    Create {
        /// New snapshot name
        name: String,
        /// Snapshot to clone
        #[arg(long)]
        from: Option<String>,
    },
    /// Remove a snapshot; fails busy while a process holds it
    Rm {
        /// Snapshot name
        name: String,
        /// Remove even if a process holds it
        #[arg(long)]
        force: bool,
    },
    /// Atomically replace a snapshot with a fresh clone of another; fails busy while a process
    /// holds it
    Reset {
        /// Snapshot to replace
        name: String,
        /// Snapshot to clone into its place
        #[arg(long)]
        from: String,
        /// Reset even if a process holds it
        #[arg(long)]
        force: bool,
    },
    /// Rename a snapshot
    Rename {
        /// Current name
        from: String,
        /// New name
        to: String,
    },
    /// Turn a clone into a base
    Promote {
        /// Snapshot name
        name: String,
    },
}

/// `cowfs base ...`
#[derive(Debug, Subcommand)]
pub enum BaseCommand {
    /// Build or refresh the warm base for a repository at a git ref
    Refresh {
        /// Repository path
        #[arg(long)]
        repo: PathBuf,
        /// Git ref to build from
        #[arg(long = "ref")]
        git_ref: String,
        /// Base snapshot name (default: derived from the repository)
        #[arg(long)]
        name: Option<String>,
    },
}

// ci-trial trial/edge-filtered (throwaway)
