//! Real-project acceptance for treehouse mode (b): a real project, a real `cowfs-core` daemon, a
//! real mount, the real companion binary, and real exit codes.
//!
//! # This suite is not an acceptance, and it does not report one
//!
//! Mode (b) needs a **published warm base**: a snapshot the daemon can later find, with the
//! repository, the ref and the commit it was built from. That step does not exist at this lane's
//! base. The chain, in the order it has to be broken:
//!
//! 1. `crates/cowfs-daemon/src/handler.rs` `base_refresh` calls `can_ingest()?` before anything
//!    else, and `can_ingest` answers `unsupported` unless `backend.ingests_directories()`.
//!    `CoreBackend::ingests_directories()` is `false` by design, documented on the method itself.
//!    So on the core the call is refused before any publication or persistence is attempted.
//! 2. Only then does `crates/cowfs-daemon/src/import.rs` matter: it found the checkout by reading
//!    the last line of `git worktree add --detach <commit>` stdout, and no line of that stdout is a
//!    path on the git this host has. Reproduced here.
//! 3. Only after a base is published does provenance matter: `base status` has to report the repo,
//!    the ref, the commit and `fresh` for `find_base` to discover it.
//!
//! Each link has a test that pins it against the real binaries, so closing one turns a green test
//! red and forces the acceptance to be updated rather than silently bypassed.
//!
//! What is proven over the same real core daemon is everything the core *does* implement: a
//! verified ingest of a real project, a promoted base, an O(1) fork whose id is distinct and whose
//! parent is the base, a real `mount_snapshot` export at a treehouse-shaped slot path, a writable
//! export, a real `cargo build` and `cargo test` of the project inside that export, a reset that
//! returns the slot to a byte-identical untouched base, and the cache hook installed and read back.
//!
//! # Honesty rules this file enforces on itself
//!
//! - A missing sibling binary is always a hard failure. Under `cargo test --workspace` the
//!   workspace binaries are built, so their absence means the command was wrong, not the host.
//! - A missing mount capability is a recorded capability skip, never a silent pass: it writes
//!   `outcome=skipped-capability` into the receipt, prints that nothing was measured, and
//!   `the_acceptance_receipt_states_what_was_measured` checks that claim against the receipt.
//!   `COWFS_ACCEPTANCE_REQUIRED=1` turns both into failures, and is the only mode that can produce
//!   acceptance.
//! - Every teardown command is bounded by one absolute deadline taken before the first spawn, the
//!   native mount table is read as a tri-state so an unreadable table quarantines instead of
//!   looking clean, and nothing recursive is ever deleted.
//! - No process group is signalled, no `pkill`, no `abort`, and the temporary tree is never left to
//!   a `Drop` that could walk a live mount point.
//!
//! Sample project: this repository at the commit under test. Real project, real dependencies, real
//! tests, small enough that nothing copies a corpus. Because the harness is part of that tree, every
//! edit to this file changes the corpus it measures, so every receipt records the commit it ran at.

mod common;

use common::private_tempdir;
use cowfs_ctl::{Client, ClientOptions, MountSnapshot, Request, Response, UnmountSnapshot};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

/// How a gate ended. Written to the receipt so a skip can never read as a measurement.
const MEASURED: &str = "measured";
const SKIPPED_CAPABILITY: &str = "skipped-capability";

/// Whether the host must be able to measure, or a capability skip is tolerated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// `COWFS_ACCEPTANCE_REQUIRED=1`: a missing binary or a missing mount is a failure.
    Required,
    /// Default: a missing binary still fails, a missing mount is recorded and announced.
    BestEffort,
}

fn mode() -> Mode {
    match std::env::var("COWFS_ACCEPTANCE_REQUIRED") {
        Ok(v) if v == "1" => Mode::Required,
        _ => Mode::BestEffort,
    }
}

/// Why a gate could not run.
#[derive(Clone, Debug)]
struct Skip {
    what: &'static str,
    why: String,
}

/// The workspace root, which is also the sample project.
fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("the crate is inside the workspace")
        .to_path_buf()
}

/// A sibling of the test binary, which is where cargo puts the binaries it built with it.
fn sibling_bin(name: &str) -> Option<PathBuf> {
    let mut path = std::env::current_exe().ok()?;
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    let candidate = path.join(name);
    candidate.is_file().then_some(candidate)
}

/// A sibling binary, or a hard failure.
///
/// This is the fix for the silent-skip defect: the five daemon gates used to return `Ok(())` when
/// `cowfs-daemon` was absent, which libtest reports as `ok`. A missing binary is a wrong
/// invocation, not a host limitation, so it fails whatever the mode is.
fn require_bin(name: &'static str) -> PathBuf {
    match sibling_bin(name) {
        Some(p) => p,
        None => panic!(
            "{name} is not beside this test binary at {}. Run the suite from the workspace \
             (`cargo test --workspace` or `cargo build -p cowfs-daemon -p cowfs-cli \
             -p cowfs-treehouse --bins` first); a missing binary is never a pass.",
            std::env::current_exe()
                .unwrap_or_else(|_| PathBuf::from("<unknown>"))
                .display(),
        ),
    }
}

/// Where raw evidence is appended, one flushed JSON object per record.
fn evidence_dir() -> PathBuf {
    let dir = match std::env::var_os("COWFS_ACCEPTANCE_EVIDENCE") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => workspace().join("bench/out/ready-real-project"),
    };
    std::fs::create_dir_all(&dir).expect("the evidence directory is creatable");
    dir
}

/// One absolute deadline for a phase. Every command in that phase is bounded by it, and it is
/// taken once before the first spawn rather than per command, so a phase cannot extend itself.
#[derive(Clone, Copy, Debug)]
struct Deadline(Instant);

impl Deadline {
    fn after(secs: u64) -> Deadline {
        Deadline(Instant::now() + Duration::from_secs(secs))
    }

    fn remaining(&self) -> Duration {
        self.0.saturating_duration_since(Instant::now())
    }

    fn expired(&self) -> bool {
        self.remaining().is_zero()
    }
}

/// Runs one command under an absolute deadline, killing only its own child if the deadline passes.
///
/// No timeout argument exists for `umount` on macOS, and `umount` against a server that is gone is
/// the case that hangs, so every teardown command goes through here.
fn run_bounded(
    deadline: Deadline,
    dir: &Path,
    program: &str,
    args: &[&str],
    cap: Duration,
) -> Result<Output, String> {
    let left = deadline.remaining().min(cap);
    if left.is_zero() {
        return Err(format!("deadline already spent before spawning {program}"));
    }
    let started = Instant::now();
    let mut child = Command::new(program)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot spawn {program} {args:?}: {e}"))?;
    // Drained while the child runs. Draining only after it exits deadlocks any command whose
    // output fills the pipe buffer, which every importing cowfs call does with its progress frames.
    let drain = |pipe: Option<std::process::ChildStdout>| {
        pipe.map(|mut p| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let _ = p.read_to_end(&mut buf);
                buf
            })
        })
    };
    let out_reader = drain(child.stdout.take());
    let err_reader = child.stderr.take().map(|mut p| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = p.read_to_end(&mut buf);
            buf
        })
    });
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() >= left => {
                let pid = child.id();
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{program} {args:?} exceeded its {}ms bound and its own pid {pid} was killed",
                    left.as_millis()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(e) => return Err(format!("cannot wait for {program}: {e}")),
        }
    }
    let out = out_reader.and_then(|h| h.join().ok()).unwrap_or_default();
    let err = err_reader.and_then(|h| h.join().ok()).unwrap_or_default();
    let status = child
        .wait()
        .map_err(|e| format!("cannot reap {program}: {e}"))?;
    Ok(Output {
        status,
        stdout: out,
        stderr: err,
    })
}

/// `run_bounded` with a panic on failure, for read-only commands in a test body.
fn sh(deadline: Deadline, dir: &Path, program: &str, args: &[&str]) -> Output {
    match run_bounded(deadline, dir, program, args, Duration::from_secs(120)) {
        Ok(o) => o,
        Err(e) => panic!("{e}"),
    }
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap_or(-1)
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Appends one record and flushes it, so an interrupted run keeps what it measured.
fn record(test: &str, fields: &[(&str, String)]) {
    let mut body = String::from("{\"test\":");
    body.push_str(&serde_json::to_string(test).expect("a test name is a string"));
    for (k, v) in fields {
        body.push(',');
        body.push_str(&serde_json::to_string(k).expect("a key is a string"));
        body.push(':');
        body.push_str(&serde_json::to_string(v).expect("a value is a string"));
    }
    body.push_str("}\n");
    let path = evidence_dir().join("acceptance.jsonl");
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .expect("the evidence file is writable");
    f.write_all(body.as_bytes()).expect("the record is written");
    f.flush().expect("the record is flushed");
    eprintln!("ACCEPTANCE {body}");
}

/// Records a capability skip so it can never be read as a measurement.
fn record_skip(test: &str, skip: &Skip) {
    record(
        test,
        &[
            ("outcome", SKIPPED_CAPABILITY.to_owned()),
            ("skipped_what", skip.what.to_owned()),
            ("skipped_why", skip.why.clone()),
            ("mode", format!("{:?}", mode())),
        ],
    );
    eprintln!(
        "ACCEPTANCE NOT MEASURED: {test} did not run ({}): {}. A capability skip is not a pass.",
        skip.what, skip.why
    );
}

/// SHA-256 of a file, or a marker when it cannot be read.
fn digest(path: &Path) -> String {
    let Ok(bytes) = std::fs::read(path) else {
        return "absent".to_owned();
    };
    let mut h = Sha256::new();
    h.update(&bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// The identity of a binary and the tree it came from, so a receipt says which daemon answered.
fn binary_manifest(deadline: Deadline) -> Vec<(String, String)> {
    let head = stdout(&sh(
        deadline,
        Path::new("/"),
        "git",
        &[
            "-C",
            &workspace().display().to_string(),
            "rev-parse",
            "HEAD",
        ],
    ));
    let rustc = stdout(&sh(deadline, Path::new("/"), "rustc", &["-V"]));
    let mut out = vec![
        ("workspace_head".to_owned(), head.trim().to_owned()),
        ("rustc".to_owned(), rustc.trim().to_owned()),
    ];
    for name in ["cowfs-daemon", "cowfs", "cowfs-treehouse"] {
        let d = match sibling_bin(name) {
            Some(p) => digest(&p),
            None => "absent".to_owned(),
        };
        out.push((format!("sha256_{name}"), d));
    }
    out
}

/// A throwaway clone of the sample project at its own commit.
///
/// `base_refresh` runs `git worktree add` inside the repository it is given, so the sample must be
/// a copy. Cloning the lease pins the commit, which is what makes the native control and the
/// snapshot the same project.
struct Sample {
    /// Held so the clone outlives nothing, and never walked while anything is mounted under it.
    dir: tempfile::TempDir,
    repo: PathBuf,
    commit: String,
}

impl Sample {
    fn new(tag: &str, deadline: Deadline) -> Sample {
        let dir = private_tempdir();
        let repo = dir.path().join("sample");
        let out = sh(
            deadline,
            dir.path(),
            "git",
            &[
                "clone",
                "-q",
                "--no-hardlinks",
                &workspace().display().to_string(),
                &repo.display().to_string(),
            ],
        );
        assert_eq!(
            code(&out),
            0,
            "cloning the sample failed: {}{}",
            stdout(&out),
            stderr(&out)
        );
        let commit = stdout(&sh(deadline, &repo, "git", &["rev-parse", "HEAD"]));
        let commit = commit.trim().to_owned();
        assert_eq!(commit.len(), 40, "a full commit id, got {commit:?}");
        record(
            tag,
            &[
                ("outcome", MEASURED.to_owned()),
                ("sample", repo.display().to_string()),
                ("sample_commit", commit.clone()),
            ],
        );
        Sample { dir, repo, commit }
    }

    fn path(&self) -> &Path {
        &self.repo
    }
}

/// What the native mount table said. Three states, because two are not enough: an unreadable table
/// must never be reported as a clean readback.
#[derive(Clone, Debug, PartialEq, Eq)]
enum MountVerdict {
    /// The table was read. `under_base` holds this runtime root's mount points.
    Known {
        under_base: Vec<PathBuf>,
        parsed: usize,
        unparsed: usize,
    },
    /// The table could not be trusted. Nothing may be unmounted and nothing may be deleted.
    Unknown(String),
}

/// Parses `mount` output. Only mount points under `base` are ever returned, so a foreign mount is
/// out of reach by construction rather than by a later check.
fn parse_mount_table(raw: &str, base: &Path) -> MountVerdict {
    if raw.trim().is_empty() {
        return MountVerdict::Unknown("the mount table was empty".to_owned());
    }
    let prefix = format!("{}/", base.display());
    let mut under_base = Vec::new();
    let (mut parsed, mut unparsed) = (0usize, 0usize);
    for line in raw.lines() {
        if line.trim().is_empty() {
            continue;
        }
        // `mount` prints `server:/export on /point (nfs, ...)`, so the point follows " on ".
        match line
            .split(" on ")
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())
        {
            Some(point) => {
                parsed += 1;
                if point.starts_with(&prefix) {
                    under_base.push(PathBuf::from(point));
                }
            }
            None => unparsed += 1,
        }
    }
    if parsed == 0 {
        return MountVerdict::Unknown(format!(
            "no line of the mount table could be parsed ({unparsed} unparsed)"
        ));
    }
    MountVerdict::Known {
        under_base,
        parsed,
        unparsed,
    }
}

/// Reads the native mount table.
fn read_mount_table(deadline: Deadline, base: &Path) -> MountVerdict {
    match run_bounded(
        deadline,
        Path::new("/"),
        "mount",
        &[],
        Duration::from_secs(20),
    ) {
        Ok(out) if code(&out) == 0 => parse_mount_table(&stdout(&out), base),
        Ok(out) => MountVerdict::Unknown(format!("mount exited {}", code(&out))),
        Err(e) => MountVerdict::Unknown(e),
    }
}

/// The mount point, its export and its filesystem type, read back from the native table.
///
/// An API's own `mounted: true` is not a readback. This is the readback, and it is what proves a
/// build really ran on the filesystem under test.
#[derive(Clone, Debug, PartialEq, Eq)]
struct MountIdentity {
    point: PathBuf,
    source: String,
    fstype: String,
}

fn mount_identity(deadline: Deadline, base: &Path, point: &Path) -> Result<MountIdentity, String> {
    let verdict = read_mount_table(deadline, base);
    let under_base = match &verdict {
        MountVerdict::Known { under_base, .. } => under_base.clone(),
        MountVerdict::Unknown(why) => return Err(format!("the mount table is unknown: {why}")),
    };
    let found = under_base
        .iter()
        .find(|p| p.as_path() == point)
        .ok_or_else(|| {
            format!(
                "{} is not in the mount table under {base:?}",
                point.display()
            )
        })?;
    let raw = stdout(&sh(deadline, Path::new("/"), "mount", &[]));
    let line = raw
        .lines()
        .find(|l| {
            l.split(" on ")
                .nth(1)
                .and_then(|r| r.split_whitespace().next())
                == Some(found.to_str().unwrap_or_default())
        })
        .ok_or_else(|| format!("no mount line for {}", found.display()))?;
    let rest = line
        .split(" on ")
        .nth(1)
        .ok_or_else(|| format!("unparsable mount line: {line}"))?;
    let mut parts = rest.split_whitespace();
    let _ = parts.next();
    let options = parts.next().unwrap_or_default();
    let fstype = options
        .trim_start_matches('(')
        .split(',')
        .next()
        .unwrap_or_default()
        .to_owned();
    Ok(MountIdentity {
        point: found.clone(),
        source: line.split(" on ").next().unwrap_or_default().to_owned(),
        fstype,
    })
}

/// What was registered when the daemon was spawned, re-checked immediately before any signal.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Registered {
    pid: u32,
    exe: String,
    store: String,
    socket: String,
}

/// Re-reads a pid's identity from the process table. `None` when it is gone.
fn read_identity(deadline: Deadline, pid: u32) -> Option<(String, String)> {
    let out = run_bounded(
        deadline,
        Path::new("/"),
        "ps",
        &["-p", &pid.to_string(), "-o", "lstart=,args="],
        Duration::from_secs(20),
    )
    .ok()?;
    if code(&out) != 0 {
        return None;
    }
    let text = stdout(&out);
    let line = text.lines().find(|l| !l.trim().is_empty())?.trim();
    // `lstart` is five fields, the rest is the command line.
    let mut parts = line.split_whitespace();
    let stamp: Vec<&str> = parts.by_ref().take(5).collect();
    let args: Vec<&str> = parts.collect();
    if stamp.len() != 5 || args.is_empty() {
        return None;
    }
    Some((stamp.join(" "), args.join(" ")))
}

/// Whether the live process is still the one this harness started.
fn identity_matches(want: &Registered, got: &(String, String)) -> bool {
    let (_stamp, args) = got;
    args.contains(&want.store) && args.contains(&want.socket) && args.contains(&want.exe)
}

/// A real `cowfs-daemon` over the real core backend, on a private store, socket and mount.
///
/// The temporary tree is deliberately leaked from `TempDir` and removed by hand, because a
/// `TempDir` drop runs `remove_dir_all` and that walks a live mount point when teardown failed.
struct Core {
    root: PathBuf,
    child: Option<Child>,
    base: PathBuf,
    socket: PathBuf,
    store: PathBuf,
    mount: PathBuf,
    pool_root: PathBuf,
    argv: Vec<String>,
    registered: Registered,
}

impl Core {
    /// `Err(Skip)` when this host cannot mount. In [`Mode::Required`] that is a failure.
    fn start(test: &'static str, deadline: Deadline) -> Result<Core, Skip> {
        // A missing binary is never a capability skip: under `cargo test --workspace` the
        // workspace binaries are built, so their absence means the command was wrong. Letting it
        // skip is exactly the defect this replaces, where libtest reported `ok` for a gate that
        // ran nothing.
        require_bin("cowfs-daemon");
        let daemon = sibling_bin("cowfs-daemon").expect("require_bin just proved it is there");
        if !cowfs_nfs_or_fuse_present() {
            let skip = Skip {
                what: "a mount adapter",
                why: "neither /sbin/mount_nfs nor /dev/fuse is present".to_owned(),
            };
            if mode() == Mode::Required {
                panic!("{test} required and {}: {}", skip.what, skip.why);
            }
            record_skip(test, &skip);
            return Err(skip);
        }

        let dir = private_tempdir();
        let leaked = dir.keep();
        // macOS resolves TMPDIR to `/private/var/...` and the daemon canonicalises its export
        // roots, so every path handed to it is built from the resolved root.
        let base = std::fs::canonicalize(&leaked).expect("the runtime root resolves");
        let socket = base.join("rt").join("c.sock");
        std::fs::create_dir_all(socket.parent().expect("the socket has a parent"))
            .expect("the socket directory is creatable");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            socket.parent().expect("the socket has a parent"),
            std::fs::Permissions::from_mode(0o700),
        )
        .expect("the socket directory is private");
        let store = base.join("store");
        let mount = base.join("mnt");
        std::fs::create_dir_all(&mount).expect("the mount point is creatable");
        let pool_root = base.join("th").join(".treehouse");
        std::fs::create_dir_all(pool_root.join("p").join("1").join("sample"))
            .expect("a treehouse-shaped slot exists");
        std::fs::set_permissions(&pool_root, std::fs::Permissions::from_mode(0o700))
            .expect("the pool root is private");

        let argv: Vec<String> = vec![
            "--store".into(),
            store.display().to_string(),
            "--mount".into(),
            mount.display().to_string(),
            "--socket".into(),
            socket.display().to_string(),
            "--backend".into(),
            "core".into(),
            "--export-root".into(),
            pool_root.display().to_string(),
        ];
        let borrowed: Vec<&str> = argv.iter().map(String::as_str).collect();
        let log = std::fs::File::create(base.join("daemon.log")).expect("the log is creatable");
        let child = Command::new(&daemon)
            .args(&borrowed)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone().expect("the log clones")))
            .stderr(Stdio::from(log))
            .spawn()
            .expect("the daemon starts");
        let registered = Registered {
            pid: child.id(),
            exe: daemon
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            store: store.display().to_string(),
            socket: socket.display().to_string(),
        };

        // Bounded wait that exits on failure as well as on success.
        let bound_deadline = Deadline::after(60);
        let mut bound = false;
        while Instant::now() < bound_deadline.0 {
            if socket.exists() {
                bound = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        if !bound {
            let mut orphan = Core {
                root: leaked,
                child: Some(child),
                base,
                socket,
                store,
                mount,
                pool_root,
                argv,
                registered,
            };
            orphan.teardown();
            panic!("the daemon never bound the socket");
        }

        let core = Core {
            root: leaked,
            child: Some(child),
            base,
            socket,
            store,
            mount,
            pool_root,
            argv,
            registered,
        };
        let mut manifest = binary_manifest(deadline);
        manifest.push(("outcome".to_owned(), MEASURED.to_owned()));
        manifest.push(("daemon_argv".to_owned(), core.argv.join(" ")));
        manifest.push(("daemon_pid".to_owned(), core.registered.pid.to_string()));
        manifest.push((
            "daemon_socket".to_owned(),
            core.socket.display().to_string(),
        ));
        manifest.push(("daemon_store".to_owned(), core.store.display().to_string()));
        manifest.push(("daemon_mount".to_owned(), core.mount.display().to_string()));
        let refs: Vec<(&str, String)> = manifest
            .iter()
            .map(|(k, v)| (k.as_str(), v.clone()))
            .collect();
        record("core-daemon", &refs);
        Ok(core)
    }

    /// The store that answers, read back from the daemon rather than assumed.
    fn status_store(&self, deadline: Deadline) -> String {
        self.cli(deadline, &["status"])
            .map(|o| {
                serde_json::from_str::<serde_json::Value>(stdout(&o).trim())
                    .ok()
                    .and_then(|v| v["store_path"].as_str().map(str::to_owned))
                    .unwrap_or_default()
            })
            .unwrap_or_default()
    }

    fn cli(&self, deadline: Deadline, args: &[&str]) -> Option<Output> {
        let cowfs = sibling_bin("cowfs")?;
        let mut full: Vec<String> = vec![
            "--socket".into(),
            self.socket.display().to_string(),
            "--json".into(),
        ];
        full.extend(args.iter().map(|a| (*a).to_owned()));
        let borrowed: Vec<&str> = full.iter().map(String::as_str).collect();
        Some(sh(deadline, Path::new("/"), cowfs.to_str()?, &borrowed))
    }

    fn client(&self) -> Client {
        Client::connect_with(&self.socket, ClientOptions::default()).expect("the daemon answers")
    }

    /// The treehouse-shaped slot path under this daemon's export root.
    fn slot_path(&self, slot: &str) -> PathBuf {
        self.pool_root.join("p").join(slot).join("sample")
    }

    /// A real treehouse-shaped slot whose pool directory carries the id the companion derives, and
    /// whose slot is a real `git worktree add`, so its `.git` resolves. The companion refuses a
    /// pool directory whose name is not the derived one, so an invented name is rejected for the
    /// wrong reason.
    fn real_slot(&self, deadline: Deadline, repo: &Path, pool_id: &str) -> PathBuf {
        let slot = self.pool_root.join(pool_id).join("1").join("sample");
        std::fs::create_dir_all(slot.parent().expect("the slot has a parent"))
            .expect("the pool directory is creatable");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            self.pool_root.join(pool_id),
            std::fs::Permissions::from_mode(0o700),
        )
        .expect("the pool directory is private");
        let sha = stdout(&sh(deadline, repo, "git", &["rev-parse", "HEAD"]));
        let added = sh(
            deadline,
            repo,
            "git",
            &[
                "-C",
                &repo.display().to_string(),
                "worktree",
                "add",
                "--detach",
                "-q",
                &slot.display().to_string(),
                sha.trim(),
            ],
        );
        assert_eq!(
            code(&added),
            0,
            "the slot worktree was not created: {}{}",
            stdout(&added),
            stderr(&added)
        );
        slot
    }

    fn daemon_log(&self) -> String {
        std::fs::read_to_string(self.base.join("daemon.log")).unwrap_or_default()
    }
}

/// One absolute deadline for the whole teardown, taken before anything is spawned.
impl Core {
    fn teardown(&mut self) {
        let deadline = Deadline::after(240);
        let argv = self.argv.join(" ");

        // 1. Ask the daemon to stop. It owns the exports and unmounts them itself, which is the
        //    only order that leaves no mount without a server.
        if let Some(cowfs) = sibling_bin("cowfs") {
            let _ = run_bounded(
                deadline,
                Path::new("/"),
                cowfs.to_str().unwrap_or_default(),
                &["--socket", &self.socket.display().to_string(), "shutdown"],
                Duration::from_secs(30),
            );
        }

        // 2. Bounded wait for the exit. Exits on the process being gone and on the deadline.
        let mut exited = false;
        if let Some(child) = self.child.as_mut() {
            let give_up = Instant::now() + Duration::from_secs(30);
            while Instant::now() < give_up && !deadline.expired() {
                match child.try_wait() {
                    Ok(Some(_)) | Err(_) => {
                        exited = true;
                        break;
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(100)),
                }
            }
        }

        // 3. Re-read the registered identity immediately before any signal, and signal only if it
        //    is still the process this harness started.
        let mut signalled = false;
        if !exited {
            match read_identity(deadline, self.registered.pid) {
                Some(got) if identity_matches(&self.registered, &got) => {
                    if let Some(child) = self.child.as_mut() {
                        let _ = child.kill();
                        let _ = child.wait();
                        signalled = true;
                    }
                }
                other => {
                    eprintln!(
                        "ACCEPTANCE: not signalling pid {}: identity {:?} does not match the \
                         registered store and socket. Quarantined.",
                        self.registered.pid,
                        other.map(|g| g.1)
                    );
                }
            }
        }

        // 4. Unmount whatever of ours the native table still lists. An unreadable table quarantines
        //    instead, and nothing is deleted in either case while the table is unknown.
        let mut unmounted: Vec<String> = Vec::new();
        let mut quarantined: Vec<String> = Vec::new();
        match read_mount_table(deadline, &self.base) {
            MountVerdict::Unknown(why) => quarantined.push(format!("mount table unknown: {why}")),
            MountVerdict::Known { under_base, .. } => {
                for path in under_base {
                    // Re-read immediately before each umount: never a stale decision, never a
                    // path that is not mounted now.
                    let still = matches!(
                        read_mount_table(deadline, &self.base),
                        MountVerdict::Known { under_base, .. } if under_base.contains(&path)
                    );
                    if !still {
                        continue;
                    }
                    match run_bounded(
                        deadline,
                        Path::new("/"),
                        "umount",
                        &[&path.display().to_string()],
                        Duration::from_secs(45),
                    ) {
                        Ok(o) if code(&o) == 0 => unmounted.push(path.display().to_string()),
                        Ok(o) => quarantined.push(format!(
                            "umount {}: {}",
                            path.display(),
                            stderr(&o).trim()
                        )),
                        Err(e) => quarantined.push(e),
                    }
                }
            }
        }

        // 5. Readback, recorded rather than assumed.
        let verdict_after = read_mount_table(deadline, &self.base);
        let left = match &verdict_after {
            MountVerdict::Known { under_base, .. } => under_base
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(","),
            MountVerdict::Unknown(why) => format!("unknown: {why}"),
        };

        record(
            "teardown",
            &[
                ("daemon_argv", argv),
                ("daemon_pid", self.registered.pid.to_string()),
                ("daemon_exited", exited.to_string()),
                ("signalled_after_identity_check", signalled.to_string()),
                ("unmounted", unmounted.join(",")),
                ("mounts_left_listed", left.clone()),
                ("quarantined", quarantined.join(" | ")),
                ("runtime_root", self.root.display().to_string()),
            ],
        );
        for q in &quarantined {
            eprintln!("ACCEPTANCE QUARANTINE: {q}");
        }

        // 6. Remove the temporary tree only when the table is known and nothing of ours is
        //    mounted under it. Never a recursive delete over an unknown table or a live mount.
        let safe = matches!(
            read_mount_table(deadline, &self.base),
            MountVerdict::Known { .. }
        ) && left.is_empty();
        if safe && deadline.remaining() > Duration::from_secs(5) {
            let _ = run_bounded(
                deadline,
                Path::new("/"),
                "rm",
                &["-rf", "--", &self.root.display().to_string()],
                Duration::from_secs(60),
            );
        } else {
            eprintln!(
                "ACCEPTANCE: leaving {} in place for inspection; mount table not clean",
                self.root.display()
            );
        }
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        // Never panics: a panic here during an unwinding failure is a double panic, which aborts
        // the process and destroys the failure message the run exists to produce.
        self.teardown();
    }
}

fn cowfs_nfs_or_fuse_present() -> bool {
    Path::new("/sbin/mount_nfs").exists() || Path::new("/dev/fuse").exists()
}

/// Every path in `root`, relative and sorted, so two trees can be compared exactly and in both
/// directions.
fn tree(root: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .display()
                .to_string();
            let kind = match entry.file_type() {
                Ok(t) if t.is_dir() => Some("d"),
                Ok(_) => Some("f"),
                Err(_) => None,
            };
            match kind {
                Some("d") => {
                    out.push(format!("d {rel}"));
                    walk(root, &path, out);
                }
                Some(_) => out.push(format!("f {rel}")),
                None => out.push(format!("? {rel}")),
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

fn dir_kib(deadline: Deadline, dir: &Path) -> String {
    let out = sh(
        deadline,
        Path::new("/"),
        "du",
        &["-sk", &dir.display().to_string()],
    );
    if code(&out) == 0 {
        stdout(&out)
            .split_whitespace()
            .next()
            .unwrap_or("?")
            .to_owned()
    } else {
        "absent".to_owned()
    }
}

/// The native control: the same project at the same commit, on a real filesystem.
///
/// This is the do-nothing baseline. It is measured, not assumed, and recorded with the same
/// toolchain and the same lockfile the exported-snapshot build uses.
#[test]
fn native_control_builds_and_tests_the_sample_project() {
    let deadline = Deadline::after(1800);
    let sample = Sample::new("native-control", deadline);
    let target = sample.dir.path().join("native-target");
    let build = Command::new("cargo")
        .args(["build", "-p", "cowfs-ctl"])
        .current_dir(sample.path())
        .env("CARGO_TARGET_DIR", &target)
        .stdin(Stdio::null())
        .output()
        .expect("cargo build runs");
    let test = Command::new("cargo")
        .args(["test", "-p", "cowfs-ctl"])
        .current_dir(sample.path())
        .env("CARGO_TARGET_DIR", &target)
        .stdin(Stdio::null())
        .output()
        .expect("cargo test runs");
    let built = target.join("debug").join("libcowfs_ctl.rlib");

    let mut fields = binary_manifest(deadline);
    fields.push(("outcome".to_owned(), MEASURED.to_owned()));
    fields.push(("sample_commit".to_owned(), sample.commit.clone()));
    fields.push(("cargo_build_exit".to_owned(), code(&build).to_string()));
    fields.push(("cargo_test_exit".to_owned(), code(&test).to_string()));
    fields.push(("rlib_sha256".to_owned(), digest(&built)));
    fields.push(("target_kib".to_owned(), dir_kib(deadline, &target)));
    let refs: Vec<(&str, String)> = fields
        .iter()
        .map(|(k, v)| (k.as_str(), v.clone()))
        .collect();
    record("native-control", &refs);

    assert_eq!(code(&build), 0, "native build failed: {}", stderr(&build));
    assert_eq!(code(&test), 0, "native test failed: {}", stderr(&test));
    assert!(built.is_file(), "the native control produced no rlib");
}

/// The first link in the chain: `base_refresh` does not publish on the core.
///
/// This test asserts a clean refusal, not merely an error: exit 1, a message that names the reason,
/// no snapshot published, and no git worktree left behind.
#[test]
fn the_core_daemon_refuses_base_refresh_and_publishes_nothing() {
    let deadline = Deadline::after(600);
    let Ok(core) = Core::start(
        "the_core_daemon_refuses_base_refresh_and_publishes_nothing",
        deadline,
    ) else {
        return;
    };
    let sample = Sample::new("base-refresh-refused", deadline);
    let companion = require_bin("cowfs-treehouse");

    let before = stdout(&sh(deadline, sample.path(), "git", &["worktree", "list"]));
    let out = sh(
        deadline,
        Path::new("/"),
        companion.to_str().expect("a utf8 path"),
        &[
            "--socket",
            &core.socket.display().to_string(),
            "--json",
            "base",
            "refresh",
            "--repo",
            &sample.path().display().to_string(),
            "--ref",
            "HEAD",
        ],
    );
    let after = stdout(&sh(deadline, sample.path(), "git", &["worktree", "list"]));
    let listed = core
        .cli(deadline, &["snapshot", "list"])
        .map(|l| stdout(&l))
        .unwrap_or_default();

    record(
        "base-refresh-refused",
        &[
            ("outcome", MEASURED.to_owned()),
            ("companion_exit", code(&out).to_string()),
            ("companion_stdout", stdout(&out).trim().to_owned()),
            ("companion_stderr", stderr(&out).trim().to_owned()),
            ("snapshot_list_after", listed.trim().to_owned()),
            (
                "worktrees_unchanged",
                (before.trim() == after.trim()).to_string(),
            ),
            ("daemon_log", core.daemon_log().trim().to_owned()),
        ],
    );

    assert_eq!(
        code(&out),
        1,
        "the refusal must be exit 1: {}{}",
        stdout(&out),
        stderr(&out)
    );
    let err = stderr(&out);
    assert!(
        err.contains("unsupported"),
        "the core must refuse by name, not fail obscurely: {err}"
    );
    assert!(
        err.contains("stores snapshots as trees"),
        "the refusal must name the reason: {err}"
    );
    assert!(
        !listed.contains("base"),
        "a refused base_refresh must publish nothing: {listed}"
    );
    assert_eq!(
        before.trim(),
        after.trim(),
        "a refused base_refresh must leave no git worktree behind"
    );
}

/// The second link: #97. `import::base_refresh` finds the checkout by reading the last line of
/// `git worktree add` stdout and treating it as a path. On this host's git no line of that stdout is
/// a path, and both forms leak a worktree.
#[test]
fn git_never_prints_the_worktree_path_this_codebase_parses() {
    let deadline = Deadline::after(300);
    let sample = Sample::new("worktree-stdout", deadline);
    let sha = sample.commit.clone();

    let worktrees = |deadline: Deadline| -> Vec<String> {
        stdout(&sh(deadline, sample.path(), "git", &["worktree", "list"]))
            .lines()
            .skip(1)
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(|l| l.split_whitespace().next().unwrap_or("").to_owned())
            .collect()
    };
    let clear = |deadline: Deadline| {
        for entry in worktrees(deadline) {
            let _ = sh(
                deadline,
                sample.path(),
                "git",
                &[
                    "-C",
                    &sample.path().display().to_string(),
                    "worktree",
                    "remove",
                    "--force",
                    &entry,
                ],
            );
        }
        worktrees(deadline).len()
    };

    let plain = sh(
        deadline,
        sample.path(),
        "git",
        &[
            "-C",
            &sample.path().display().to_string(),
            "worktree",
            "add",
            "--detach",
            &sha,
        ],
    );
    let leaked_plain = worktrees(deadline);
    let after_plain = clear(deadline);
    let quiet = sh(
        deadline,
        sample.path(),
        "git",
        &[
            "-C",
            &sample.path().display().to_string(),
            "worktree",
            "add",
            "--detach",
            "-q",
            &sha,
        ],
    );
    let leaked_quiet = worktrees(deadline);
    let after_quiet = clear(deadline);

    let last_line = |out: &Output| -> String {
        stdout(out)
            .lines()
            .rev()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or_default()
            .to_owned()
    };
    let parsed_plain = last_line(&plain);
    let parsed_quiet = last_line(&quiet);

    record(
        "worktree-stdout",
        &[
            ("outcome", MEASURED.to_owned()),
            ("sample_commit", sha),
            (
                "git_version",
                stdout(&sh(deadline, Path::new("/"), "git", &["--version"]))
                    .trim()
                    .to_owned(),
            ),
            ("plain_exit", code(&plain).to_string()),
            ("plain_stdout", stdout(&plain).trim().to_owned()),
            ("plain_leaked_worktrees", leaked_plain.join(",")),
            ("plain_worktrees_after_cleanup", after_plain.to_string()),
            ("quiet_exit", code(&quiet).to_string()),
            ("quiet_stdout", stdout(&quiet).trim().to_owned()),
            ("quiet_leaked_worktrees", leaked_quiet.join(",")),
            (
                "parsed_plain_is_a_dir",
                Path::new(&parsed_plain).is_dir().to_string(),
            ),
            (
                "parsed_quiet_is_a_dir",
                Path::new(&parsed_quiet).is_dir().to_string(),
            ),
            (
                "worktree_list_entries_after_cleanup",
                after_quiet.to_string(),
            ),
        ],
    );

    assert_eq!(
        code(&plain),
        0,
        "git worktree add failed: {}",
        stderr(&plain)
    );
    assert_eq!(
        code(&quiet),
        0,
        "git -q worktree add failed: {}",
        stderr(&quiet)
    );
    assert!(
        !Path::new(&parsed_plain).is_dir(),
        "the parsed stdout is a directory, so this host's git is not the affected one: {parsed_plain:?}"
    );
    assert!(
        !Path::new(&parsed_quiet).is_dir(),
        "the -q stdout is a directory, so this host's git is not the affected one: {parsed_quiet:?}"
    );
    assert!(
        !leaked_plain.is_empty() || !leaked_quiet.is_empty(),
        "git reported no new worktree, so the leak this code causes was not observed"
    );
    assert_eq!(after_quiet, 0, "the leaked worktrees were not cleaned up");
}

/// The remaining companion-side gap, named precisely: `CowfsMaterialiser` never calls
/// `mount_snapshot`, even though the daemon implements it and this harness uses it directly.
#[test]
fn the_companion_never_calls_the_mount_snapshot_the_daemon_provides() {
    let deadline = Deadline::after(600);
    let Ok(core) = Core::start(
        "the_companion_never_calls_the_mount_snapshot_the_daemon_provides",
        deadline,
    ) else {
        return;
    };
    let sample = Sample::new("materialiser", deadline);
    let companion = require_bin("cowfs-treehouse");
    let companion_str = companion.to_str().expect("a utf8 path").to_owned();
    let pool_id = stdout(&sh(
        deadline,
        Path::new("/"),
        &companion_str,
        &["pool-id", &sample.path().display().to_string()],
    ));
    let pool_id = pool_id.trim().to_owned();
    assert!(!pool_id.is_empty(), "no pool id for the sample");
    let base = cowfs_treehouse::base_snapshot(&pool_id).expect("a derived base name");
    let slot = core.real_slot(deadline, sample.path(), &pool_id);

    let imported = core
        .cli(
            deadline,
            &[
                "import",
                &sample.path().display().to_string(),
                "--name",
                &base,
            ],
        )
        .expect("the cli runs");
    assert_eq!(code(&imported), 0, "import failed: {}", stderr(&imported));
    let promoted = core
        .cli(deadline, &["snapshot", "promote", &base])
        .expect("cli");
    assert_eq!(code(&promoted), 0, "promote failed: {}", stderr(&promoted));

    let out = sh(
        deadline,
        Path::new("/"),
        &companion_str,
        &[
            "--socket",
            &core.socket.display().to_string(),
            "--json",
            "provision",
            "--slot",
            &slot.display().to_string(),
        ],
    );
    record(
        "materialiser",
        &[
            ("outcome", MEASURED.to_owned()),
            ("pool_id", pool_id.clone()),
            ("base_snapshot_name", base.clone()),
            ("slot_snapshot_expected", format!("{pool_id}-1")),
            (
                "slot_dot_git",
                std::fs::read_to_string(slot.join(".git"))
                    .unwrap_or_default()
                    .trim()
                    .to_owned(),
            ),
            ("companion_exit", code(&out).to_string()),
            ("companion_stderr", stderr(&out).trim().to_owned()),
            ("slot_path", slot.display().to_string()),
        ],
    );
    assert_eq!(
        code(&out),
        1,
        "the refusal must be exit 1: {}{}",
        stdout(&out),
        stderr(&out)
    );
    let err = stderr(&out);
    assert!(
        err.contains("mount_snapshot"),
        "the refusal must name the missing call: {err}"
    );
    assert!(
        err.contains("gap 1"),
        "the refusal must point at the documented gap: {err}"
    );
}

/// Every mode (b) postcondition the core does implement, over one real daemon and a real project.
///
/// The base here is published by the core's own verified ingest and then promoted, because
/// `base_refresh` does not exist on the core. That is stated as a substitution, not as acceptance.
#[test]
fn every_implemented_mode_b_postcondition_holds_over_the_real_core() {
    let deadline = Deadline::after(1800);
    let Ok(core) = Core::start(
        "every_implemented_mode_b_postcondition_holds_over_the_real_core",
        deadline,
    ) else {
        return;
    };
    let sample = Sample::new("mode-b-postconditions", deadline);
    let base = "sr-base";
    let slot_snapshot = "sr-slot-1";
    let slot = core.slot_path("1");

    let imported = core
        .cli(
            deadline,
            &[
                "import",
                &sample.path().display().to_string(),
                "--name",
                base,
            ],
        )
        .expect("cli");
    assert_eq!(code(&imported), 0, "import failed: {}", stderr(&imported));
    let report: serde_json::Value = stdout(&imported)
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .expect("the import report is JSON");
    assert_eq!(report["verified"], true, "{report}");
    assert_eq!(
        report["source_root_hash"], report["imported_root_hash"],
        "{report}"
    );

    let promoted = core
        .cli(deadline, &["snapshot", "promote", base])
        .expect("cli");
    assert_eq!(code(&promoted), 0, "promote failed: {}", stderr(&promoted));
    let forked = core
        .cli(
            deadline,
            &["snapshot", "create", slot_snapshot, "--from", base],
        )
        .expect("cli");
    assert_eq!(
        code(&forked),
        0,
        "snapshot create failed: {}",
        stderr(&forked)
    );
    let listed = core.cli(deadline, &["snapshot", "list"]).expect("cli");
    assert_eq!(
        code(&listed),
        0,
        "snapshot list failed: {}",
        stderr(&listed)
    );
    let list: serde_json::Value = serde_json::from_str(stdout(&listed).trim()).expect("JSON list");
    let entry = |name: &str| -> serde_json::Value {
        list["snapshots"]
            .as_array()
            .expect("an array")
            .iter()
            .find(|s| s["name"] == name)
            .cloned()
            .unwrap_or(serde_json::Value::Null)
    };
    assert_ne!(base, slot_snapshot, "the ids must differ");
    assert_eq!(entry(base)["name"], base, "the base is listed: {list}");
    assert_eq!(
        entry(slot_snapshot)["parent"],
        base,
        "the slot is a clone of the base, not of the empty tree: {list}"
    );

    // Which store answered, read back from the daemon rather than assumed.
    let answered = core.status_store(deadline);
    assert_eq!(
        answered,
        std::fs::canonicalize(&core.store)
            .unwrap_or_else(|_| core.store.clone())
            .display()
            .to_string(),
        "the answering daemon is not the one this harness started"
    );

    let mut client = core.client();
    let mount_info = match client.call(Request::MountSnapshot(MountSnapshot {
        name: slot_snapshot.to_owned(),
        path: slot.display().to_string(),
        expect_no_holders: true,
    })) {
        Ok(Response::MountInfo(m)) => m.clone(),
        other => panic!("mount_snapshot did not answer mount_info: {other:?}"),
    };
    assert!(mount_info.mounted, "{mount_info:?}");
    // An API's own `mounted: true` is not a readback.
    let identity = mount_identity(deadline, &core.base, &slot)
        .unwrap_or_else(|e| panic!("the export is not in the native mount table: {e}"));
    record(
        "mode-b-mount-identity",
        &[
            ("outcome", MEASURED.to_owned()),
            ("mount_point", identity.point.display().to_string()),
            ("mount_source", identity.source.clone()),
            ("mount_fstype", identity.fstype.clone()),
            ("adapter_says", mount_info.adapter.clone()),
            ("answering_store", answered),
        ],
    );

    // Symmetric: the export equals the source in both directions.
    let exported = tree(&slot);
    let source = tree(sample.path());
    let only_in_export: Vec<&String> = exported.iter().filter(|e| !source.contains(e)).collect();
    let only_in_source: Vec<&String> = source.iter().filter(|e| !exported.contains(e)).collect();
    record(
        "mode-b-export-readback",
        &[
            ("outcome", MEASURED.to_owned()),
            ("source_entries", source.len().to_string()),
            ("export_entries", exported.len().to_string()),
            ("only_in_export", only_in_export.len().to_string()),
            ("only_in_source", only_in_source.len().to_string()),
        ],
    );
    assert!(
        only_in_source.is_empty(),
        "{} source entries are missing from the export: {only_in_source:?}",
        only_in_source.len()
    );
    assert!(
        only_in_export.is_empty(),
        "{} entries exist only in the export: {only_in_export:?}",
        only_in_export.len()
    );

    let write =
        std::fs::write(slot.join("ACCEPTANCE-WRITE"), b"slot only\n").map_err(|e| e.to_string());
    let write_note = write.clone().map_or_else(|e| e, |()| "ok".to_owned());
    assert!(write.is_ok(), "the export is not writable: {write:?}");

    // Reset returns the slot to an untouched base.
    let reset = core
        .cli(
            deadline,
            &["snapshot", "reset", slot_snapshot, "--from", base],
        )
        .expect("cli");
    assert_eq!(code(&reset), 0, "snapshot reset failed: {}", stderr(&reset));
    let unmounted = client.call(Request::UnmountSnapshot(UnmountSnapshot {
        path: slot.display().to_string(),
    }));
    assert!(unmounted.is_ok(), "unmount_snapshot failed: {unmounted:?}");
    drop(client);

    // The base exported separately is what the slot must equal again. A fresh export on a fresh
    // client is used rather than the still-mounted one, because a mounted readback can lag.
    let base_slot = core.slot_path("2");
    let mut client = core.client();
    client
        .call(Request::MountSnapshot(MountSnapshot {
            name: base.to_owned(),
            path: base_slot.display().to_string(),
            expect_no_holders: true,
        }))
        .expect("the base exports");
    let base_tree = tree(&base_slot);
    let base_digest = digest(&base_slot.join("Cargo.toml"));
    let _ = client.call(Request::UnmountSnapshot(UnmountSnapshot {
        path: base_slot.display().to_string(),
    }));
    drop(client);

    let mut client = core.client();
    client
        .call(Request::MountSnapshot(MountSnapshot {
            name: slot_snapshot.to_owned(),
            path: slot.display().to_string(),
            expect_no_holders: true,
        }))
        .expect("the reset slot exports again");
    let slot_tree_after_reset = tree(&slot);
    let slot_digest = digest(&slot.join("Cargo.toml"));
    let write_survived = slot.join("ACCEPTANCE-WRITE").exists();
    let _ = client.call(Request::UnmountSnapshot(UnmountSnapshot {
        path: slot.display().to_string(),
    }));
    drop(client);

    record(
        "mode-b-postconditions",
        &[
            ("outcome", MEASURED.to_owned()),
            ("import_verified", report["verified"].to_string()),
            (
                "root_hash",
                report["source_root_hash"]
                    .as_str()
                    .unwrap_or("?")
                    .to_owned(),
            ),
            ("files", report["files"].to_string()),
            ("bytes", report["bytes"].to_string()),
            ("base_snapshot", base.to_owned()),
            ("slot_snapshot", slot_snapshot.to_owned()),
            (
                "slot_parent",
                entry(slot_snapshot)["parent"]
                    .as_str()
                    .unwrap_or("?")
                    .to_owned(),
            ),
            ("export_write", write_note),
            ("reset_exit", code(&reset).to_string()),
            ("base_entries", base_tree.len().to_string()),
            (
                "slot_entries_after_reset",
                slot_tree_after_reset.len().to_string(),
            ),
            ("base_cargo_toml_sha256", base_digest.clone()),
            ("slot_cargo_toml_sha256", slot_digest.clone()),
            ("slot_write_survived_reset", write_survived.to_string()),
        ],
    );
    assert_eq!(
        base_tree, slot_tree_after_reset,
        "a reset slot must equal the base exactly"
    );
    assert_eq!(
        base_digest, slot_digest,
        "the reset slot's bytes differ from the base's"
    );
    assert!(
        !write_survived,
        "the reset did not discard what the slot wrote"
    );
}

/// A real `cargo build` and `cargo test` of the sample project inside a real exported snapshot.
///
/// The mount identity is read back from the native table before the build runs, so a PASS here
/// cannot be an ordinary native build that never touched cowfs.
#[test]
fn a_real_project_builds_and_tests_inside_an_exported_slot_snapshot() {
    let deadline = Deadline::after(3600);
    let Ok(core) = Core::start(
        "a_real_project_builds_and_tests_inside_an_exported_slot_snapshot",
        deadline,
    ) else {
        return;
    };
    let sample = Sample::new("slot-build", deadline);
    let base = "sr-build-base";
    let slot_snapshot = "sr-build-slot";
    let slot = core.slot_path("1");

    let imported = core
        .cli(
            deadline,
            &[
                "import",
                &sample.path().display().to_string(),
                "--name",
                base,
            ],
        )
        .expect("cli");
    assert_eq!(code(&imported), 0, "import failed: {}", stderr(&imported));
    let promoted = core
        .cli(deadline, &["snapshot", "promote", base])
        .expect("cli");
    assert_eq!(code(&promoted), 0, "promote failed: {}", stderr(&promoted));
    let created = core
        .cli(
            deadline,
            &["snapshot", "create", slot_snapshot, "--from", base],
        )
        .expect("cli");
    assert_eq!(
        code(&created),
        0,
        "snapshot create failed: {}",
        stderr(&created)
    );

    let mut client = core.client();
    match client.call(Request::MountSnapshot(MountSnapshot {
        name: slot_snapshot.to_owned(),
        path: slot.display().to_string(),
        expect_no_holders: true,
    })) {
        Ok(Response::MountInfo(m)) => assert!(m.mounted, "{m:?}"),
        other => panic!("mount_snapshot did not answer mount_info: {other:?}"),
    }
    drop(client);

    // Re-read the real mount identity immediately before the build.
    let identity = mount_identity(deadline, &core.base, &slot).unwrap_or_else(|e| {
        panic!("refusing to build: the export is not verified in the mount table: {e}")
    });
    let expected_fs = if cfg!(target_os = "macos") {
        "nfs"
    } else {
        "fuse"
    };
    assert!(
        identity.fstype.starts_with(expected_fs),
        "the slot is on {} not {expected_fs}: {:?}",
        identity.fstype,
        identity
    );
    let answered = core.status_store(deadline);
    let store = core.store.display().to_string();
    assert!(
        answered.ends_with(&store) || store.ends_with(&answered),
        "the answering daemon is not the one this harness started: {answered} vs {store}"
    );

    let in_slot = |deadline: Deadline, args: &[&str]| -> Output {
        run_bounded(deadline, &slot, "cargo", args, Duration::from_secs(1800))
            .unwrap_or_else(|e| panic!("{e}"))
    };
    let build = in_slot(deadline, &["build", "-p", "cowfs-ctl"]);
    let test = in_slot(deadline, &["test", "-p", "cowfs-ctl"]);
    let rlib = slot.join("target").join("debug").join("libcowfs_ctl.rlib");

    let mut fields = binary_manifest(deadline);
    fields.push(("outcome".to_owned(), MEASURED.to_owned()));
    fields.push(("sample_commit".to_owned(), sample.commit.clone()));
    fields.push((
        "mount_point".to_owned(),
        identity.point.display().to_string(),
    ));
    fields.push(("mount_source".to_owned(), identity.source.clone()));
    fields.push(("mount_fstype".to_owned(), identity.fstype.clone()));
    fields.push(("answering_store".to_owned(), answered));
    fields.push(("cargo_build_exit".to_owned(), code(&build).to_string()));
    fields.push(("cargo_test_exit".to_owned(), code(&test).to_string()));
    fields.push(("rlib_sha256".to_owned(), digest(&rlib)));
    fields.push(("rlib_exists".to_owned(), rlib.is_file().to_string()));
    fields.push(("export_kib".to_owned(), dir_kib(deadline, &slot)));
    let refs: Vec<(&str, String)> = fields
        .iter()
        .map(|(k, v)| (k.as_str(), v.clone()))
        .collect();
    record("slot-build", &refs);

    assert_eq!(
        code(&build),
        0,
        "cargo build inside the verified export failed:\n{}",
        stderr(&build)
    );
    assert_eq!(
        code(&test),
        0,
        "cargo test inside the verified export failed:\n{}",
        stderr(&test)
    );
    assert!(rlib.is_file(), "the in-slot build produced no rlib");
}

/// The third link: provenance, which is what acceptance actually requires.
///
/// Acceptance needs `base status` to report the repository, the ref, the commit and `fresh`, and
/// `find_base` to discover the base. On a base where `base_refresh` refuses there is nothing
/// published, so that is recorded. On a base where it succeeds, the provenance is demanded, which is
/// what makes this a gate rather than a description.
#[test]
fn a_published_warm_base_must_be_discoverable_with_its_provenance() {
    let deadline = Deadline::after(900);
    let Ok(core) = Core::start(
        "a_published_warm_base_must_be_discoverable_with_its_provenance",
        deadline,
    ) else {
        return;
    };
    let sample = Sample::new("base-provenance", deadline);
    let companion = require_bin("cowfs-treehouse")
        .to_str()
        .expect("a utf8 path")
        .to_owned();
    let repo = sample.path().display().to_string();

    let refresh = sh(
        deadline,
        Path::new("/"),
        &companion,
        &[
            "--socket",
            &core.socket.display().to_string(),
            "--json",
            "base",
            "refresh",
            "--repo",
            &repo,
            "--ref",
            "HEAD",
        ],
    );
    let refresh_exit = code(&refresh);
    let refresh_err = stderr(&refresh).trim().to_owned();
    let published = refresh_exit == 0;

    let status = sh(
        deadline,
        Path::new("/"),
        &companion,
        &[
            "--socket",
            &core.socket.display().to_string(),
            "--json",
            "base",
            "status",
            "--repo",
            &repo,
            "--ref",
            "HEAD",
        ],
    );
    let status_exit = code(&status);
    let status_json: serde_json::Value = stdout(&status)
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str(l).ok())
        .unwrap_or(serde_json::Value::Null);
    let listed = core
        .cli(deadline, &["snapshot", "list"])
        .map(|l| stdout(&l).trim().to_owned())
        .unwrap_or_default();
    let base_commit = status_json["base_commit"].as_str().unwrap_or("").to_owned();
    let fresh = status_json["fresh"].as_bool();

    record(
        "base-provenance",
        &[
            ("outcome", MEASURED.to_owned()),
            ("sample_commit", sample.commit.clone()),
            ("refresh_exit", refresh_exit.to_string()),
            ("refresh_stderr", refresh_err.clone()),
            ("status_exit", status_exit.to_string()),
            (
                "base_snapshot",
                status_json["snapshot"].as_str().unwrap_or("").to_owned(),
            ),
            ("base_commit", base_commit.clone()),
            (
                "head_commit",
                status_json["head_commit"].as_str().unwrap_or("").to_owned(),
            ),
            (
                "fresh",
                fresh.map(|f| f.to_string()).unwrap_or("absent".to_owned()),
            ),
            (
                "reason",
                status_json["reason"].as_str().unwrap_or("").to_owned(),
            ),
            ("snapshot_list", listed.clone()),
        ],
    );

    if !published {
        assert!(
            refresh_err.contains("unsupported") || refresh_err.contains("not_found"),
            "base refresh failed for an unexpected reason on this base: {refresh_err}"
        );
        assert_eq!(
            status_exit, 1,
            "a base that was never published must not report fresh"
        );
        assert_ne!(
            fresh,
            Some(true),
            "nothing was published, so nothing is fresh"
        );
        return;
    }

    assert_eq!(refresh_exit, 0);
    assert_eq!(
        status_exit, 0,
        "a published base must be statusable: {status:?}"
    );
    assert!(
        !base_commit.is_empty(),
        "the published base records no commit, so find_base cannot use it"
    );
    assert_eq!(
        base_commit, sample.commit,
        "the base was built from another commit"
    );
    assert_eq!(
        fresh,
        Some(true),
        "a base at the ref is not fresh: {status:?}"
    );
    assert!(
        listed.contains(status_json["snapshot"].as_str().unwrap_or("?")),
        "the published base is not in the snapshot list: {listed}"
    );
}

/// The cache hook, read back for real.
///
/// The outcome strings are asserted, not merely recorded: `Added` then `AlreadyThere` is the claim
/// the report makes, so it is checked. The `post_create` line installed is compared with the line
/// treehouse would read, and the operator's pre-existing setting is checked for survival.
#[test]
fn the_cache_hook_is_installed_and_read_back_from_the_real_config() {
    let deadline = Deadline::after(300);
    let home = private_tempdir();
    let companion = require_bin("cowfs-treehouse");
    let config = home.path().join(".config/treehouse/config.toml");
    std::fs::create_dir_all(config.parent().expect("a parent")).expect("mkdir");
    std::fs::write(&config, "max_trees = 12\n").expect("seed");
    let h = home.path().display().to_string();

    let run = |command: Option<&str>| -> Output {
        let owned;
        let args: Vec<&str> = match command {
            Some(c) => {
                owned = vec![
                    "hooks".to_owned(),
                    "install".to_owned(),
                    "--home".to_owned(),
                    h.clone(),
                    "--command".to_owned(),
                    c.to_owned(),
                ];
                owned.iter().map(String::as_str).collect()
            }
            None => vec!["hooks", "install", "--home", &h],
        };
        sh(
            deadline,
            Path::new("/"),
            companion.to_str().expect("a utf8 path"),
            &args,
        )
    };

    let first = run(None);
    let after_first = std::fs::read_to_string(&config).expect("the config is readable");
    let second = run(None);
    let after_second = std::fs::read_to_string(&config).expect("the config is readable");
    let config_path = cowfs_treehouse::user_config_path_for(home.path());

    record(
        "cache-hook",
        &[
            ("outcome", MEASURED.to_owned()),
            ("first_exit", code(&first).to_string()),
            ("first_stdout", stdout(&first).trim().to_owned()),
            ("second_exit", code(&second).to_string()),
            ("second_stdout", stdout(&second).trim().to_owned()),
            ("config_path", config_path.display().to_string()),
            ("config_after_first", after_first.clone()),
            ("idempotent", (after_first == after_second).to_string()),
        ],
    );

    assert_eq!(code(&first), 0, "hooks install failed: {}", stderr(&first));
    assert_eq!(
        code(&second),
        0,
        "hooks install failed: {}",
        stderr(&second)
    );
    assert_eq!(
        stdout(&first).trim(),
        "Added",
        "the first install must report that it added the hook"
    );
    assert_eq!(
        stdout(&second).trim(),
        "AlreadyThere",
        "the second install must report that the hook was already correct"
    );
    assert_eq!(
        config_path, config,
        "the companion must write the path treehouse reads"
    );
    assert!(
        after_first.contains("[hooks]"),
        "no hooks table: {after_first}"
    );

    // The installed `post_create` line, exactly as treehouse reads it.
    let post_create = after_first
        .lines()
        .find_map(|l| l.trim().strip_prefix("post_create"))
        .unwrap_or_default()
        .trim();
    assert!(
        post_create.contains("cowfs-treehouse provision"),
        "post_create does not invoke the companion: {post_create:?}"
    );
    assert!(
        post_create.contains("--slot $PWD"),
        "post_create does not carry the slot path treehouse 3.1 provides: {post_create:?}"
    );
    assert!(
        after_first.contains("# managed by cowfs-treehouse hooks install"),
        "no sentinel, so a later install cannot tell its own line from an operator's: {after_first}"
    );
    assert!(
        after_first.starts_with("max_trees = 12\n"),
        "an existing operator setting was lost: {after_first}"
    );
    assert_eq!(
        after_first, after_second,
        "hooks install must be idempotent"
    );
}

/// The teardown's own logic, exercised with synthetic input and no mount at all.
///
/// This is the safe control for the cleanup path: it runs before any destructive mounted
/// experiment, and it proves the mount readback is tri-state and scoped, which is what stops the
/// teardown from failing open on its own safety check.
#[test]
fn the_mount_readback_is_tri_state_and_never_foreign() {
    let base = Path::new("/private/tmp/cowfs-synthetic-base");
    let good = "\
map auto_home on /System/Volumes/Data/home (autofs, automounted, nobrowse)
localhost:/cowfs-abc on /private/tmp/cowfs-synthetic-base/mnt (nfs, nodev, nosuid, mounted by zeeshanhaque)
localhost:/cowfs-def on /private/tmp/cowfs-synthetic-base/th/.treehouse/p/1/sample (nfs, nodev, nosuid, mounted by zeeshanhaque)
localhost:/elsewhere on /Users/somebody/elsewhere (nfs, nodev, nosuid, mounted by zeeshanhaque)
";
    match parse_mount_table(good, base) {
        MountVerdict::Known {
            under_base,
            parsed,
            unparsed,
        } => {
            assert_eq!(unparsed, 0, "every synthetic line should parse");
            assert!(parsed >= 4, "parsed {parsed} lines");
            assert_eq!(
                under_base,
                vec![base.join("mnt"), base.join("th/.treehouse/p/1/sample"),],
                "only mount points under this root may be returned"
            );
            assert!(
                !under_base.iter().any(|p| p.starts_with("/Users")),
                "a foreign mount leaked into the scoped list"
            );
        }
        other => panic!("a well formed table must be Known, got {other:?}"),
    }

    // Empty is Unknown, not "nothing is mounted".
    assert!(
        matches!(parse_mount_table("", base), MountVerdict::Unknown(_)),
        "an empty table must be Unknown, never a clean readback"
    );
    // Unparsable is Unknown, so nothing is unmounted and nothing is deleted.
    assert!(
        matches!(
            parse_mount_table("garbage\nmore garbage", base),
            MountVerdict::Unknown(_)
        ),
        "an unparsable table must be Unknown"
    );
    // A sibling directory that merely shares a name prefix is not under this root. The prefix
    // carries its trailing separator, so `...-base-2` cannot pass as `...-base`.
    match parse_mount_table(
        "localhost:/x on /private/tmp/cowfs-synthetic-base-2/mnt (nfs, nosuid)\n",
        base,
    ) {
        MountVerdict::Known { under_base, .. } => assert!(
            under_base.is_empty(),
            "a sibling directory sharing a name prefix leaked in: {under_base:?}"
        ),
        other => panic!("expected Known with no scoped entries, got {other:?}"),
    }
    record(
        "mount-readback-control",
        &[("outcome", MEASURED.to_owned()), ("cases", "5".to_owned())],
    );
}

/// What the receipt says about what was measured.
///
/// A capability skip is only honest if it is written down. This reads the receipt the run produced
/// and fails if a gate recorded a skip while the run is being presented as an acceptance.
#[test]
fn the_acceptance_receipt_states_what_was_measured() {
    let deadline = Deadline::after(120);
    let path = evidence_dir().join("acceptance.jsonl");
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let records: Vec<serde_json::Value> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let skips: Vec<&serde_json::Value> = records
        .iter()
        .filter(|r| r["outcome"] == SKIPPED_CAPABILITY)
        .collect();
    record(
        "receipt-summary",
        &[
            ("outcome", MEASURED.to_owned()),
            ("records_read", records.len().to_string()),
            ("capability_skips", skips.len().to_string()),
            ("mode", format!("{:?}", mode())),
        ],
    );
    assert!(
        !records.is_empty(),
        "the receipt is empty, so this run measured nothing at all"
    );
    if !skips.is_empty() {
        let names: Vec<&str> = skips.iter().filter_map(|r| r["test"].as_str()).collect();
        eprintln!(
            "ACCEPTANCE NOT MEASURED: {} gate(s) were capability-skipped: {names:?}. \
             This run is NOT an acceptance.",
            skips.len()
        );
        assert_eq!(
            mode(),
            Mode::BestEffort,
            "COWFS_ACCEPTANCE_REQUIRED=1 was set, so a capability skip must have been a failure"
        );
    }
    // The receipt must never claim a warm base was published while the chain is still broken.
    let claimed_warm = records
        .iter()
        .any(|r| r["warm_base_published"] == true || r["base_status_fresh"] == true);
    assert!(
        !claimed_warm,
        "the receipt claims a published warm base, which no gate at this base established"
    );
    let _ = deadline;
}

/// The acceptance itself, `#[ignore]`d because it cannot pass until the chain is broken.
///
/// Run it on a head that claims to have core `base_refresh` publication, an explicit worktree path,
/// and durable provenance:
///
/// ```text
/// COWFS_ACCEPTANCE_REQUIRED=1 cargo test -p cowfs-treehouse \
///   --test real_project_acceptance -- --ignored warm_base_acceptance_over_a_real_core \
///   --test-threads=1 --nocapture
/// ```
///
/// It asserts, with real exit codes: a warm base published from a real git ref with discoverable
/// provenance; two fresh slots that each clone that published base, not an imported artifact and
/// not the empty tree; a real `cargo build` and `cargo test` inside each, with the mount identity
/// read back before the build; the base still intact afterwards; and a reset that returns each
/// slot to a byte-identical untouched base.
#[test]
#[ignore = "chain broken: can_ingest gate, then #97 worktree path, then #98 provenance"]
fn warm_base_acceptance_over_a_real_core() {
    let deadline = Deadline::after(3600);
    let companion = require_bin("cowfs-treehouse")
        .to_str()
        .expect("a utf8 path")
        .to_owned();
    let Ok(core) = Core::start("warm_base_acceptance_over_a_real_core", deadline) else {
        panic!("the acceptance cannot run on a host that cannot mount");
    };
    let sample = Sample::new("warm-acceptance", deadline);
    let repo = sample.path().display().to_string();

    let refresh = sh(
        deadline,
        Path::new("/"),
        &companion,
        &[
            "--socket",
            &core.socket.display().to_string(),
            "--json",
            "base",
            "refresh",
            "--repo",
            &repo,
            "--ref",
            "HEAD",
        ],
    );
    assert_eq!(
        code(&refresh),
        0,
        "base refresh did not publish: {}{}",
        stdout(&refresh),
        stderr(&refresh)
    );
    let published: serde_json::Value = stdout(&refresh)
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str(l).ok())
        .expect("the refresh report is JSON");
    let base_name = published["snapshot"]
        .as_str()
        .expect("the refresh report names a snapshot")
        .to_owned();
    let base_commit = published["commit"].as_str().unwrap_or("").to_owned();
    assert_eq!(
        base_commit, sample.commit,
        "the published base records no real commit"
    );

    let status = sh(
        deadline,
        Path::new("/"),
        &companion,
        &[
            "--socket",
            &core.socket.display().to_string(),
            "--json",
            "base",
            "status",
            "--repo",
            &repo,
            "--ref",
            "HEAD",
        ],
    );
    assert_eq!(code(&status), 0, "the published base is not discoverable");
    let status_json: serde_json::Value = stdout(&status)
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str(l).ok())
        .expect("the status is JSON");
    assert_eq!(status_json["fresh"], true, "{status:?}");

    // The base, exported once, is the reference every slot must end at again.
    let base_slot = core.slot_path("base");
    let mut client = core.client();
    client
        .call(Request::MountSnapshot(MountSnapshot {
            name: base_name.clone(),
            path: base_slot.display().to_string(),
            expect_no_holders: true,
        }))
        .expect("the published base exports");
    drop(client);
    let base_tree = tree(&base_slot);
    let base_manifest = digest(&base_slot.join("Cargo.toml"));

    for slot_no in ["1", "2"] {
        let snapshot = format!("{base_name}-slot-{slot_no}");
        let created = core
            .cli(
                deadline,
                &["snapshot", "create", &snapshot, "--from", &base_name],
            )
            .expect("cli");
        assert_eq!(code(&created), 0, "slot {slot_no} was not created");
        let list: serde_json::Value = serde_json::from_str(
            stdout(&core.cli(deadline, &["snapshot", "list"]).expect("cli")).trim(),
        )
        .expect("JSON list");
        let parent = list["snapshots"]
            .as_array()
            .expect("an array")
            .iter()
            .find(|s| s["name"] == snapshot)
            .and_then(|s| s["parent"].as_str())
            .unwrap_or_default()
            .to_owned();
        assert_eq!(
            parent, base_name,
            "slot {slot_no} is not a clone of the published warm base"
        );

        let slot = core.slot_path(slot_no);
        let mut client = core.client();
        match client.call(Request::MountSnapshot(MountSnapshot {
            name: snapshot.clone(),
            path: slot.display().to_string(),
            expect_no_holders: true,
        })) {
            Ok(Response::MountInfo(m)) => assert!(m.mounted, "{m:?}"),
            other => panic!("mount_snapshot did not answer mount_info: {other:?}"),
        }
        drop(client);
        let identity = mount_identity(deadline, &core.base, &slot)
            .unwrap_or_else(|e| panic!("slot {slot_no} export is not in the mount table: {e}"));

        let build = run_bounded(
            deadline,
            &slot,
            "cargo",
            &["build", "-p", "cowfs-ctl"],
            Duration::from_secs(1800),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let test = run_bounded(
            deadline,
            &slot,
            "cargo",
            &["test", "-p", "cowfs-ctl"],
            Duration::from_secs(1800),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            code(&build),
            0,
            "slot {slot_no} build failed:\n{}",
            stderr(&build)
        );
        assert_eq!(
            code(&test),
            0,
            "slot {slot_no} test failed:\n{}",
            stderr(&test)
        );

        // Reset the slot and prove it is the untouched base again, on a fresh export.
        let mut client = core.client();
        let _ = client.call(Request::UnmountSnapshot(UnmountSnapshot {
            path: slot.display().to_string(),
        }));
        drop(client);
        let reset = core
            .cli(
                deadline,
                &["snapshot", "reset", &snapshot, "--from", &base_name],
            )
            .expect("cli");
        assert_eq!(
            code(&reset),
            0,
            "slot {slot_no} reset failed: {}",
            stderr(&reset)
        );

        let mut client = core.client();
        client
            .call(Request::MountSnapshot(MountSnapshot {
                name: snapshot.clone(),
                path: slot.display().to_string(),
                expect_no_holders: true,
            }))
            .expect("the reset slot exports again");
        let slot_tree = tree(&slot);
        let slot_manifest = digest(&slot.join("Cargo.toml"));
        let _ = client.call(Request::UnmountSnapshot(UnmountSnapshot {
            path: slot.display().to_string(),
        }));
        drop(client);

        record(
            "warm-acceptance",
            &[
                ("outcome", MEASURED.to_owned()),
                ("slot", slot_no.to_owned()),
                ("base", base_name.clone()),
                ("snapshot", snapshot.clone()),
                ("parent", parent),
                ("mount_fstype", identity.fstype.clone()),
                ("cargo_build_exit", code(&build).to_string()),
                ("cargo_test_exit", code(&test).to_string()),
                ("reset_exit", code(&reset).to_string()),
                ("base_entries", base_tree.len().to_string()),
                ("slot_entries_after_reset", slot_tree.len().to_string()),
                ("base_manifest", base_manifest.clone()),
                ("slot_manifest_after_reset", slot_manifest.clone()),
            ],
        );
        assert_eq!(
            base_tree, slot_tree,
            "slot {slot_no} after reset is not the untouched base"
        );
        assert_eq!(
            base_manifest, slot_manifest,
            "slot {slot_no} after reset differs from the base byte for byte"
        );
    }

    // The base itself must be untouched by two slot builds.
    let base_after = tree(&base_slot);
    assert_eq!(
        base_tree, base_after,
        "the published base changed while slots were built from it"
    );
    record(
        "warm-acceptance",
        &[
            ("outcome", MEASURED.to_owned()),
            ("base_intact_after_two_slots", "true".to_owned()),
            ("base_entries", base_after.len().to_string()),
        ],
    );
}
