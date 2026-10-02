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

/// F7: a session alias is one entry per inode the session created and still has a name for, and it
/// costs a bounded number of bytes per inode. In its own test binary so the RSS measurement is not
/// polluted by the other cache tests.
#[test]
fn a_session_alias_costs_a_bounded_number_of_bytes_per_inode() {
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
    // every live inode keeps one alias for the session, so the count tracks the file count; the node
    // table is still bounded, so the alias entry is what a session pays per file
    assert_eq!(
        s.aliases,
        n as usize + 1,
        "the alias count must be one per live inode: {s:?}"
    );
    assert!(s.nodes <= 64, "the node table grew to {}: {s:?}", s.nodes);
    // Measured inside this process between two marks, so other tests in the same binary cannot be
    // blamed. redb's own growth for a growing database file was measured at 967 bytes per file on
    // `cowfs-meta` alone, so the ceiling below is generous; it exists to catch a per-file cost in
    // cowfs-core, not to police redb.
    if n > 6000 {
        let (at, rss_at) = marks[0];
        let per_file = (rss - rss_at) as f64 * 1024.0 / (n - at) as f64;
        println!("{per_file:.0} bytes per file after {at} files (RSS, includes redb)");
        assert!(
            per_file < 4096.0,
            "{} bytes per file after {at} files: something is still per-file",
            per_file
        );
    }
    // the files are all still there and readable, each under the number it was given
    let a = c.lookup(d, b"f0").expect("lookup f0").ino;
    assert_eq!(read_all(&c, a), b"hello", "content of f0");
    let last = c
        .lookup(d, format!("f{}", n - 1).as_bytes())
        .expect("lookup last")
        .ino;
    assert_eq!(read_all(&c, last), b"hello", "content of the last file");
    c.check().unwrap();
}

/// The cost of a session alias, measured rather than guessed: 500,000 creates, the size of a large
/// `git clone`. Two numbers are reported: the alias table's own bytes from its bucket counts, which
/// is exact, and the process RSS delta, which also carries the dentry table and redb's growth and so
/// is an upper bound on everything rather than on this.
#[test]
#[ignore = "heavy: ALIAS_FILES=500000 cargo test -p cowfs-core --release --test alias -- --ignored alias_bytes_at_500k_creates"]
fn alias_bytes_at_500k_creates() {
    let n: u32 = std::env::var("ALIAS_FILES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(500_000);
    let dir = tempfile::tempdir().unwrap();
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
    let mut dirs = Vec::new();
    for i in 0..100 {
        let d = c.mkdir(r, format!("d{i}").as_bytes(), 0o755).unwrap().ino;
        c.forget(d, 1);
        dirs.push(d);
    }
    c.flush().unwrap();
    let (entries0, bytes0) = c.alias_table();
    let rss0 = rss_kib();
    let t = std::time::Instant::now();
    for i in 0..n {
        let a = c
            .create(dirs[i as usize % 100], format!("f{i}").as_bytes(), 0o644)
            .unwrap()
            .ino;
        c.write(a, 0, b"hello").unwrap();
        c.forget(a, 1);
        if i % 4096 == 4095 {
            c.flush().unwrap();
        }
    }
    c.flush().unwrap();
    let s = c.stats();
    let rss1 = rss_kib();
    let (entries1, bytes1) = c.alias_table();
    let per_alias = (bytes1 - bytes0) as f64 / (entries1 - entries0).max(1) as f64;
    let per_file_rss = (rss1 - rss0) as f64 * 1024.0 / n as f64;
    println!(
        "{n} creates in {:?}: aliases {} nodes {} dentries {}",
        t.elapsed(),
        s.aliases,
        s.nodes,
        s.dentries
    );
    println!(
        "alias table: {entries1} entries, {bytes1} bytes, {per_alias:.1} bytes per entry; \
         RSS {rss0} -> {rss1} KiB, {per_file_rss:.1} bytes per file (includes redb and dentries)"
    );
    assert_eq!(s.aliases, n as usize + 100, "{s:?}");
    // two maps, each one (u64, u64) entry plus its control byte: 17 bytes per bucket, and a hash map
    // keeps its load factor under 7/8, so 17 * 8/7 ≈ 19.4 per entry per map
    // two maps of (u64, u64) plus a control byte each: 17 bytes per bucket. The count of buckets is
    // a power of two, so the honest ceiling is the worst case just past a doubling, 2 * 17 * 2 = 68,
    // and the floor at a full load factor is 2 * 17 * 8/7 = 39. Measured at 500k it is near the
    // ceiling because 500,100 entries sit just above the previous power of two.
    assert!(
        (39.0..68.0).contains(&per_alias),
        "{per_alias:.1} bytes per session alias: outside what two (u64, u64) maps can cost"
    );
    c.check().unwrap();
}

/// The ceiling: past it a create is `NoSpace` with an explanation, not a number that goes stale
/// later. Setting it low is the only way to reach it without a million creates.
#[test]
fn a_create_past_the_alias_ceiling_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(
        dir.path(),
        Options {
            alias_limit: 4,
            ..test_opts()
        },
    )
    .unwrap();
    c.create_snapshot("s").unwrap();
    let r = root_entry(&c, "s").ino;
    // forget as a stateless adapter does, so the only thing holding a number is that its inode has
    // a name
    let mut made = Vec::new();
    for i in 0..4 {
        let a = c.create(r, format!("f{i}").as_bytes(), 0o644).unwrap();
        c.forget(a.ino, 1);
        made.push(a.ino);
    }
    c.flush().unwrap();
    assert_eq!(c.stats().aliases, 4, "{:?}", c.stats());
    let err = c
        .create(r, b"too_many", 0o644)
        .expect_err("the ceiling did not refuse the create");
    assert_eq!(err, Error::NoSpace, "the ceiling refused with {err:?}");
    let msg = c.last_flush_error().expect("the refusal explained nothing");
    assert!(
        msg.contains("session inode limit") && msg.contains('4'),
        "the refusal does not say what the ceiling is: {msg}"
    );
    // the numbers already handed out still work, which is the point of refusing rather than
    // breaking them
    for (i, ino) in made.iter().enumerate() {
        assert_eq!(c.getattr(*ino).unwrap().ino, *ino, "f{i} lost its number");
        let again = c.lookup(r, format!("f{i}").as_bytes()).unwrap().ino;
        assert_eq!(again, *ino, "f{i} is reachable under another number");
        c.forget(again, 1);
    }
    // unlinking one frees its alias, so the ceiling is on live inodes and not on the session
    c.unlink(r, b"f0").unwrap();
    c.flush().unwrap();
    assert_eq!(c.stats().aliases, 3, "{:?}", c.stats());
    let again = c
        .create(r, b"now_ok", 0o644)
        .expect("the ceiling did not free up");
    assert_eq!(c.getattr(again.ino).unwrap().ino, again.ino);
}

/// F7: a number never changes for as long as its inode has a name, whether or not a caller holds a
/// reference or a handle.
#[test]
fn a_number_never_changes_while_its_inode_has_a_name() {
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
    assert_eq!(c.lookup(r, b"held").unwrap().ino, a);
    c.forget(a, 1);
    c.flush().unwrap();
    c.flush().unwrap();
    assert_eq!(
        c.lookup(r, b"held").unwrap().ino,
        a,
        "a file nothing holds changed number"
    );
    assert_eq!(c.getattr(a).unwrap().ino, a, "the number went stale");
    assert_eq!(read_all(&c, a), b"x");
    c.check().unwrap();
}
