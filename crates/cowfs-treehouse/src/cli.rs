use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;
use std::time::Duration;

use crate::error::{Env, Error, Result, EXIT_ERROR, EXIT_OK};
use crate::mode_a::{Doctor, ReturnOptions};
use crate::mode_b::{BaseRefresh, PromoteOptions, DEFAULT_NFS_TIMEOUT, DEFAULT_TREEHOUSE_TIMEOUT};
use crate::{connect, mode_a, mode_b, treehouse_bin, Daemon, Treehouse};

const AFTER_HELP: &str = "\
Exit codes:
  0    success
  1    the daemon returned an error, treehouse failed, or another failure
  2    usage error
  3    the cowfs daemon is not running
  4    the cowfs daemon did not answer in time
  5    the slot is held: busy from the daemon, or holders survived termination
  130  interrupted

Default socket: the same one `cowfs` uses. COWFS_SOCKET and COWFS_TIMEOUT work here too.
COWFS_TREEHOUSE_BIN overrides the treehouse binary; --treehouse-bin overrides that.";

/// The cowfs treehouse companion.
#[derive(Debug, Parser)]
#[command(name = "cowfs-treehouse", version, about, after_help = AFTER_HELP)]
pub struct Cli {
    /// Control socket path, like `cowfs --socket`
    #[arg(long, global = true, value_name = "PATH", env = "COWFS_SOCKET")]
    pub socket: Option<PathBuf>,
    /// Seconds without a reply before giving up, like `cowfs --timeout`
    #[arg(long, global = true, value_name = "SECS", env = "COWFS_TIMEOUT")]
    pub timeout: Option<u64>,
    /// Print one JSON object on stdout
    #[arg(long, global = true)]
    pub json: bool,
    /// The treehouse binary to drive
    #[arg(long, global = true, value_name = "PATH")]
    pub treehouse_bin: Option<PathBuf>,
    /// A sandbox HOME for treehouse, so its config and default pool stay out of the way
    #[arg(long, global = true, value_name = "DIR")]
    pub treehouse_home: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Command,
}

/// Top-level commands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Report what a mount needs before unmodified treehouse can use it
    Setup(SetupArgs),
    /// Check everything mode (a) needs
    Doctor(DoctorArgs),
    /// Return a slot, handling the holders `treehouse return` misses
    Return(ReturnArgs),
    /// Mode (b): acquire a snapshot-native slot
    Get(GetArgs),
    /// Mode (b): the `post_create` entry point
    Provision(ProvisionArgs),
    /// Mode (b): warm base operations
    Base {
        #[command(subcommand)]
        command: BaseCommand,
    },
    /// Mode (b): discard a slot and its snapshot
    Discard(ReturnArgs),
    /// Install the `post_create` hook into the treehouse user config
    Hooks {
        #[command(subcommand)]
        command: HooksCommand,
    },
    /// Print the pool id of a repository
    PoolId {
        /// Repository path
        repo: PathBuf,
    },
}

#[derive(Debug, Args)]
pub struct SetupArgs {
    /// The cowfs mount point
    #[arg(long)]
    pub mount: PathBuf,
    /// Treehouse root to keep on the mount; treehouse appends `.treehouse` to it
    #[arg(long)]
    pub pool_root: PathBuf,
    /// The main checkout to keep on the mount
    #[arg(long)]
    pub repo: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct DoctorArgs {
    /// The cowfs mount point
    #[arg(long)]
    pub mount: PathBuf,
    /// The treehouse root, without the `.treehouse` that treehouse appends
    #[arg(long)]
    pub pool_root: Option<PathBuf>,
    /// The main checkout
    #[arg(long)]
    pub repo: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct ReturnArgs {
    /// The slot directory, or the treehouse slot name
    #[arg(long)]
    pub slot: PathBuf,
    /// Treehouse root, always passed explicitly
    #[arg(long)]
    pub root: Option<PathBuf>,
    /// Terminate holders using treehouse's SIGTERM, 2s, SIGKILL policy
    #[arg(long)]
    pub force: bool,
    /// `a` reports holders and hands the slot back to treehouse. `b` also swaps the slot's
    /// snapshot to the warm base first, so treehouse's own reset runs against an empty tree.
    #[arg(long, default_value = "a", value_name = "MODE")]
    pub mode: String,
    /// How long to wait for `.nfs*` silly-rename dirt to clear
    #[arg(long, value_name = "SECS")]
    pub nfs_timeout: Option<u64>,
    /// How long treehouse itself may take
    #[arg(long, value_name = "SECS")]
    pub treehouse_timeout: Option<u64>,
}

#[derive(Debug, Args)]
pub struct GetArgs {
    /// The main checkout to acquire a slot for
    #[arg(long)]
    pub repo: PathBuf,
    /// Treehouse root, always passed explicitly
    #[arg(long)]
    pub root: Option<PathBuf>,
    /// Extra arguments for `treehouse get --lease`, after `--`
    #[arg(last = true)]
    pub extra: Vec<String>,
}

#[derive(Debug, Args)]
pub struct ProvisionArgs {
    /// The slot directory treehouse created
    #[arg(long)]
    pub slot: PathBuf,
    /// The pool id, when it is already known
    #[arg(long)]
    pub pool_id: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum BaseCommand {
    /// Build or refresh the warm base
    Refresh(BaseRefreshArgs),
    /// Say whether the warm base matches a ref
    Status {
        /// The main checkout
        #[arg(long)]
        repo: PathBuf,
        /// The git ref to compare against
        #[arg(long = "ref", default_value = "main")]
        git_ref: String,
    },
    /// Promote a snapshot to a base, explicitly
    Promote {
        /// The snapshot to promote
        #[arg(long)]
        snapshot: String,
        /// Only report whether it exists
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Debug, Args)]
pub struct BaseRefreshArgs {
    /// The main checkout
    #[arg(long)]
    pub repo: PathBuf,
    /// The git ref to build from
    #[arg(long = "ref", default_value = "main")]
    pub git_ref: String,
    /// Run this command in a leased treehouse slot before the refresh
    #[arg(long, value_name = "CMD")]
    pub build: Option<String>,
    /// Build in this already-leased slot instead of acquiring one
    #[arg(long)]
    pub slot: Option<PathBuf>,
    /// Treehouse root, needed only when --build leases its own slot
    #[arg(long)]
    pub root: Option<PathBuf>,
    /// Refuse per-slot compiler flags, which spike 6 measured to dirty every clone
    #[arg(long)]
    pub rustflags: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum HooksCommand {
    /// Install the `post_create` hook
    Install {
        /// HOME whose `.config/treehouse/config.toml` to write
        #[arg(long)]
        home: PathBuf,
        /// The command to install
        // `$PWD`, because stock treehouse 3.1.0 runs `post_create` with the worktree as its
        // working directory and sets no slot variable. The proposed upstream `pre_create` hook would
        // supply `TREEHOUSE_SLOT_PATH`, which is the better form once it exists.
        #[arg(long, default_value = "cowfs-treehouse provision --slot $PWD")]
        command: String,
    },
}

/// Runs the companion and returns the process exit code. `cowfs-cli` calls this so a future
/// `cowfs treehouse` subcommand parses nothing twice.
pub fn run(_env: Env, args: impl IntoIterator<Item = String>) -> i32 {
    let args: Vec<String> = args.into_iter().collect();
    // clap derives the program name and the usage line from argv[0], so it has to be present.
    // Without it the root command gets named after the first argument, and every subcommand then
    // resolves against the wrong command.
    let argv = std::iter::once(env!("CARGO_PKG_NAME").to_owned()).chain(args);
    let cli = match Cli::try_parse_from(argv) {
        Ok(c) => c,
        Err(e) => {
            let _ = e.print();
            return if e.use_stderr() {
                EXIT_USAGE_CODE
            } else {
                EXIT_OK
            };
        }
    };
    let env = Env {
        socket: cli.socket.clone(),
        timeout: cli.timeout,
        json: cli.json,
    };
    match dispatch(&cli, &env) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("cowfs-treehouse: {e}");
            e.exit_code()
        }
    }
}

const EXIT_USAGE_CODE: i32 = crate::error::EXIT_USAGE;

fn dispatch(cli: &Cli, env: &Env) -> Result<i32> {
    match &cli.command {
        Command::Setup(a) => {
            let setup = {
                let mut daemon = connect(env)?;
                mode_a::setup(&mut daemon, &a.mount, &a.pool_root, a.repo.as_deref())?
            };
            emit(env, &setup)?;
            Ok(EXIT_OK)
        }
        Command::Doctor(a) => {
            let mut daemon = connect(env)?;
            let report = mode_a::doctor(
                &mut daemon,
                &Doctor {
                    mount: a.mount.clone(),
                    pool_root: a.pool_root.clone(),
                    main_checkout: a.repo.clone(),
                },
            )?;
            report.print(env.json);
            Ok(report.exit_code())
        }
        Command::Return(a) | Command::Discard(a) => {
            let discard = matches!(cli.command, Command::Discard(_));
            let mode_b = discard || a.mode == "b";
            if a.mode != "a" && a.mode != "b" {
                return Err(Error::Usage(format!(
                    "--mode must be a or b, not {:?}",
                    a.mode
                )));
            }
            // Mode (a) needs no daemon, so nothing connects before the pool is named.
            let root = match a.root.clone() {
                Some(r) => r,
                None => {
                    return Err(Error::Usage(
                        "--root is required: name the treehouse pool explicitly, as \
                         `cowfs-treehouse return --slot <path> --root ~/.cowfs/mnt/th`"
                            .to_owned(),
                    ))
                }
            };
            let mut daemon = if mode_b { Some(connect(env)?) } else { None };
            let out = do_return(cli, &mut daemon, a, &root, mode_b, discard)?;
            emit(env, &out)?;
            Ok(EXIT_OK)
        }
        Command::Get(a) => {
            let mut daemon = connect(env)?;
            let th = treehouse_of(cli, a.root.as_deref())?;
            let out = mode_b::get(
                &mut daemon,
                &th,
                &mode_b::CowfsMaterialiser,
                &a.repo,
                &a.extra,
            )?;
            emit(env, &out)?;
            Ok(EXIT_OK)
        }
        Command::Provision(a) => {
            let mut daemon = connect(env)?;
            let materialiser = mode_b::CowfsMaterialiser;
            let out = mode_b::Provision {
                daemon: &mut daemon,
                materialiser: &materialiser,
                slot_path: a.slot.clone(),
                pool_id: a.pool_id.clone(),
            }
            .run()?;
            emit(env, &out)?;
            Ok(EXIT_OK)
        }
        Command::Base { command } => dispatch_base(cli, env, command),
        Command::Hooks { command } => match command {
            HooksCommand::Install { home, command } => {
                let action = mode_b::hooks_install(home, command)?;
                if env.json {
                    println!(
                        "{{\"action\":\"{}\"}}",
                        match &action {
                            mode_b::HookAction::Added => "added",
                            mode_b::HookAction::AlreadyThere => "already-there",
                            mode_b::HookAction::Refused(_) => "refused",
                        }
                    );
                } else {
                    println!("{action:?}");
                }
                Ok(if matches!(action, mode_b::HookAction::Refused(_)) {
                    EXIT_ERROR
                } else {
                    EXIT_OK
                })
            }
        },
        Command::PoolId { repo } => {
            let id = crate::pool_id(&crate::main_repo_root(repo)?)?;
            if env.json {
                emit(env, &id)?;
            } else {
                println!("{id}");
            }
            Ok(EXIT_OK)
        }
    }
}

fn dispatch_base(cli: &Cli, env: &Env, command: &BaseCommand) -> Result<i32> {
    match command {
        BaseCommand::Refresh(a) => {
            if a.rustflags.is_some() {
                return Err(Error::Usage(
                    "--rustflags is refused: spike 6 measured that a warm target/ built with a \
                     slot-specific --remap-path-prefix recompiles every unit, because RUSTFLAGS is \
                     part of cargo's fingerprint, so such a base dirties every slot it is cloned into"
                        .to_owned(),
                ));
            }
            let mut daemon = connect(env)?;
            let treehouse = match (&a.build, &a.root) {
                (Some(_), Some(root)) => Some(treehouse_of(cli, Some(root))?),
                _ => None,
            };
            let out = BaseRefresh {
                daemon: &mut daemon,
                treehouse: treehouse.as_ref(),
                repo: a.repo.clone(),
                git_ref: a.git_ref.clone(),
                build: a.build.clone(),
                slot: a.slot.clone(),
            }
            .run()?;
            emit(env, &out)?;
            Ok(EXIT_OK)
        }
        BaseCommand::Status { repo, git_ref } => {
            let mut daemon = connect(env)?;
            let out = mode_b::base_status(&mut daemon, repo, git_ref)?;
            emit(env, &out)?;
            Ok(if out.fresh { EXIT_OK } else { EXIT_ERROR })
        }
        BaseCommand::Promote { snapshot, dry_run } => {
            let mut daemon = connect(env)?;
            let out = mode_b::base_promote(
                &mut daemon,
                &PromoteOptions {
                    snapshot: snapshot.clone(),
                    dry_run: *dry_run,
                },
            )?;
            emit(env, &out)?;
            Ok(EXIT_OK)
        }
    }
}

fn do_return(
    cli: &Cli,
    daemon: &mut Option<Daemon>,
    a: &ReturnArgs,
    root: &std::path::Path,
    mode_b_snapshot: bool,
    discard: bool,
) -> Result<mode_a::ReturnOutcome> {
    let slot = resolve_slot(cli, root, &a.slot)?;
    let th = treehouse_of(cli, Some(root))?;
    let (snapshot, reset_to) = if mode_b_snapshot {
        let Some(daemon) = daemon.as_mut() else {
            return Err(Error::Usage(
                "--mode b needs a cowfs daemon, so pass --socket or start one".to_owned(),
            ));
        };
        let pool_id = match crate::pool_id_of_slot_path(&slot) {
            Some(id) => id,
            None => crate::pool_id(&crate::main_repo_root(&slot)?)?,
        };
        let name = slot
            .parent()
            .and_then(std::path::Path::file_name)
            .and_then(std::ffi::OsStr::to_str)
            .ok_or_else(|| {
                Error::Usage(format!("cannot read a slot name from {}", slot.display()))
            })?;
        let snapshot = crate::slot_snapshot(&pool_id, name)?;
        daemon.ensure_snapshot(&snapshot, None)?;
        (Some(snapshot), Some(crate::empty_snapshot(&pool_id)?))
    } else {
        (None, None)
    };
    mode_a::return_slot(
        daemon.as_mut(),
        &th,
        &ReturnOptions {
            slot,
            force: a.force,
            nfs_timeout: Duration::from_secs(
                a.nfs_timeout.unwrap_or(DEFAULT_NFS_TIMEOUT.as_secs()),
            ),
            snapshot,
            reset_to,
            drop_snapshot: discard,
            treehouse_timeout: Duration::from_secs(
                a.treehouse_timeout
                    .unwrap_or(DEFAULT_TREEHOUSE_TIMEOUT.as_secs()),
            ),
        },
    )
}

/// Turns a slot name into its path when the caller gave a name, and leaves a path alone.
fn resolve_slot(_cli: &Cli, root: &std::path::Path, slot: &std::path::Path) -> Result<PathBuf> {
    if slot.is_absolute() {
        // Exactly as the caller spelled it. treehouse records the path it was given and matches
        // later calls against that string, so canonicalising here makes its own lookup fail and
        // the slot reads as unleased.
        return Ok(slot.to_path_buf());
    }
    let name = slot.to_string_lossy().into_owned();
    let entries = std::fs::read_dir(root.join(".treehouse")).map_err(|e| {
        Error::Io(format!(
            "cannot read {}: {e}; give the slot as an absolute path",
            root.join(".treehouse").display()
        ))
    })?;
    for pool in entries.flatten() {
        let candidate = pool.path().join(&name);
        if candidate.is_dir() {
            return Ok(candidate);
        }
    }
    Err(Error::Usage(format!(
        "no slot {name:?} under {}; give the slot as an absolute path",
        root.join(".treehouse").display()
    )))
}

fn treehouse_of(cli: &Cli, root: Option<&std::path::Path>) -> Result<Treehouse> {
    let root = root.ok_or_else(|| {
        Error::Usage("--root is required: name the treehouse pool explicitly".to_owned())
    })?;
    Treehouse::new(
        treehouse_bin(cli.treehouse_bin.clone()),
        root,
        cli.treehouse_home.clone(),
    )
}

fn emit(env: &Env, value: &impl serde::Serialize) -> Result<()> {
    if env.json {
        println!(
            "{}",
            serde_json::to_string(value)
                .map_err(|e| Error::Io(format!("cannot render output: {e}")))?
        );
    }
    Ok(())
}
