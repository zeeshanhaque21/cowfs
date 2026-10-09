use std::fmt;
use std::fmt::Write as _;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use cowfs_vfs::Vfs;

use super::ctx::{Ctx, Failure, Outcome};

/// How tightly a check's expectation is tied to cowfs rather than to the operating system.
/// The levels are ordered: selecting `Portable` runs `Posix` and `Portable` checks.
/// The crate docs list the evidence behind each classification.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    /// Believed to hold on ext4, btrfs and APFS. Safe for a native-filesystem control run.
    Posix,
    /// Holds on the common local filesystems but depends on something that varies:
    /// timestamp granularity, sparse files, xattr support, or was not verified on all three.
    Portable,
    /// A cowfs contract decision, or a `Vfs` concept with no native equivalent
    /// (`forget`, `Stale`, handles pinning inodes).
    Cowfs,
}

impl Level {
    /// Lower-case name, as printed in tables and accepted by `COWFS_CONFORMANCE_LEVEL`.
    pub fn name(self) -> &'static str {
        match self {
            Level::Posix => "posix",
            Level::Portable => "portable",
            Level::Cowfs => "cowfs",
        }
    }

    /// Inverse of `name`, case-insensitive.
    pub fn parse(s: &str) -> Option<Self> {
        [Level::Posix, Level::Portable, Level::Cowfs]
            .into_iter()
            .find(|l| l.name().eq_ignore_ascii_case(s))
    }
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.name())
    }
}

/// One named conformance check.
#[derive(Clone, Copy)]
pub struct Check {
    pub name: &'static str,
    /// The module the check lives in: basic, io, readdir, rename, ...
    pub category: &'static str,
    /// What the check's expectations rest on.
    pub level: Level,
    /// Heavy checks only run when `COWFS_CONFORMANCE_HEAVY=1` (or `Options::heavy`).
    pub heavy: bool,
    pub run: fn(&Ctx) -> Outcome,
}

impl Check {
    /// Whether the check only makes sense when the backend declares a hardlink limit it can
    /// reach quickly (`Options::link_limit`). True for `hardlink_limit_reports_too_many_links`.
    pub fn link_limit_required(&self) -> bool {
        self.name == "hardlink_limit_reports_too_many_links"
    }
}

impl fmt::Debug for Check {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}::{}", self.category, self.name)
    }
}

/// Every check, normal ones first.
pub fn all_checks() -> Vec<Check> {
    crate::__conformance_checks!(__registry)
}

/// True when `COWFS_CONFORMANCE_HEAVY=1`.
pub fn heavy_enabled() -> bool {
    std::env::var("COWFS_CONFORMANCE_HEAVY").is_ok_and(|v| v == "1")
}

/// Default time a normal check may take before it is declared hung.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
/// Default time a heavy check may take before it is declared hung.
pub const DEFAULT_HEAVY_TIMEOUT: Duration = Duration::from_secs(600);

/// Runs one check on a fresh filesystem from `factory` with the default hang timeout.
/// A panic or a hang becomes a `Failure`.
pub fn run_check(check: &Check, factory: &dyn Fn() -> Arc<dyn Vfs>) -> Outcome {
    run_check_timed(check, factory, None).0
}

/// Like `run_check` with an explicit hang timeout (`None` picks the default for the check).
/// The bool is true when the check hung: its thread is still running, detached, and holds its
/// filesystem, so the caller should treat the backend as suspect.
pub fn run_check_timed(
    check: &Check,
    factory: &dyn Fn() -> Arc<dyn Vfs>,
    timeout: Option<Duration>,
) -> (Outcome, bool) {
    run_check_with(check, factory, &Options::default(), timeout)
}

/// Like `run_check_timed` but the context sees `Options` (the xattr and link-limit knobs).
pub fn run_check_with(
    check: &Check,
    factory: &dyn Fn() -> Arc<dyn Vfs>,
    opts: &Options,
    timeout: Option<Duration>,
) -> (Outcome, bool) {
    let ctx = Ctx::with_options(factory(), opts);
    let run = check.run;
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name(format!("conformance-{}", check.name))
        .spawn(move || {
            let r = catch_unwind(AssertUnwindSafe(|| run(&ctx)));
            let _ = tx.send(match r {
                Ok(o) => o,
                Err(p) => {
                    let msg = p
                        .downcast_ref::<&str>()
                        .map(|s| (*s).to_string())
                        .or_else(|| p.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "non-string panic".into());
                    Err(Failure(format!("panicked: {msg}")))
                }
            });
        });
    if let Err(e) = spawned {
        return (
            Err(Failure(format!("cannot spawn check thread: {e}"))),
            false,
        );
    }
    let limit = timeout.unwrap_or(if check.heavy {
        DEFAULT_HEAVY_TIMEOUT
    } else {
        DEFAULT_TIMEOUT
    });
    match rx.recv_timeout(limit) {
        Ok(o) => (o, false),
        Err(_) => (
            Err(Failure(format!(
                "no result after {limit:?}: hang or deadlock, check thread leaked"
            ))),
            true,
        ),
    }
}

/// Runs the check called `name` (see `all_checks`).
pub fn run_named(name: &str, factory: &dyn Fn() -> Arc<dyn Vfs>) -> Outcome {
    match all_checks().into_iter().find(|c| c.name == name) {
        Some(c) => run_check_timed(&c, factory, Options::from_env().timeout).0,
        None => Err(Failure(format!("no check named {name}"))),
    }
}

/// Like `run_named` but panics with the failure. This is what `conformance_tests!` expands to.
pub fn assert_named(name: &str, factory: &dyn Fn() -> Arc<dyn Vfs>) {
    if let Err(f) = run_named(name, factory) {
        panic!("conformance check {name} failed: {f}");
    }
}

/// Panics if any name is not a check. `conformance_tests!` calls this for its skip list, so a
/// skip that names a misspelt or removed check is a test failure and never a silent no-op.
pub fn assert_skip_names(names: &[&str]) {
    let checks = all_checks();
    let unknown: Vec<&&str> = names
        .iter()
        .filter(|n| !checks.iter().any(|c| c.name == **n))
        .collect();
    assert!(
        unknown.is_empty(),
        "skip list names unknown checks: {unknown:?}"
    );
}

/// Which checks `run_all` runs, and how.
#[derive(Clone, Debug, Default)]
pub struct Options {
    pub heavy: bool,
    /// Only checks whose `category::name` contains this text.
    pub filter: Option<String>,
    /// Only checks at this level or lower (see `Level`). `None` runs every check.
    pub level: Option<Level>,
    /// Checks not to run, each with the reason. A name that is not a check is an error in the
    /// report. Skipped checks are listed in the report and counted, never silently dropped.
    pub skip: Vec<(String, String)>,
    /// Hang timeout for every check. `None` means `DEFAULT_TIMEOUT`, or `DEFAULT_HEAVY_TIMEOUT`
    /// for heavy checks.
    pub timeout: Option<Duration>,
    /// The backend's xattr names carry a namespace prefix, so the valid-name case of
    /// `xattr_name_validation` uses `user.` plus `NAME_MAX - 5` bytes. Linux needs this.
    /// (Alternatively skip that check with a reason.)
    pub xattr_names: bool,
    /// The backend's hardlink limit, when it is small enough that the check can reach it inside
    /// the timeout. `None` means the check is reported as skipped. Native filesystems allow
    /// 65,000 or more, so the check is meant for backends that declare a smaller one.
    pub link_limit: Option<u32>,
    /// The backend reaches a real kernel that refuses this process device nodes (no
    /// `CAP_MKNOD`), as a probe `mknod` on the host found (`cowfs_vfs_path::host_can_make_devices`):
    /// the device checks then fall back to the fifo and the socket when the backend answers
    /// `PermissionDenied`. Left false, a `PermissionDenied` to a device is a failure, so a backend
    /// that must allow devices (MemVfs, Core, or a kernel run with the privilege) cannot regress
    /// to it unnoticed.
    pub no_device_privilege: bool,
}

impl Options {
    /// Heavy checks follow `COWFS_CONFORMANCE_HEAVY`, the filter follows
    /// `COWFS_CONFORMANCE_FILTER`, the level follows `COWFS_CONFORMANCE_LEVEL`
    /// (`posix`, `portable` or `cowfs`) and the hang timeout follows
    /// `COWFS_CONFORMANCE_TIMEOUT_SECS`.
    pub fn from_env() -> Self {
        Self {
            heavy: heavy_enabled(),
            filter: std::env::var("COWFS_CONFORMANCE_FILTER").ok(),
            level: std::env::var("COWFS_CONFORMANCE_LEVEL")
                .ok()
                .and_then(|v| Level::parse(&v)),
            skip: Vec::new(),
            xattr_names: matches!(
                std::env::var("COWFS_CONFORMANCE_XATTR_NAMES").as_deref(),
                Ok("1") | Ok("prefixed")
            ),
            no_device_privilege: false,
            link_limit: std::env::var("COWFS_CONFORMANCE_LINK_LIMIT")
                .ok()
                .and_then(|v| v.parse().ok()),
            timeout: std::env::var("COWFS_CONFORMANCE_TIMEOUT_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .map(Duration::from_secs),
        }
    }
}

/// One line of a `Report`.
#[derive(Debug)]
pub struct CheckResult {
    pub check: Check,
    pub outcome: Outcome,
    pub elapsed: Duration,
    /// The check hung and its thread is still running.
    pub leaked: bool,
}

/// A check that `Options::skip` kept from running.
#[derive(Debug)]
pub struct Skipped {
    pub check: Check,
    pub reason: String,
}

/// Results of `run_all`.
#[derive(Debug, Default)]
pub struct Report {
    pub results: Vec<CheckResult>,
    pub skipped_heavy: usize,
    /// Checks skipped through `Options::skip`, with reasons.
    pub skipped: Vec<Skipped>,
    /// Checks left out because they are above `Options::level`.
    pub above_level: usize,
    /// Problems with the options themselves, such as a skip that names no check.
    pub config_errors: Vec<String>,
}

impl Report {
    pub fn failures(&self) -> Vec<&CheckResult> {
        self.results.iter().filter(|r| r.outcome.is_err()).collect()
    }

    /// True when nothing failed and the options were valid. Skips do not make it false: look at
    /// `skipped_count`.
    pub fn passed(&self) -> bool {
        self.failures().is_empty() && self.config_errors.is_empty()
    }

    /// Checks skipped through `Options::skip`.
    pub fn skipped_count(&self) -> usize {
        self.skipped.len()
    }

    /// Checks whose thread hung and was left running.
    pub fn leaked_count(&self) -> usize {
        self.results.iter().filter(|r| r.leaked).count()
    }

    /// Panics with the table if anything failed. On success it prints every skip with its
    /// reason to stderr.
    pub fn assert_ok(&self) {
        assert!(self.passed(), "conformance failed:\n{}", self.table());
        if !self.skipped.is_empty() {
            eprintln!("conformance: {} check(s) skipped:", self.skipped.len());
            for s in &self.skipped {
                eprintln!("  {}: {}", s.check.name, s.reason);
            }
        }
    }

    /// A plain-text table: status, level, category, name, time, failure reason or skip reason.
    pub fn table(&self) -> String {
        let mut s = String::new();
        for r in &self.results {
            let status = if r.outcome.is_ok() { "ok  " } else { "FAIL" };
            let _ = write!(
                s,
                "{status} {:<8} {:<14} {:<48} {:>7.2?}",
                r.check.level, r.check.category, r.check.name, r.elapsed
            );
            if r.leaked {
                s.push_str("  [LEAKED THREAD]");
            }
            if let Err(f) = &r.outcome {
                let _ = write!(s, "  {f}");
            }
            s.push('\n');
        }
        for k in &self.skipped {
            let _ = writeln!(
                s,
                "SKIP {:<8} {:<14} {:<48} {:>7}  {}",
                k.check.level, k.check.category, k.check.name, "-", k.reason
            );
        }
        for e in &self.config_errors {
            let _ = writeln!(s, "ERROR {e}");
        }
        let _ = writeln!(
            s,
            "{} run, {} failed, {} skipped, {} heavy not enabled, {} above the level, {} leaked threads",
            self.results.len(),
            self.failures().len(),
            self.skipped.len(),
            self.skipped_heavy,
            self.above_level,
            self.leaked_count()
        );
        s
    }
}

/// Runs the selected checks, each on a fresh filesystem from `factory`.
pub fn run_all(factory: &dyn Fn() -> Arc<dyn Vfs>, opts: &Options) -> Report {
    let checks = all_checks();
    let mut report = Report::default();
    for (name, _) in &opts.skip {
        if !checks.iter().any(|c| c.name == name) {
            report
                .config_errors
                .push(format!("skip names {name:?}, which is not a check"));
        }
    }
    for check in checks {
        let id = format!("{}::{}", check.category, check.name);
        if opts.filter.as_ref().is_some_and(|f| !id.contains(f)) {
            continue;
        }
        if opts.level.is_some_and(|l| check.level > l) {
            report.above_level += 1;
            continue;
        }
        if let Some((_, reason)) = opts.skip.iter().find(|(n, _)| n == check.name) {
            report.skipped.push(Skipped {
                check,
                reason: reason.clone(),
            });
            continue;
        }
        if check.heavy && !opts.heavy {
            report.skipped_heavy += 1;
            continue;
        }
        if check.link_limit_required() && opts.link_limit.is_none() {
            report.skipped.push(Skipped {
                check,
                reason: "backend did not declare a small hardlink limit (Options::link_limit)"
                    .into(),
            });
            continue;
        }
        let start = Instant::now();
        let (outcome, leaked) = run_check_with(&check, factory, opts, opts.timeout);
        report.results.push(CheckResult {
            check,
            outcome,
            elapsed: start.elapsed(),
            leaked,
        });
    }
    report
}
