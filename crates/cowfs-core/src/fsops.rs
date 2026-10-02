//! Test seams for the durability calls the design argument in `docs/v1-core.md` depends on.
//!
//! Two things, both inert unless a test arms them: a fault rule that makes one `sync` fail, and a
//! trace of the calls in the order they happened.

use std::fs::File;
use std::io;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[doc(hidden)]
pub enum Fault {
    FileSync,
    DirSync,
}

#[derive(Debug)]
struct Rule {
    what: Fault,
    path_has: String,
    times: u32,
}

#[derive(Debug, Default)]
struct State {
    rules: Vec<Rule>,
    trace: Vec<String>,
    armed: bool,
}

fn state() -> &'static Mutex<State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(Mutex::default)
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut s = state().lock().unwrap_or_else(|e| e.into_inner());
    f(&mut s)
}

/// Make the next `times` `sync`s of a file (or directory) whose path contains `path_has` fail.
#[doc(hidden)]
pub fn set_fault(what: Fault, path_has: &str, times: u32) {
    with(|s| {
        s.armed = true;
        s.rules.push(Rule {
            what,
            path_has: path_has.to_string(),
            times,
        });
    });
}

/// Turn the trace on and drop every rule and record.
#[doc(hidden)]
pub fn arm() {
    with(|s| {
        s.armed = true;
        s.rules.clear();
        s.trace.clear();
    });
}

#[doc(hidden)]
pub fn disarm() {
    with(|s| {
        s.armed = false;
        s.rules.clear();
        s.trace.clear();
    });
}

/// The recorded calls, oldest first.
#[doc(hidden)]
pub fn trace_take() -> Vec<String> {
    with(|s| std::mem::take(&mut s.trace))
}

pub(crate) fn note(what: &str) {
    with(|s| {
        if s.armed {
            s.trace.push(what.to_string());
        }
    });
}

fn take_fault(what: Fault, path: &Path) -> io::Result<()> {
    with(|s| {
        if !s.armed {
            return Ok(());
        }
        let name = path.to_string_lossy().into_owned();
        let Some(i) = s
            .rules
            .iter()
            .position(|r| r.what == what && name.contains(&r.path_has))
        else {
            return Ok(());
        };
        if s.rules[i].times == 0 {
            s.rules.remove(i);
            return Ok(());
        }
        s.rules[i].times -= 1;
        Err(io::Error::other(format!("injected sync fault on {name}")))
    })
}

/// Syncs `f` and records `sync_file:<name>`, so a test can assert the ordering around it.
pub(crate) fn sync_file(f: &File, path: &Path) -> io::Result<()> {
    note(&format!("sync_file:{}", file_name(path)));
    take_fault(Fault::FileSync, path)?;
    f.sync_all()
}

/// Syncs the directory `path` and records `sync_dir:<path>`, so a test can name the exact one.
pub(crate) fn sync_dir(path: &Path) -> io::Result<()> {
    note(&format!("sync_dir:{}", path.display()));
    take_fault(Fault::DirSync, path)?;
    File::open(path)?.sync_all()
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.to_string_lossy().into_owned(),
        |n| n.to_string_lossy().into_owned(),
    )
}
