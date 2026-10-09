//! Issue 259: an absent treehouse is a skip on a developer machine and a failure in CI.
//!
//! The probe is `#[ignore]`d and run as a child of this binary with the environment a runner has and
//! no treehouse reachable, so the real `require_treehouse!` macro is what is judged, not a copy.

mod common;

use std::process::Command;

const PROBE: &str = "probe_require_treehouse_with_no_treehouse";

/// Not a test on its own: the child process below runs it with no treehouse on PATH.
#[test]
#[ignore = "run by the tests below in a child process with no treehouse reachable"]
fn probe_require_treehouse_with_no_treehouse() {
    let _bin = require_treehouse!();
    println!("PROBE-REACHED-BODY");
}

fn run_probe(env: &[(&str, &str)]) -> (bool, String) {
    let home = tempfile::tempdir().expect("tempdir");
    let mut cmd = Command::new(std::env::current_exe().expect("current exe"));
    // An empty PATH resolves a bare `treehouse` against the working directory, so it is an empty one.
    cmd.current_dir(home.path());
    cmd.args([
        "--ignored",
        "--exact",
        PROBE,
        "--nocapture",
        "--test-threads=1",
    ])
    // No PATH entry, no $HOME/.local/bin/treehouse, no override: treehouse cannot be found.
    .env_clear()
    .env("PATH", "")
    .env("HOME", home.path());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run the probe");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

#[test]
fn a_missing_treehouse_fails_the_test_when_ci_is_set() {
    let (ok, text) = run_probe(&[("CI", "true")]);
    assert!(
        !ok,
        "the probe passed with CI set and no treehouse:\n{text}"
    );
    assert!(text.contains("issue 259"), "unexpected failure:\n{text}");
    assert!(!text.contains("PROBE-REACHED-BODY"), "{text}");
}

#[test]
fn a_missing_treehouse_fails_the_test_when_it_is_explicitly_required() {
    let (ok, text) = run_probe(&[("COWFS_REQUIRE_TREEHOUSE", "1")]);
    assert!(!ok, "the probe passed with the requirement set:\n{text}");
    assert!(text.contains("issue 259"), "unexpected failure:\n{text}");
}

#[test]
fn a_missing_treehouse_still_skips_with_a_visible_reason_on_a_developer_machine() {
    for env in [&[][..], &[("COWFS_REQUIRE_TREEHOUSE", "0"), ("CI", "")][..]] {
        let (ok, text) = run_probe(env);
        assert!(ok, "the probe failed with treehouse optional:\n{text}");
        assert!(text.contains("skipping probe_require_treehouse"), "{text}");
        assert!(!text.contains("PROBE-REACHED-BODY"), "{text}");
    }
}

#[test]
fn the_requirement_follows_the_environment() {
    let was = (
        std::env::var_os("CI"),
        std::env::var_os("COWFS_REQUIRE_TREEHOUSE"),
    );
    // One test mutates the environment, and no other test in this binary reads it in-process.
    for (ci, req, want) in [
        (None, None, false),
        (Some(""), None, false),
        (Some("true"), None, true),
        (None, Some("1"), true),
        (None, Some("0"), false),
        (None, Some(""), false),
    ] {
        match ci {
            Some(v) => std::env::set_var("CI", v),
            None => std::env::remove_var("CI"),
        }
        match req {
            Some(v) => std::env::set_var("COWFS_REQUIRE_TREEHOUSE", v),
            None => std::env::remove_var("COWFS_REQUIRE_TREEHOUSE"),
        }
        assert_eq!(common::treehouse_required(), want, "CI={ci:?} REQ={req:?}");
    }
    for (name, v) in [("CI", was.0), ("COWFS_REQUIRE_TREEHOUSE", was.1)] {
        match v {
            Some(v) => std::env::set_var(name, v),
            None => std::env::remove_var(name),
        }
    }
}

#[test]
fn a_found_treehouse_is_returned_whether_or_not_it_is_required() {
    let bin = std::path::PathBuf::from("/x/treehouse");
    for required in [false, true] {
        assert_eq!(
            common::treehouse_or_skip(required, Ok(bin.clone())),
            Some(bin.clone())
        );
    }
}

/// A treehouse that answers `--version` with `version`, in a directory of its own.
fn fake_treehouse(version: &str) -> (tempfile::TempDir, String) {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = dir.path().join("treehouse");
    std::fs::write(&bin, format!("#!/bin/sh\necho {version}\n")).expect("write");
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let path = bin.display().to_string();
    (dir, path)
}

#[test]
fn a_treehouse_older_than_3_1_0_skips_on_a_developer_machine_and_fails_in_ci() {
    for old in ["v2.0.0", "v3.0.9", "garbage"] {
        let (_keep, bin) = fake_treehouse(old);
        let (ok, text) = run_probe(&[("COWFS_TREEHOUSE_BIN", &bin)]);
        assert!(
            ok,
            "{old}: an old treehouse must skip when optional:\n{text}"
        );
        assert!(text.contains("is older than 3.1.0"), "{old}: {text}");
        assert!(!text.contains("PROBE-REACHED-BODY"), "{old}: {text}");
        for gate in [("CI", "true"), ("COWFS_REQUIRE_TREEHOUSE", "1")] {
            let (ok, text) = run_probe(&[("COWFS_TREEHOUSE_BIN", &bin), gate]);
            assert!(
                !ok,
                "{old}: an old treehouse must fail with {gate:?}:\n{text}"
            );
            assert!(text.contains("is older than 3.1.0"), "{old}: {text}");
            assert!(!text.contains("PROBE-REACHED-BODY"), "{old}: {text}");
        }
    }
}

#[test]
fn a_treehouse_at_or_above_3_1_0_is_used() {
    for good in ["v3.1.0", "v3.1.2", "v4.0.0"] {
        let (_keep, bin) = fake_treehouse(good);
        let (ok, text) = run_probe(&[("COWFS_TREEHOUSE_BIN", &bin), ("CI", "true")]);
        assert!(ok, "{good} must be accepted:\n{text}");
        assert!(text.contains("PROBE-REACHED-BODY"), "{good}: {text}");
    }
}

#[test]
fn versions_parse() {
    assert_eq!(common::parse_version("v3.1.2"), Some((3, 1, 2)));
    assert_eq!(common::parse_version("3.10.0\n"), Some((3, 10, 0)));
    assert_eq!(common::parse_version("v2.0.0-rc1"), Some((2, 0, 0)));
    assert_eq!(common::parse_version("nope"), None);
}
