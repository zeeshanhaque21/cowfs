// Why does execve of a file this process just wrote return ETXTBSY?
//
// Five theories were measured and rejected before this spike, so this measures the boundary instead
// of guessing again. What the earlier probe did and this one does differently:
//
//   * It labelled its own modes wrong. The old "no write in the loop" mode created a fresh directory
//     every round and wrote before every exec, so it never tested what it claimed. Here `execonly`
//     writes the file once per thread and then execs it with no write at all, which is the control
//     that was believed to have run.
//   * It asked who holds the file open by looking at the file under test. That question has the
//     answer "nobody" by construction, because the writing thread closed the handle before exec. So
//     this also records every write-mode descriptor open anywhere on the box at the moment of the
//     failure, capped and counted, because execve also loads the interpreter and the loader's
//     libraries and ETXTBSY is raised against whatever inode is already open for writing.
//   * It never compared spawn mechanisms. `posix` uses the same glibc path Rust's Command takes;
//     `forkexec` forces the fork and exec path with a pre_exec hook. Everything else is held fixed.
//
// usage: etxtbsy-spike THREADS ROUNDS MODE MAX_SAMPLES [BASE_DIR] [posix|forkexec]
//
// Modes: execonly (write once per thread, exec every round), inplace, pub, script, elf, static
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

const BODY: &[u8] = b"#!/bin/sh\nexit 0\n";
const STATIC_BIN: &str = "/home/moonscape/cowfs-ns17/static-true";

fn write_exec(p: &Path, body: &[u8]) {
    {
        let mut f = std::fs::File::create(p).expect("create");
        f.write_all(body).expect("write");
        f.flush().expect("flush");
    }
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

fn write_exec_pub(p: &Path, body: &[u8]) {
    let tmp = p.with_extension(format!("tmp{}", std::process::id()));
    write_exec(&tmp, body);
    std::fs::rename(&tmp, p).expect("rename into place");
}

fn place(mode: &str, stub: &Path) {
    match mode {
        "execonly" | "script" => {
            if !stub.exists() {
                write_exec(stub, BODY);
            }
        }
        "inplace" => write_exec(stub, BODY),
        "pub" => write_exec_pub(stub, BODY),
        "elf" => {
            std::fs::copy("/bin/true", stub).expect("copy a dynamic binary");
            std::fs::set_permissions(stub, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        "static" => {
            let bin = std::env::var("ETXTBSY_STATIC_BIN").unwrap_or_else(|_| STATIC_BIN.into());
            std::fs::copy(bin, stub).expect("copy the static binary");
            std::fs::set_permissions(stub, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        other => panic!("unknown mode {other}"),
    }
}

/// The filesystem the path is on, from mountinfo, matched against the nearest existing ancestor
/// because a directory created inside a mount has no line of its own.
fn fstype_for(path: &Path) -> String {
    let mut probe = path.to_path_buf();
    while !probe.exists() {
        if !probe.pop() {
            return "no-existing-ancestor".into();
        }
    }
    let target = probe.to_string_lossy().into_owned();
    let mut best: Option<(usize, String)> = None;
    let Ok(mounts) = std::fs::read_to_string("/proc/self/mountinfo") else {
        return "unknown".into();
    };
    for line in mounts.lines() {
        let Some((left, right)) = line.split_once(" - ") else {
            continue;
        };
        let mut fields = left.split_whitespace();
        let _id = fields.next();
        let Some(mount_point) = fields.next() else { continue };
        let fs = right.split_whitespace().next().unwrap_or("?");
        let mp = mount_point.to_string();
        if target == mp || target.starts_with(&format!("{mp}/")) {
            if best.as_ref().is_none_or(|(len, _)| mp.len() > *len) {
                best = Some((mp.len(), fs.to_string()));
            }
        }
    }
    best.map_or_else(|| "not-found".into(), |(_, fs)| fs)
}

/// Write-mode descriptors open anywhere on the box, capped so one sample cannot bury the report.
fn open_writers(cap: usize) -> (usize, Vec<String>) {
    let mut found = Vec::new();
    let total = std::cell::Cell::new(0usize);
    let Ok(procs) = std::fs::read_dir("/proc") else {
        return (0, found);
    };
    for pid in procs.flatten() {
        let name = pid.file_name().to_string_lossy().into_owned();
        if !name.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let Ok(fds) = std::fs::read_dir(pid.path().join("fd")) else { continue };
        for fd in fds.flatten() {
            let info = std::fs::read_to_string(pid.path().join("fdinfo").join(fd.file_name()))
                .unwrap_or_default();
            let Ok(flags) = info
                .lines()
                .find_map(|l| l.strip_prefix("flags:"))
                .unwrap_or("")
                .trim()
                .parse::<u32>()
            else {
                continue;
            };
            if flags & 0o3 == 0 {
                continue;
            }
            let Ok(target) = std::fs::read_link(fd.path()) else { continue };
            let kind = if flags & 0o3 == 1 { "WRITE" } else { "RDWR" };
            total.set(total.get() + 1);
            if found.len() < cap {
                let comm = std::fs::read_to_string(pid.path().join("comm"))
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                found.push(format!("{kind} pid={name} comm={comm} {target:?}"));
            }
        }
    }
    (total.get(), found)
}

/// Anyone holding this exact file open, which is the question the write-handle theory asks.
fn holders_of(path: &Path) -> Vec<String> {
    let want = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut found = Vec::new();
    let Ok(procs) = std::fs::read_dir("/proc") else {
        return found;
    };
    for pid in procs.flatten() {
        let name = pid.file_name().to_string_lossy().into_owned();
        if !name.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let Ok(fds) = std::fs::read_dir(pid.path().join("fd")) else { continue };
        for fd in fds.flatten() {
            let Ok(target) = std::fs::read_link(fd.path()) else { continue };
            if std::fs::canonicalize(&target).unwrap_or(target.clone()) != want {
                continue;
            }
            let comm = std::fs::read_to_string(pid.path().join("comm"))
                .unwrap_or_default()
                .trim()
                .to_string();
            found.push(format!("pid={name} comm={comm} fd={}", fd.file_name().to_string_lossy()));
        }
    }
    found
}

fn main() {
    let arg = |n: usize, d: usize| {
        std::env::args()
            .nth(n)
            .and_then(|s| s.parse().ok())
            .unwrap_or(d)
    };
    let threads = arg(1, 8);
    let rounds = arg(2, 60);
    let mode = std::env::args().nth(3).unwrap_or_else(|| "execonly".into());
    let max_samples = arg(4, 3);
    let root_dir = std::env::args()
        .nth(5)
        .map_or_else(std::env::temp_dir, PathBuf::from);
    let spawn = std::env::args().nth(6).unwrap_or_else(|| "posix".into());

    let base = root_dir.join(format!("etxtbsy-spike-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&base);
    let fs = fstype_for(&base);
    let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .unwrap_or_default()
        .trim()
        .to_string();

    let ok = Arc::new(AtomicUsize::new(0));
    let busy = Arc::new(AtomicUsize::new(0));
    let other = Arc::new(AtomicUsize::new(0));
    let other_kinds = Arc::new(Mutex::new(std::collections::BTreeMap::<String, usize>::new()));
    let samples = Arc::new(Mutex::new(Vec::<String>::new()));

    let shared = (base.clone(), mode.clone(), fs.clone(), kernel.clone(), spawn.clone());
    let mut handles = Vec::new();
    for t in 0..threads {
        let (ok, busy, other, other_kinds, samples) =
            (ok.clone(), busy.clone(), other.clone(), other_kinds.clone(), samples.clone());
        let (base, mode, fs, kernel, spawn) = shared.clone();
        handles.push(std::thread::spawn(move || {
            // `execonly` gets one directory per thread, written once, so the loop below execs the
            // same untouched file for every round.
            let thread_dir = base.join(format!("t{t}"));
            let _ = std::fs::create_dir_all(&thread_dir);
            let stub = thread_dir.join("s");
            if mode == "execonly" {
                write_exec(&stub, BODY);
            }
            for r in 0..rounds {
                let dir = if mode == "execonly" {
                    thread_dir.clone()
                } else {
                    let d = base.join(format!("t{t}-r{r}"));
                    let _ = std::fs::create_dir_all(&d);
                    d
                };
                let target = if mode == "execonly" {
                    stub.clone()
                } else {
                    let s = dir.join("s");
                    place(&mode, &s);
                    s
                };

                let mut cmd = std::process::Command::new(&target);
                if spawn == "forkexec" {
                    // A pre_exec hook is enough to stop Rust choosing posix_spawn, so this is the
                    // fork and exec path with everything else identical.
                    unsafe {
                        cmd.pre_exec(|| Ok(()));
                    }
                }
                match cmd.status() {
                    Ok(s) if s.success() => {
                        ok.fetch_add(1, Ordering::SeqCst);
                    }
                    Ok(s) => {
                        other.fetch_add(1, Ordering::SeqCst);
                        *other_kinds
                            .lock()
                            .unwrap()
                            .entry(format!("nonzero status {s}"))
                            .or_default() += 1;
                    }
                    Err(e) => {
                        let errno = e.raw_os_error().unwrap_or(0);
                        if errno == 26 {
                            busy.fetch_add(1, Ordering::SeqCst);
                            let mut log = samples.lock().unwrap();
                            if log.len() < max_samples {
                                let (n, sample) = open_writers(8);
                                log.push(format!(
                                    "ETXTBSY errno={errno} path={} fs={fs} kernel={kernel} \
                                     spawn={spawn} writes_in_the_loop={} holders_of_target={:?} \
                                     box_writers={n} first_few={sample:?} e={e}",
                                    target.display(),
                                    mode != "execonly",
                                    holders_of(&target),
                                ));
                            }
                        } else {
                            other.fetch_add(1, Ordering::SeqCst);
                            *other_kinds
                                .lock()
                                .unwrap()
                                .entry(format!("errno {errno}"))
                                .or_default() += 1;
                        }
                    }
                }
                if mode != "execonly" {
                    let _ = std::fs::remove_dir_all(&dir);
                }
            }
            if mode == "execonly" {
                let _ = std::fs::remove_dir_all(&thread_dir);
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }

    println!(
        "mode={mode} spawn={spawn} threads={threads} rounds={rounds} execs={} ok={} etxtbsy={} \
         other={} fs={fs} kernel={kernel}",
        ok.load(Ordering::SeqCst) + busy.load(Ordering::SeqCst) + other.load(Ordering::SeqCst),
        ok.load(Ordering::SeqCst),
        busy.load(Ordering::SeqCst),
        other.load(Ordering::SeqCst),
    );
    let kinds = other_kinds.lock().unwrap();
    if !kinds.is_empty() {
        println!("other failures: {kinds:?}");
    }
    for line in samples.lock().unwrap().iter() {
        println!("sample: {line}");
    }
    if busy.load(Ordering::SeqCst) == 0 {
        println!("no ETXTBSY in this configuration, so it does not reproduce here");
    }
    let _ = std::fs::remove_dir_all(&base);
}