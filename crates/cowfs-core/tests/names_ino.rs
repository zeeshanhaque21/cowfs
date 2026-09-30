//! F4 and F5: inode numbers are never reused across a restart, and snapshot names follow the
//! CLI's rules.

mod common;

use common::*;
use cowfs_core::{name_key, validate_snapshot_name, ControlError, Core};
use cowfs_vfs::{Error, Vfs, ROOT_INO};

/// F4: a number handed out in one session is never handed out in the next, so a stale NFS
/// file handle gets `Stale` and never another file's bytes.
#[test]
fn virtual_inode_numbers_are_never_reused_across_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let first = {
        let c = Core::open(dir.path(), test_opts()).unwrap();
        c.create_snapshot("s").unwrap();
        let r = root_entry(&c, "s").ino;
        let a = c.create(r, b"A", 0o644).unwrap().ino;
        c.write(a, 0, b"AAAA").unwrap();
        c.sync().unwrap();
        // a file that is created but never committed still consumed a number
        let pending = c.create(r, b"pending", 0o644).unwrap().ino;
        vec![(a, "A"), (pending, "pending")]
    };
    let c = Core::open(dir.path(), test_opts()).unwrap();
    let r = root_entry(&c, "s").ino;
    for (old, what) in first.iter().copied() {
        assert_eq!(
            c.getattr(old),
            Err(Error::Stale),
            "{what}: a number from the previous session is not stale"
        );
        assert!(c.read(old, 0, 16).is_err());
    }
    let mut fresh = Vec::new();
    for i in 0..200 {
        let a = c.create(r, format!("n{i}").as_bytes(), 0o644).unwrap().ino;
        c.write(a, 0, b"BBBB").unwrap();
        fresh.push(a);
    }
    for a in &fresh {
        assert!(
            !first.iter().any(|(n, _)| *n == *a),
            "a new file got an old session's number {a:#x}"
        );
    }
    c.sync().unwrap();
    assert_eq!(read_all(&c, fresh[0]), b"BBBB");
    assert_eq!(read_all(&c, c.lookup(r, b"A").unwrap().ino), b"AAAA");
}

/// F4: the reservation is durable, so a process that dies between sessions still cannot reuse a
/// number. The child creates files and aborts without syncing or closing.
#[test]
fn the_virtual_number_reservation_survives_a_crash() {
    let dir = tempfile::tempdir().unwrap();
    let exe = std::env::current_exe().unwrap();
    let mut last = 0;
    for _ in 0..3 {
        let _ = std::fs::remove_file(dir.path().join("last.txt"));
        let out = std::process::Command::new(&exe)
            .args([
                "the_virtual_number_reservation_survives_a_crash_child",
                "--exact",
                "--nocapture",
            ])
            .env("COWFS_VIRT_CRASH_DIR", dir.path())
            .output()
            .unwrap();
        let n: u64 = std::fs::read_to_string(dir.path().join("last.txt"))
            .map(|s| s.trim().parse().expect("a number"))
            .unwrap_or_else(|e| {
                panic!(
                    "child reported no number: {e}; stdout {:?} stderr {:?}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                )
            });
        assert!(
            n > last,
            "the child handed out {n:#x}, not above {last:#x}; child stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        last = n;
    }
    let c = Core::open(dir.path(), test_opts()).unwrap();
    let r = root_entry(&c, "s").ino;
    let a = c.create(r, b"after", 0o644).unwrap().ino;
    assert!(
        a > last,
        "{a:#x} reused a number from a killed session ({last:#x})"
    );
    c.sync().unwrap();
}

#[test]
fn the_virtual_number_reservation_survives_a_crash_child() {
    let Ok(dir) = std::env::var("COWFS_VIRT_CRASH_DIR") else {
        return;
    };
    let c = Core::open(&dir, test_opts()).unwrap();
    if c.snapshot_view("s").is_err() {
        c.create_snapshot("s").unwrap();
    }
    let r = root_entry(&c, "s").ino;
    let mut last = 0;
    for i in 0..50 {
        last = c.create(r, format!("f{i}").as_bytes(), 0o644).unwrap().ino;
    }
    let mut f = std::fs::File::create(std::path::Path::new(&dir).join("last.txt")).unwrap();
    use std::io::Write as _;
    writeln!(f, "{last}").unwrap();
    f.sync_all().unwrap();
    std::process::abort();
}

/// F5: the names the CLI refuses are refused here too, and names that alias each other collide.
#[test]
fn snapshot_names_follow_the_cli_rules() {
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(dir.path(), test_opts()).unwrap();
    for bad in [
        "",
        ".",
        "..",
        ".git",
        "._x",
        ".nfs1",
        "a/b",
        "a\nb",
        "a\u{1b}[31m",
        "a\tb",
        "\u{85}",
    ] {
        assert!(
            validate_snapshot_name(bad).is_err(),
            "{bad:?} must be refused"
        );
        assert!(c.create_snapshot(bad).is_err(), "{bad:?} must be refused");
    }
    assert!(c.create_snapshot(&"x".repeat(256)).is_err());
    c.create_snapshot("ok").unwrap();
    c.create_snapshot("caf\u{e9}").unwrap();
    for alias in ["cafe\u{301}", "CAF\u{c9}", "Caf\u{e9}", "OK"] {
        assert_eq!(
            c.create_snapshot(alias),
            Err(ControlError::Exists),
            "{alias:?} aliases an existing name"
        );
    }
    assert_eq!(name_key("caf\u{e9}"), name_key("cafe\u{301}"));
    let names: Vec<String> = c
        .list_snapshots()
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(names.len(), 2, "{names:?}");
    // the same rule for fork, rename and promote
    assert_eq!(c.fork_snapshot("ok", "OK"), Err(ControlError::Exists));
    assert_eq!(
        c.rename_snapshot("ok", "CAF\u{c9}"),
        Err(ControlError::Exists)
    );
    assert_eq!(
        c.promote_base("ok", "ok"),
        Err(ControlError::InvalidName(
            "source and target are the same snapshot"
        ))
    );
    c.check().unwrap();
    let r = root_entry(&c, "ok").ino;
    mkfile(&c, r, "f", b"x");
    assert!(c.rename_snapshot("ok", "ok2").is_ok());
}
