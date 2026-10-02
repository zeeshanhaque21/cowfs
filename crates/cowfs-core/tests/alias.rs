//! F7: the alias table must not grow with every file a session creates, and a number stays stable
//! while a caller can still be holding it. In its own test binary so the RSS measurement is not
//! polluted by the other cache tests.

mod common;

use common::*;
use cowfs_core::{Core, Options};
use cowfs_vfs::{Error, Vfs};

fn rss_kib() -> u64 {
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or(0)
}

/// F7: the alias table must not grow with every file a session creates.
#[test]
fn aliases_drain_for_committed_files_with_no_references() {
    let dir = tempfile::tempdir().unwrap();
    // meta's own caches are bounded here so the measurement is of cowfs-core's maps
    let c = Core::open(
        dir.path(),
        Options {
            meta: cowfs_meta::Options {
                node_cache: 4096,
                cache_size: 16 << 20,
                ..Default::default()
            },
            ..test_opts()
        },
    )
    .unwrap();
    c.create_snapshot("s").unwrap();
    let r = root_entry(&c, "s").ino;
    let d = c.mkdir(r, b"d", 0o755).unwrap().ino;
    let n: u32 = std::env::var("ALIAS_FILES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20_000);
    let mut marks = Vec::new();
    for i in 0..n {
        let a = c.create(d, format!("f{i}").as_bytes(), 0o644).unwrap().ino;
        c.write(a, 0, b"hello").unwrap();
        c.forget(a, 1);
        if i % 4096 == 4095 {
            c.flush().unwrap();
        }
        if i == 4999 {
            marks.push((i + 1, rss_kib()));
        }
    }
    c.flush().unwrap();
    let s = c.stats();
    let rss = rss_kib();
    println!(
        "{n} creates: aliases {} nodes {} dentries {} rss -> {rss} KiB",
        s.aliases, s.nodes, s.dentries
    );
    assert!(
        s.aliases <= 64,
        "the alias table grew to {}: {s:?}",
        s.aliases
    );
    assert!(s.nodes <= 64, "the node table grew to {}: {s:?}", s.nodes);
    // Measured inside this process between two marks, so other tests in the same binary cannot be
    // blamed. Before the fix the alias table held one entry per file for the life of the session
    // (the critic measured 500,100 aliases and 469 MiB at 500,000 files); the residue below is
    // redb mapping a growing database file (967 bytes per file measured on `cowfs-meta` alone).
    if n > 6000 {
        let (at, rss_at) = marks[0];
        let per_file = (rss - rss_at) as f64 * 1024.0 / (n - at) as f64;
        println!("{} more files grew {per_file:.0} bytes per file", n - at);
        assert!(
            per_file < 4096.0,
            "{} bytes per file after {at} files: something is still per-file",
            per_file
        );
    }
    // the files are all still there and readable
    let meta_ino = cowfs_meta::Meta::unpack_ino(root_entry(&c, "s").ino).1;
    let names: Vec<String> = c
        .meta()
        .snapshot("s")
        .unwrap()
        .readdir(cowfs_meta::Ino(2), 0, 1_000_000)
        .unwrap()
        .entries
        .iter()
        .map(|e| String::from_utf8_lossy(&e.name).into_owned())
        .collect();
    eprintln!(
        "DBG meta has {} names, f0 {} f19999 {} f15000 {}",
        names.len(),
        names.contains(&"f0".to_string()),
        names.contains(&"f19999".to_string()),
        names.contains(&"f15000".to_string()),
    );
    let _ = meta_ino;
    let a = c.lookup(d, b"f0").expect("lookup f0").ino;
    assert_eq!(read_all(&c, a), b"hello", "content of f0");
    let last = c
        .lookup(d, format!("f{}", n - 1).as_bytes())
        .expect("lookup last")
        .ino;
    assert_eq!(read_all(&c, last), b"hello", "content of the last file");
    c.check().unwrap();
}

/// F7: a number stays stable while a caller holds a reference or a handle.
#[test]
fn a_referenced_file_keeps_its_number_across_a_flush() {
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(dir.path(), test_opts()).unwrap();
    c.create_snapshot("s").unwrap();
    let r = root_entry(&c, "s").ino;
    let a = c.create(r, b"held", 0o644).unwrap().ino;
    c.write(a, 0, b"x").unwrap();
    c.flush().unwrap();
    c.flush().unwrap();
    assert_eq!(
        c.lookup(r, b"held").unwrap().ino,
        a,
        "a referenced file changed number"
    );
    let h = c.open(a).unwrap();
    c.flush().unwrap();
    assert_eq!(c.lookup(r, b"held").unwrap().ino, a);
    c.release(h).unwrap();
    c.flush().unwrap();
    assert_eq!(
        c.lookup(r, b"held").unwrap().ino,
        a,
        "an open file changed number"
    );
    // once nothing holds it the number may change, and the old one is then stale
    c.flush().unwrap();
    let after = c.lookup(r, b"held").unwrap().ino;
    assert_eq!(read_all(&c, after), b"x");
    if after != a {
        assert_eq!(
            c.getattr(a),
            Err(Error::Stale),
            "a released number must be stale"
        );
        assert_eq!(c.mkdir(a, b"d", 0o755), Err(Error::Stale));
        assert_eq!(c.readdir(a, 0, 10), Err(Error::Stale));
    }
    assert!(c.stats().aliases <= 1, "{:?}", c.stats());
}
