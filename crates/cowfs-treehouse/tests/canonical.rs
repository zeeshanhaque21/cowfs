//! Canonical build paths for issue #17: the opt-in namespace wiring around `run_build`.
//!
//! These are stub-only tests. They cover the default path staying unchanged, the refusals, the
//! config pairing and the argv, all without a mount namespace. The actual namespace integration is
//! measured on Linux by `scripts/namespaces17-treehouse-linux.sh`, which drives the real companion
//! over a real cowfs FUSE mount with a real warm base and two fresh slots.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use cowfs_treehouse::{run_build, Canonical};

fn companion() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_cowfs-treehouse"))
}

fn run(args: &[&str]) -> std::process::Output {
    Command::new(companion())
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("cannot run cowfs-treehouse {args:?}: {e}"))
}

/// A directory tree under a fresh temp dir, cleaned up when the guard drops.
struct Tmp(PathBuf);

impl Tmp {
    fn new(tag: &str) -> Tmp {
        // Shell-safe by construction: these paths are interpolated into the stub scripts below, and
        // `{:?}` on a ThreadId renders `ThreadId(9)`, whose parenthesis was a syntax error inside
        // those scripts. The tag is unique per test, so the pid alone is enough to separate runs.
        let base = fixture_root().join(format!("cowfs-canon-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("the temp dir");
        Tmp(base)
    }

    fn dir(&self, name: &str) -> PathBuf {
        let p = self.0.join(name);
        std::fs::create_dir_all(&p).expect("the subdirectory");
        p
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Where each test puts its fixture: the system temporary directory.
///
/// The Linux stub tests below note a measured flake in this file and the candidate fixes that were
/// measured and rejected for it.
fn fixture_root() -> PathBuf {
    std::env::temp_dir()
}

/// The failure this fixture used to have, and why it writes the way it does. Read before changing it.
///
/// On the Linux host used for issue 17 (kernel 6.12, `/tmp` on tmpfs, several other agents' toolchains
/// running on the same machine), running this test binary repeatedly at 8 test threads fails
/// intermittently in exactly the four tests that exec a stub this process has just written, always
/// with `Text file busy (os error 26)` from `execve`, and never in any other test. Measured rates on
/// the same code, 150 or 200 runs per row:
///
/// | Change | Failures |
/// |---|---|
/// | as written | 4 / 150 |
/// | stub written under a temporary name and `rename`d into place | 11 / 120 |
/// | the four stub tests serialised against each other | 4 / 150 |
/// | every process spawn in this file serialised | 1 / 150 |
/// | fixture moved off the shared tmpfs, into the build directory | 16 / 200 |
///
/// A standalone probe reproduces the same failure with no test harness at all, and after five
/// rejected theories it says something narrower and truer than the table above. Probe source:
/// `scripts/namespaces17-etxtbsy-spike.rs`. Host: Linux 6.12.109+rpt-rpi-2712 aarch64. 1600 execs per
/// row unless stated:
///
/// | Configuration | ETXTBSY |
/// |---|---|
/// | write then exec, `posix_spawn`, 8 threads, on tmpfs | 40 / 1600 |
/// | write then exec, `posix_spawn`, 8 threads, on ext4 | 73 / 1600 |
/// | exec only, no write at all, `posix_spawn`, 8 threads | 0 / 1600 |
/// | exec only, no write at all, `posix_spawn`, 1 thread | 0 / 400 |
/// | write-open and close, zero bytes written, then exec, 8 threads | 4 / 3200 |
/// | write-open and close by a waited-for child, then exec, 8 threads | 0 / 3200 |
///
/// It needs two of them together: a write-open of that file, and concurrency. It is not the
/// `posix_spawn` path, because forcing `fork` + `exec` fails the same, 2 / 400 against 4 / 400.
/// Publishing by `rename` does not help either, 3 / 400, so the inode being exec'd was never open
/// for write and the refusal is not a writer this test can see. Serialising these tests does not
/// help, 4 / 150, because one test execs its own stub twice, back to back. It is not the
/// filesystem, not the shebang and not the loader: a statically linked binary with no interpreter
/// or libraries fails the same way and a differently shaped one fails more often.
///
/// Two earlier claims in this comment were wrong and are withdrawn. The write handle was not ruled
/// out, because the probe mode labelled "no write in the loop" created a fresh directory and wrote
/// before every exec. And the scan that "found nobody holding the file" was looking at the file
/// under test while the writing thread had already closed it, so it answered its own question. At
/// the moment of each failure no process holds the file open and no write-mode descriptor on the box
/// is a library or an executable.
///
/// What is left is a characterisation, not a mechanism, and none is claimed. The write-open on that
/// inode is still in flight when the exec lands, which is consistent with every row above and was
/// inferred from them, not observed. No kernel behaviour is claimed either. The product is not
/// implicated: `run_build` is right to report a helper it cannot exec, and this is a fixture defect.
///
/// So the bytes go to a short-lived child and the parent waits for it, because process exit releases
/// the write-open. Nothing is retried, serialised, slept, ignored or accepted as an error.
/// `write_stub_is_safe_under_concurrent_exec` is the control, `#[ignore]`d because it spawns
/// thousands of processes.
///
/// Writes an executable stub from a short-lived child, and does not return until that child is gone.
///
/// The bytes go over stdin and the path is a positional argument, so nothing a caller supplies is
/// ever shell source. The parent holds no descriptor on the target once the pipe is dropped, and the
/// wait cannot outlive the child: `cat` reads to end of input and then exits.
fn write_stub(path: &Path, body: &str) {
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg("cat > \"$1\"")
        .arg("stub-writer")
        .arg(path.as_os_str())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the stub writer");
    let mut pipe = child.stdin.take().expect("the stub writer takes a pipe");
    let written = pipe.write_all(body.as_bytes());
    drop(pipe); // the write end closes here, which is what lets `cat` reach end of input
    let out = child.wait_with_output().expect("wait for the stub writer");
    // The status is checked before the write result, so a writer that could not open the file
    // reports the shell's own message instead of a bare broken pipe.
    assert!(
        out.status.success(),
        "the stub writer exited {:?}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    written.expect("the stub writer took every byte");
    assert_eq!(
        std::fs::read(path).expect("read the stub back"),
        body.as_bytes(),
        "the stub on disk is not the bytes that were written"
    );
    #[cfg(unix)]
    {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod the stub");
        assert_eq!(
            std::fs::metadata(path)
                .expect("stat the stub")
                .permissions()
                .mode()
                & 0o7777,
            0o755,
            "the stub is not executable"
        );
    }
}

/// The stub on disk is exactly the bytes given, and it is executable, once the writer has exited.
#[test]
fn a_stub_is_byte_exact_and_executable_after_the_writer_exits() {
    let t = Tmp::new("writer-bytes");
    let p = t.dir("bin").join("ns-stub");
    let body = "#!/bin/sh\nexit 0\n";
    write_stub(&p, body);
    assert_eq!(
        std::fs::read(&p).expect("read the stub"),
        body.as_bytes(),
        "the bytes are verbatim"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&p)
                .expect("stat the stub")
                .permissions()
                .mode()
                & 0o7777,
            0o755,
            "the stub is executable"
        );
    }
    assert!(
        Command::new(&p).status().expect("exec the stub").success(),
        "and it runs"
    );
}

/// A path is a path. Spaces, a quote, a glob and a substitution are characters in a filename,
/// because the path is an argument and never part of the command: had it been pasted into the
/// command, `$(exit 3)` would have made the redirect fail and `write_stub` would have panicked
/// before this assertion was reached.
#[test]
fn a_stub_path_is_never_shell_source() {
    let t = Tmp::new("writer-path");
    let dir = t.dir("bin ' quote; * $(exit 3)");
    let p = dir.join("ns-stub");
    write_stub(&p, "#!/bin/sh\nexit 0\n");
    assert!(p.exists(), "the stub is at the literal path {p:?}");
    assert!(
        Command::new(&p).status().expect("exec the stub").success(),
        "and it runs"
    );
}

/// Arbitrary bytes survive the pipe. The first body carries a NUL and is checked for round trip only,
/// because a shell is not required to read a NUL as an ordinary byte. The second carries shell
/// metacharacters and non-ASCII text, and is executed as well.
#[test]
fn arbitrary_stub_bytes_survive_the_pipe() {
    let t = Tmp::new("writer-bytes-arbitrary");
    let p = t.dir("bin").join("ns-stub");
    let with_nul = "#!/bin/sh\n# \0 a\0b\nexit 0\n";
    write_stub(&p, with_nul);
    assert_eq!(
        std::fs::read(&p).expect("read the stub"),
        with_nul.as_bytes(),
        "a NUL round trips byte for byte"
    );

    let body =
        "#!/bin/sh\n# ' \" $ ` \\ * ? | & ; > < ( ) { } [ ] ~ #! \u{e9}\u{4e2d}\u{6587}\nexit 0\n";
    write_stub(&p, body);
    assert_eq!(
        std::fs::read(&p).expect("read the stub"),
        body.as_bytes(),
        "metacharacters round trip byte for byte"
    );
    assert!(
        Command::new(&p).status().expect("exec the stub").success(),
        "and a stub carrying them still runs"
    );
}

/// A writer that cannot create the file must fail the test: never leave a stub behind for a later
/// exec to run, and never report success.
#[test]
fn a_stub_the_writer_cannot_create_is_refused() {
    let t = Tmp::new("writer-refused");
    let p = t.0.join("no-such-directory").join("ns-stub");
    let refused = std::panic::catch_unwind(|| write_stub(&p, "#!/bin/sh\nexit 0\n"))
        .expect_err("a writer that cannot create the file must not pass");
    let message = refused
        .downcast_ref::<String>()
        .map(String::as_str)
        .unwrap_or("a non-string panic");
    assert!(
        message.contains("stub writer"),
        "the failure names the writer, so a silent stub is impossible: {message}"
    );
    assert!(!p.exists(), "nothing may be left behind at {p:?}");
}

/// The control for the failure this fixture used to have, at the seam that had it: many threads each
/// writing their own stub through `write_stub` and exec'ing it. Ignored by default because it spawns
/// thousands of processes; run it with `--ignored` when changing how a stub is written.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "spawns thousands of processes; opt in with --ignored"]
fn write_stub_is_safe_under_concurrent_exec() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};

    let threads = 8;
    let rounds = 400;
    let t = Tmp::new("writer-concurrent");
    let execs = Arc::new(AtomicUsize::new(0));
    let refused = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(Barrier::new(threads));
    let mut handles = Vec::new();
    for i in 0..threads {
        let root = t.0.clone();
        let execs = Arc::clone(&execs);
        let refused = Arc::clone(&refused);
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            let dir = root.join(format!("t{i}"));
            std::fs::create_dir_all(&dir).expect("the thread directory");
            let stub = dir.join("ns-stub");
            barrier.wait();
            for _ in 0..rounds {
                write_stub(&stub, "#!/bin/sh\nexit 0\n");
                match Command::new(&stub).status() {
                    Ok(s) if s.success() => {
                        execs.fetch_add(1, Ordering::SeqCst);
                    }
                    Ok(s) => panic!("the stub exited {s}"),
                    Err(e) => {
                        refused.fetch_add(1, Ordering::SeqCst);
                        eprintln!("thread {i} could not exec its own stub: {e}");
                    }
                }
            }
        }));
    }
    for h in handles {
        if let Err(panic) = h.join() {
            let message = panic
                .downcast_ref::<String>()
                .map(String::as_str)
                .unwrap_or("a writer thread panicked");
            panic!("{message}");
        }
    }
    let refused = refused.load(Ordering::SeqCst);
    let execs = execs.load(Ordering::SeqCst);
    assert_eq!(
        refused,
        0,
        "{refused} of {} execs were refused after write_stub",
        execs + refused
    );
    assert_eq!(execs, threads * rounds, "every exec must have run: {execs}");
}

/// A helper that behaves like `scripts/cowfs-ns-run.sh` but records its argv, so the wiring can be
/// checked without a namespace and without running the real script.
fn recording_helper(t: &Tmp, exit: i32, stderr: &str) -> PathBuf {
    let dir = t.dir("bin");
    let path = dir.join("ns-stub");
    let body = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"{out}\"\nexit {exit}\n{stderr}",
        out = dir.join("argv.txt").display(),
        exit = exit,
        stderr = stderr
    );
    write_stub(&path, &body);
    path
}

#[cfg(target_os = "linux")]
fn recorded(helper: &std::path::Path) -> Vec<String> {
    let p = helper
        .parent()
        .expect("the stub directory")
        .join("argv.txt");
    std::fs::read_to_string(&p)
        .expect("the stub ran and recorded its argv")
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn without_a_canonical_the_build_runs_at_the_slot_path() {
    // The pre-existing behaviour, and the one every macOS caller gets.
    let t = Tmp::new("default");
    let slot = t.dir("slot");
    run_build(&slot, "exit 0", None).expect("a plain build succeeds");
    assert!(
        !slot.join("marker").exists(),
        "nothing should have been written by exit 0"
    );
}

#[test]
fn without_a_canonical_a_failing_build_is_an_io_error() {
    let t = Tmp::new("default-fail");
    let slot = t.dir("slot");
    let err = run_build(&slot, "exit 3", None).expect_err("exit 3 must fail");
    assert!(
        matches!(&err, cowfs_treehouse::Error::Io(m) if m.contains("failed")),
        "expected an Io failure, got {err:?}"
    );
}

#[test]
fn the_canonical_directory_must_be_absolute() {
    let t = Tmp::new("relative");
    let c = Canonical {
        dir: PathBuf::from("relative/path"),
        helper: recording_helper(&t, 0, ""),
    };
    let err = c
        .validate()
        .expect_err("a relative canonical path is refused");
    assert!(err.exit_code() == 2, "a usage error is exit 2: {err:?}");
    assert!(err.to_string().contains("absolute"), "{err}");
}

#[test]
fn a_missing_canonical_directory_is_refused_and_not_created() {
    let t = Tmp::new("missing");
    let absent = t.0.join("never-created");
    let c = Canonical {
        dir: absent.clone(),
        helper: recording_helper(&t, 0, ""),
    };
    let err = c
        .validate()
        .expect_err("a missing canonical directory is refused");
    assert!(err.exit_code() == 2, "{err:?}");
    assert!(err.to_string().contains("must already exist"), "{err}");
    assert!(
        !absent.exists(),
        "the companion must never create the canonical directory"
    );
}

#[test]
fn a_missing_helper_is_refused() {
    let t = Tmp::new("nohelper");
    let c = Canonical {
        dir: t.dir("canonical"),
        helper: t.0.join("no-such-helper.sh"),
    };
    let err = c.validate().expect_err("a missing helper is refused");
    assert!(err.exit_code() == 2, "{err:?}");
    assert!(err.to_string().contains("--ns-helper"), "{err}");
}

#[cfg(target_os = "linux")]
#[test]
fn the_argv_keeps_the_build_command_as_one_element() {
    let t = Tmp::new("argv");
    let slot = t.dir("slot");
    let canonical = t.dir("canonical");
    let helper = recording_helper(&t, 0, "");
    let c = Canonical {
        dir: canonical.clone(),
        helper: helper.clone(),
    };
    // A build command with shell metacharacters and spaces, which is user configuration and has
    // always been a shell string.
    let command = "cargo build --release && echo 'a b' > out.txt";
    c.validate().expect("a valid canonical");
    run_build(&slot, command, Some(&c)).expect("the stub exits 0");

    // No element may repeat the helper's own path: the helper is the program, and Command::new
    // already supplies it. A repeat here made the helper read its own path as --src's value.
    let argv = recorded(&helper);
    assert_eq!(
        argv,
        vec![
            "--src".to_owned(),
            slot.display().to_string(),
            "--canonical".to_owned(),
            canonical.display().to_string(),
            "--".to_owned(),
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            command.to_owned(),
        ],
        "the canonical path is its own argv element and the command is never re-split"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn the_canonical_path_is_never_pasted_into_the_command_string() {
    // A canonical directory with a space and a quote in it must not be able to reach the shell as
    // text, because it is passed as its own argv element and never concatenated.
    let t = Tmp::new("injection");
    let slot = t.dir("slot");
    let canonical = t.dir("canonical with ' quote");
    let helper = recording_helper(&t, 0, "");
    let c = Canonical {
        dir: canonical.clone(),
        helper: helper.clone(),
    };
    run_build(&slot, "exit 0", Some(&c)).expect("the stub exits 0");
    let argv = recorded(&helper);
    let at = argv
        .iter()
        .position(|a| a == "--canonical")
        .expect("--canonical");
    assert_eq!(argv[at + 1], canonical.display().to_string());
    // The helper records argv lines; a newline in the path would split into extra lines.
    assert_eq!(argv.len(), 8, "one argv element per line: {argv:?}");
}

/// The macOS control. `validate` refuses the platform last, so this still proves the ordering: a
/// fully valid canonical pair on a platform without namespaces is refused, not ignored.
#[cfg(not(target_os = "linux"))]
#[test]
fn off_linux_a_valid_canonical_pair_is_unsupported_and_never_ignored() {
    let t = Tmp::new("mac");
    let slot = t.dir("slot");
    let helper = recording_helper(&t, 0, "");
    let c = Canonical {
        dir: t.dir("canonical"),
        helper,
    };
    let err = run_build(&slot, "touch SHOULD_NOT_EXIST", Some(&c))
        .expect_err("macOS must refuse a canonical path");
    assert!(
        matches!(&err, cowfs_treehouse::Error::Unsupported(m) if m.contains("macOS")),
        "expected Unsupported naming macOS, got {err:?}"
    );
    assert!(
        !slot.join("SHOULD_NOT_EXIST").exists(),
        "the build must not run"
    );
}

/// The regression that mattered: the helper path used to be repeated as the first argument, so the
/// helper read its own path where `--src`'s value belonged and refused. `args` is pure, so this is
/// checked on every platform, namespace or not.
#[test]
fn the_helper_path_is_never_repeated_as_an_argument() {
    let t = Tmp::new("no-repeat");
    let slot = t.dir("slot");
    let helper = recording_helper(&t, 0, "");
    let c = Canonical {
        dir: t.dir("canonical"),
        helper: helper.clone(),
    };
    let argv = c.args(&slot, "exit 0");
    assert_eq!(
        argv.first().map(|a| a.to_string_lossy().into_owned()),
        Some("--src".to_owned()),
        "the arguments start at --src, not at the helper's own path"
    );
    assert!(
        !argv.iter().any(|a| a == helper.as_os_str()),
        "the program must not also appear as an argument: {argv:?}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_refused_namespace_is_unsupported_and_the_build_does_not_run() {
    // The stub refuses every time, which is what a host without unprivileged namespaces looks like.
    let t = Tmp::new("refused");
    let slot = t.dir("slot");
    let c = Canonical {
        dir: t.dir("canonical"),
        helper: recording_helper(
            &t,
            77,
            "echo 'cowfs-ns-run.sh: UNMEASURABLE: no private mount namespace' >&2\n",
        ),
    };
    let err = run_build(&slot, "touch SHOULD_NOT_EXIST", Some(&c))
        .expect_err("a refused namespace must fail");
    assert!(
        matches!(&err, cowfs_treehouse::Error::Unsupported(m) if m.contains("UNMEASURABLE")),
        "expected Unsupported carrying UNMEASURABLE, got {err:?}"
    );
    assert!(
        !slot.join("SHOULD_NOT_EXIST").exists(),
        "the build must not run"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_working_namespace_lets_a_failing_build_stay_a_failure() {
    // The 77 collision: the stub succeeds on the probe and then the build exits 77. That must stay
    // an Io failure, not be reported as an unavailable namespace.
    let t = Tmp::new("payload77");
    let slot = t.dir("slot");
    let probe_marker = t.0.join("probe-count");
    let helper = {
        let dir = t.dir("bin");
        let p = dir.join("ns-payload77");
        let body = format!(
            "#!/bin/sh\nn=$(cat \"{m}\" 2>/dev/null || echo 0)\necho $((n+1)) > \"{m}\"\n\
             printf '%s\\n' \"$@\" > \"{a}\"\n\
             if [ \"$n\" = 0 ]; then exit 0; fi\nexit 77\n",
            m = probe_marker.display(),
            a = dir.join("argv.txt").display(),
        );
        write_stub(&p, &body);
        p
    };
    let c = Canonical {
        dir: t.dir("canonical"),
        helper,
    };
    let err = run_build(&slot, "exit 0", Some(&c)).expect_err("payload 77 must fail");
    assert!(
        matches!(&err, cowfs_treehouse::Error::Io(m) if m.contains("failed")),
        "a payload exit of 77 must stay an Io failure, got {err:?}"
    );
}

#[test]
fn canonical_without_a_helper_is_a_usage_error() {
    let out = run(&[
        "base",
        "refresh",
        "--repo",
        "/tmp",
        "--build",
        "true",
        "--canonical",
        "/tmp",
    ]);
    assert_eq!(out.status.code(), Some(2), "a usage error is exit 2");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--ns-helper"), "{err}");
    assert!(err.contains("--canonical"), "{err}");
}

#[test]
fn a_helper_without_a_canonical_is_a_usage_error() {
    let out = run(&[
        "base",
        "refresh",
        "--repo",
        "/tmp",
        "--build",
        "true",
        "--ns-helper",
        "/tmp/whatever.sh",
    ]);
    assert_eq!(out.status.code(), Some(2), "a usage error is exit 2");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--canonical"), "{err}");
}

#[test]
fn neither_flag_keeps_the_pre_existing_command_line() {
    // Default compatibility: a plain `base refresh --build` parses exactly as before. The daemon
    // is not running here, so the exit code is 3, which proves the flags were not required and no
    // namespace was demanded.
    let out = run(&["base", "refresh", "--repo", "/tmp", "--build", "true"]);
    assert_eq!(
        out.status.code(),
        Some(3),
        "expected the daemon-not-running path, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
