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

/// Expects an error whose errno is one of the listed errors' errnos (see `errno_matches`).
macro_rules! ensure_err_any {
    ($result:expr, [$($want:expr),+ $(,)?], $($msg:tt)+) => {{
        let want = [$($want),+];
        match $result {
            Err(e) if $crate::conformance::errno_matches(&e, &want) => {}
            Err(e) => {
                return Err($crate::conformance::Failure(format!(
                    "{}: got error {:?}, want one of {:?}",
                    format!($($msg)+),
                    e,
                    want
                )))
            }
            Ok(_) => {
                return Err($crate::conformance::Failure(format!(
                    "{}: succeeded, want one of {:?}",
                    format!($($msg)+),
                    want
                )))
            }
        }
    }};
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
pub mod special;
pub mod symlinks;
pub mod xattrs;

pub use ctx::{pattern, Ctx, Failure, Outcome};
pub use runner::{
    all_checks, assert_named, assert_skip_names, heavy_enabled, run_all, run_check,
    run_check_timed, run_named, Check, CheckResult, Level, Options, Report, Skipped,
    DEFAULT_HEAVY_TIMEOUT, DEFAULT_TIMEOUT,
};

/// True when `e` has the errno of one of `want`. `PermissionDenied` also accepts `EPERM`,
/// which is what the kernel returns where `Error::PermissionDenied` maps to `EACCES`.
pub(crate) fn errno_matches(e: &cowfs_vfs::Error, want: &[cowfs_vfs::Error]) -> bool {
    let errno = e.errno();
    want.iter().any(|w| {
        w.errno() == errno || (*w == cowfs_vfs::Error::PermissionDenied && errno == libc::EPERM)
    })
}
