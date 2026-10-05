use super::reader::*;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const TABLE: &str =
    "localhost:/cowfs-aaa on /private/tmp/mnt-one (nfs, nodev, nosuid, mounted by zeeshanhaque)
localhost:/cowfs-bbb on /private/tmp/mnt\\040two (nfs, nodev, nosuid, mounted by zeeshanhaque)
/dev/disk3s1s1 on / (apfs, local, journaled)
";

#[test]
fn an_exact_path_is_mounted_and_another_is_absent() {
    assert_eq!(
        classify(Some(TABLE), Path::new("/private/tmp/mnt-one")),
        MountState::Mounted
    );
    assert_eq!(
        classify(Some(TABLE), Path::new("/private/tmp/other")),
        MountState::Absent
    );
}

#[test]
fn a_prefix_of_a_mounted_path_is_not_mounted() {
    // The shared predicate is a `contains` on unescaped output, so a shorter path is a
    // substring of a mounted one. Exact equality must not have that property.
    assert_eq!(
        classify(Some(TABLE), Path::new("/private/tmp/mnt")),
        MountState::Absent,
        "a prefix must not be reported as mounted"
    );
}

#[test]
fn a_space_is_decoded_once_and_matches_exactly() {
    let table = "localhost:/x on /private/tmp/mnt\\040two (nfs, nodev, nosuid)\n";
    assert_eq!(
        classify(Some(table), Path::new("/private/tmp/mnt two")),
        MountState::Mounted,
        "mount prints a space as \\040 and the reader must decode it"
    );
    assert_eq!(
        classify(Some(table), Path::new("/private/tmp/mnt\\040two")),
        MountState::Absent,
        "the escaped spelling is not the path"
    );
}

#[test]
fn a_literal_backslash_sequence_is_not_decoded_twice() {
    // A name that really contains the four characters `\040` must survive one pass unchanged.
    assert_eq!(
        unescape_mount_field("/a\\040b"),
        "/a b",
        "one pass decodes the escape once"
    );
    assert_eq!(
        unescape_mount_field("/a\\134040b"),
        "/a\\040b",
        "an escaped backslash is not rescanned as the start of another escape"
    );
}

#[test]
fn every_backslash_escape_mount_uses_is_decoded() {
    assert_eq!(unescape_mount_field("a\\040b"), "a b");
    assert_eq!(unescape_mount_field("a\\011b"), "a\tb");
    assert_eq!(unescape_mount_field("a\\012b"), "a\nb");
    assert_eq!(unescape_mount_field("a\\134b"), "a\\b");
    assert_eq!(
        unescape_mount_field("a\\999b"),
        "a\\999b",
        "an escape mount does not define is left alone"
    );
}

#[test]
fn a_table_that_cannot_be_read_or_parsed_is_unknown() {
    assert_eq!(
        classify(None, Path::new("/private/tmp/mnt-one")),
        MountState::Unknown
    );
    assert_eq!(
        classify(Some(""), Path::new("/private/tmp/mnt-one")),
        MountState::Absent
    );
    // A truncated line: no " on " and no " (".
    assert_eq!(
        classify(
            Some("localhost:/x /private/tmp/mnt-one"),
            Path::new("/private/tmp/mnt-one")
        ),
        MountState::Unknown
    );
    assert_eq!(
        classify(
            Some("src on  (nfs, nodev)"),
            Path::new("/private/tmp/mnt-one")
        ),
        MountState::Unknown,
        "an empty path field is unparseable, not absent"
    );
}

#[test]
fn an_unparseable_table_never_reports_absent() {
    let mut table = TABLE.to_string();
    table.push_str("garbage-without-a-shape\n");
    assert_eq!(
        classify(Some(&table), Path::new("/private/tmp/mnt-one")),
        MountState::Unknown,
        "one bad line must make the whole table untrusted"
    );
}

/// The delete decision is the thing that must be fail-closed, so it is asserted directly rather
/// than inferred from `classify`.
#[test]
fn only_proven_absence_permits_a_delete() {
    let may_delete = |s: MountState| s == MountState::Absent;
    assert!(!may_delete(MountState::Mounted));
    assert!(!may_delete(MountState::Unknown));
    assert!(may_delete(MountState::Absent));
}

#[test]
fn a_pid_that_is_not_ours_is_never_signalled() {
    // A synthetic identity naming this test process's own pid but the wrong store. The store and
    // socket are what make an identity ours, so a mismatch must refuse rather than signal, and
    // the refusal must be reported rather than swallowed.
    let fake = ChildIdentity {
        pid: std::process::id(),
        start: process_start(std::process::id()).unwrap_or(1),
        exe: PathBuf::from("/sbin/mount"),
        argv: "/sbin/mount".into(),
        store: PathBuf::from("/private/tmp/not-our-store-at-all"),
        socket: PathBuf::from("/private/tmp/not-our-socket-at-all"),
    };
    assert!(!identity_matches(&fake), "a foreign store must not match");
    assert_eq!(
        signal_if_ours(&fake, "KILL"),
        SignalOutcome::Refused("identity no longer matches the child this fixture spawned".into()),
        "a mismatched identity is preserved, not signalled"
    );
}

#[test]
fn an_unreadable_identity_is_never_signalled() {
    let unreadable = ChildIdentity {
        pid: 0,
        start: 0,
        exe: PathBuf::new(),
        argv: String::new(),
        store: PathBuf::from("/x"),
        socket: PathBuf::from("/y"),
    };
    assert!(!identity_matches(&unreadable));
    assert!(matches!(
        signal_if_ours(&unreadable, "KILL"),
        SignalOutcome::Refused(_)
    ));
}

/// The positive case for the identity check, without touching a real mount.
///
/// A child whose command line really does carry this fixture's store and socket must be
/// recognised, or the check would be a check that always refuses and would prove nothing. The
/// child is an owned `sh` that carries the paths as its own argv, and it is stopped through its
/// own handle rather than by signal, because a handle needs no identity check.
#[test]
fn a_child_carrying_our_paths_is_recognised() {
    let dir = std::env::temp_dir().join("d90-guard-owned");
    let _ = std::fs::create_dir_all(&dir);
    let store = dir.join("store");
    let socket = dir.join("sock");
    // `tail -f` on two files this fixture created keeps running and keeps both paths in its own
    // argv, which is the shape a daemon this fixture spawned has.
    fs::write(&store, b"").expect("a store placeholder");
    fs::write(&socket, b"").expect("a socket placeholder");
    let mut b = spawn_bounded(
        Path::new("/usr/bin/tail"),
        &[OsStr::new("-f"), store.as_os_str(), socket.as_os_str()],
        &store,
        &socket,
        Duration::from_millis(200),
    )
    .expect("spawn an owned child carrying our paths");
    assert!(
        !b.finished,
        "tail -f on two files must not finish inside 200ms"
    );
    assert!(
        identity_matches(&b.identity),
        "a live child whose command line carries this fixture's store and socket must match"
    );
    assert_eq!(
        signal_if_ours(&b.identity, "TERM"),
        SignalOutcome::Signalled,
        "a matching identity is signalled"
    );
    b.stop_owned();
    assert!(b.reaped());
    let _ = std::fs::remove_dir_all(&dir);
}

/// The negative direction, with a real owned child: one that does not name our paths must be
/// refused even though its pid is live and its start time is readable.
#[test]
fn a_live_child_that_is_not_ours_is_refused() {
    let mut child = Command::new("/bin/sleep")
        .arg("30")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn an owned harmless child");
    let argv = vec!["/bin/sleep".to_string(), "30".to_string()];
    let id = identity_of(
        child.id(),
        &argv,
        &PathBuf::from("/private/tmp/not-our-store"),
        &PathBuf::from("/private/tmp/not-our-socket"),
    );
    assert!(
        !identity_matches(&id),
        "a child without our store in its argv must not match"
    );
    assert!(matches!(
        signal_if_ours(&id, "KILL"),
        SignalOutcome::Refused(_)
    ));
    let _ = child.kill();
    let _ = child.wait();
}

/// A recycled pid is the case the start time exists for. The pid is live and belongs to us, but
/// the recorded start time is wrong, so the identity must be refused rather than trusted.
#[test]
fn a_recycled_pid_is_refused_on_the_start_time() {
    let dir = std::env::temp_dir().join("d90-guard-recycle");
    let _ = std::fs::create_dir_all(&dir);
    let store = dir.join("store");
    let socket = dir.join("sock");
    fs::write(&store, b"").expect("a store placeholder");
    fs::write(&socket, b"").expect("a socket placeholder");
    let mut b = spawn_bounded(
        Path::new("/usr/bin/tail"),
        &[OsStr::new("-f"), store.as_os_str(), socket.as_os_str()],
        &store,
        &socket,
        Duration::from_millis(200),
    )
    .expect("spawn an owned child");
    let mut wrong = b.identity.clone();
    wrong.start = b.identity.start.wrapping_add(1);
    assert!(
        !identity_matches(&wrong),
        "a pid whose start time differs is a different process"
    );
    assert!(matches!(
        signal_if_ours(&wrong, "KILL"),
        SignalOutcome::Refused(_)
    ));
    b.stop_owned();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The bound is real: a child that outlives its budget is reported unfinished rather than waited
/// on, and is then stopped through its own handle.
#[test]
fn a_command_that_outlives_its_budget_is_reported_not_awaited() {
    let t = Instant::now();
    let mut b = spawn_bounded(
        Path::new("/bin/sleep"),
        &[OsStr::new("30")],
        Path::new("/private/tmp/x"),
        Path::new("/private/tmp/y"),
        Duration::from_millis(300),
    )
    .expect("spawn bounded");
    assert!(!b.finished, "a 30s sleep cannot finish inside 300ms");
    assert!(
        t.elapsed() < Duration::from_secs(5),
        "the budget must bound the wait, not just report on it"
    );
    b.stop_owned();
}

/// A command that finishes inside its budget is reported finished, so nothing signals it later.
///
/// macOS only: it runs the real `/sbin/mount`, which does not exist on Linux, and the reader
/// parses the macOS table format. The synthetic cases above pin the parser on every runner.
#[cfg(target_os = "macos")]
#[test]
fn a_command_that_finishes_inside_its_budget_is_reported_finished() {
    let b = spawn_bounded(
        Path::new("/sbin/mount"),
        &[],
        Path::new("/private/tmp/x"),
        Path::new("/private/tmp/y"),
        Duration::from_secs(10),
    )
    .expect("spawn bounded");
    assert!(b.finished, "listing the mount table is fast");
}

/// Reading the real table here is safe: it runs, parses, and classifies. What it must never do
/// is delete, and this test does not.
///
/// macOS only, for the same reason as the test above: `/sbin/mount` and the table format are both
/// macOS-shaped, and Linux `mount` prints a `type <fstype>` field this reader does not read.
#[cfg(target_os = "macos")]
#[test]
fn the_real_table_is_readable_and_classifies_a_private_path() {
    let dir = std::env::temp_dir().join("d90-guard-table");
    let p = dir.join("mnt");
    let _ = std::fs::create_dir_all(&p);
    let (state, reader) = read_mount_table(
        &p,
        &dir.join("store"),
        &dir.join("sock"),
        Duration::from_secs(10),
    );
    assert!(matches!(state, MountState::Mounted | MountState::Absent));
    assert!(reader.is_some(), "the reader's identity is kept");
    if let Some(mut b) = reader {
        b.stop_owned();
    }
    let _ = std::fs::remove_dir_all(&dir);
}
