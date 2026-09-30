//! The conformance suite: named checks over any `Vfs`.

macro_rules! ensure {
    ($cond:expr, $($msg:tt)+) => {
        if !$cond {
            return Err($crate::conformance::Failure(format!($($msg)+)));
        }
    };
}

macro_rules! ensure_eq {
    ($left:expr, $right:expr, $($msg:tt)+) => {{
        let (l, r) = (&$left, &$right);
        if l != r {
            return Err($crate::conformance::Failure(format!(
                "{}: got {:?}, want {:?}",
                format!($($msg)+),
                l,
                r
            )));
        }
    }};
}

/// Expects `Err(want)` from a `Vfs` call.
macro_rules! ensure_err {
    ($result:expr, $want:expr, $($msg:tt)+) => {
        match $result {
            Err(e) if e == $want => {}
            Err(e) => {
                return Err($crate::conformance::Failure(format!(
                    "{}: got error {:?}, want {:?}",
                    format!($($msg)+),
                    e,
                    $want
                )))
            }
            Ok(_) => {
                return Err($crate::conformance::Failure(format!(
                    "{}: succeeded, want error {:?}",
                    format!($($msg)+),
                    $want
                )))
            }
        }
    };
}

mod ctx;
mod list;
mod runner;

pub mod attrs;
pub mod basic;
pub mod concurrency;
pub mod dirs;
pub mod io;
pub mod lifecycle;
pub mod links;
pub mod names;
pub mod readdir;
pub mod readonly_mode;
pub mod rename;
pub mod symlinks;
pub mod xattrs;

pub use ctx::{pattern, Ctx, Failure, Outcome};
pub use runner::{
    all_checks, assert_named, heavy_enabled, run_all, run_check, run_named, Check, CheckResult,
    Options, Report,
};
