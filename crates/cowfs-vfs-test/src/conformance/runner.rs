use std::fmt::Write as _;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use cowfs_vfs::Vfs;

use super::ctx::{Ctx, Failure, Outcome};

/// One named conformance check.
#[derive(Clone, Copy)]
pub struct Check {
    pub name: &'static str,
    /// The module the check lives in: basic, io, readdir, rename, ...
    pub category: &'static str,
    /// Heavy checks only run when `COWFS_CONFORMANCE_HEAVY=1` (or `Options::heavy`).
    pub heavy: bool,
    pub run: fn(&Ctx) -> Outcome,
}

impl std::fmt::Debug for Check {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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

const NORMAL_TIMEOUT: Duration = Duration::from_secs(300);
const HEAVY_TIMEOUT: Duration = Duration::from_secs(1800);

/// Runs one check on a fresh filesystem from `factory`. A panic or a hang becomes a `Failure`.
pub fn run_check(check: &Check, factory: &dyn Fn() -> Arc<dyn Vfs>) -> Outcome {
    let ctx = Ctx::new(factory());
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
        return Err(Failure(format!("cannot spawn check thread: {e}")));
    }
    let limit = if check.heavy {
        HEAVY_TIMEOUT
    } else {
        NORMAL_TIMEOUT
    };
    rx.recv_timeout(limit).unwrap_or_else(|_| {
        Err(Failure(format!(
            "no result after {limit:?}: hang or deadlock"
        )))
    })
}

/// Runs the check called `name` (see `all_checks`).
pub fn run_named(name: &str, factory: &dyn Fn() -> Arc<dyn Vfs>) -> Outcome {
    match all_checks().into_iter().find(|c| c.name == name) {
        Some(c) => run_check(&c, factory),
        None => Err(Failure(format!("no check named {name}"))),
    }
}

/// Like `run_named` but panics with the failure. This is what `conformance_tests!` expands to.
pub fn assert_named(name: &str, factory: &dyn Fn() -> Arc<dyn Vfs>) {
    if let Err(f) = run_named(name, factory) {
        panic!("conformance check {name} failed: {f}");
    }
}

/// Which checks `run_all` runs.
#[derive(Clone, Debug, Default)]
pub struct Options {
    pub heavy: bool,
    /// Only checks whose `category::name` contains this text.
    pub filter: Option<String>,
}

impl Options {
    /// Heavy checks follow `COWFS_CONFORMANCE_HEAVY`, the filter follows `COWFS_CONFORMANCE_FILTER`.
    pub fn from_env() -> Self {
        Self {
            heavy: heavy_enabled(),
            filter: std::env::var("COWFS_CONFORMANCE_FILTER").ok(),
        }
    }
}

/// One line of a `Report`.
#[derive(Debug)]
pub struct CheckResult {
    pub check: Check,
    pub outcome: Outcome,
    pub elapsed: Duration,
}

/// Results of `run_all`.
#[derive(Debug, Default)]
pub struct Report {
    pub results: Vec<CheckResult>,
    pub skipped_heavy: usize,
}

impl Report {
    pub fn failures(&self) -> Vec<&CheckResult> {
        self.results.iter().filter(|r| r.outcome.is_err()).collect()
    }

    pub fn passed(&self) -> bool {
        self.failures().is_empty()
    }

    /// A plain-text table: status, category, name, time, failure reason.
    pub fn table(&self) -> String {
        let mut s = String::new();
        for r in &self.results {
            let status = if r.outcome.is_ok() { "ok  " } else { "FAIL" };
            let _ = write!(
                s,
                "{status} {:<14} {:<48} {:>7.2?}",
                r.check.category, r.check.name, r.elapsed
            );
            if let Err(f) = &r.outcome {
                let _ = write!(s, "  {f}");
            }
            s.push('\n');
        }
        let _ = writeln!(
            s,
            "{} run, {} failed, {} heavy skipped",
            self.results.len(),
            self.failures().len(),
            self.skipped_heavy
        );
        s
    }
}

/// Runs the selected checks, each on a fresh filesystem from `factory`.
pub fn run_all(factory: &dyn Fn() -> Arc<dyn Vfs>, opts: &Options) -> Report {
    let mut report = Report::default();
    for check in all_checks() {
        let id = format!("{}::{}", check.category, check.name);
        if opts.filter.as_ref().is_some_and(|f| !id.contains(f)) {
            continue;
        }
        if check.heavy && !opts.heavy {
            report.skipped_heavy += 1;
            continue;
        }
        let start = Instant::now();
        let outcome = run_check(&check, factory);
        report.results.push(CheckResult {
            check,
            outcome,
            elapsed: start.elapsed(),
        });
    }
    report
}
