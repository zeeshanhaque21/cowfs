mod common;

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use common::{mkfile, pattern, read_all, test_opts};
use cowfs_core::Core;
use cowfs_vfs::{Vfs, ROOT_INO};

fn verify_pid(pid: u32) {
    #[cfg(target_os = "linux")]
    {
        let cmd = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap();
        assert!(String::from_utf8_lossy(&cmd).contains("flush_boundary"));
    }
    #[cfg(not(target_os = "linux"))]
    assert!(Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "pid=,args="])
        .status()
        .unwrap()
        .success());
}

#[test]
fn boundary_child() {
    let Ok(dir) = std::env::var("COWFS_BOUNDARY_DIR") else {
        return;
    };
    let after = std::env::var("COWFS_BOUNDARY_AFTER").unwrap() == "1";
    let armed = Arc::new(AtomicBool::new(false));
    let flag = armed.clone();
    let c = Core::open_with_meta(&dir, test_opts(), move |dir, mut opts| {
        let sync = opts.before_sync.take().unwrap();
        opts.before_sync = Some(Arc::new(move || {
            let stop = flag.load(Ordering::Acquire);
            if !stop || after {
                sync()?;
            }
            if stop {
                println!("BOUNDARY after={after}");
                std::io::stdout().flush().unwrap();
                verify_pid(std::process::id());
                let status = Command::new("kill")
                    .args(["-KILL", &std::process::id().to_string()])
                    .status()
                    .unwrap();
                panic!("SIGKILL failed: {status}");
            }
            Ok(())
        }));
        cowfs_meta::Meta::open(dir.join("meta.redb"), opts)
    })
    .unwrap();
    c.create_snapshot("s").unwrap();
    let fs = c.snapshot_view("s").unwrap();
    let safe = mkfile(&fs, ROOT_INO, "safe", &pattern(300_000, 1));
    fs.fsync(safe.ino, false).unwrap();
    let changed = mkfile(&fs, ROOT_INO, "changed", &pattern(700_000, 2));
    fs.fsync(changed.ino, false).unwrap();
    println!("ACKED");
    std::io::stdout().flush().unwrap();
    fs.write(changed.ino, 0, &pattern(700_000, 3)).unwrap();
    mkfile(&fs, ROOT_INO, "pending", &pattern(400_000, 4));
    armed.store(true, Ordering::Release);
    c.sync().unwrap();
    panic!("boundary was not reached");
}

#[test]
fn sigkill_at_store_sync_boundaries_preserves_fsynced_data_and_whole_commits() {
    use std::os::unix::process::ExitStatusExt;
    for after in ["0", "1"] {
        let dir = tempfile::tempdir().unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["boundary_child", "--exact", "--nocapture"])
            .env("COWFS_BOUNDARY_DIR", dir.path())
            .env("COWFS_BOUNDARY_AFTER", after)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let start = std::time::Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if start.elapsed() > std::time::Duration::from_secs(120) {
                verify_pid(child.id());
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("store boundary child timed out");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        let mut log = String::new();
        child
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut log)
            .unwrap();
        assert_eq!(status.signal(), Some(9), "{log} {status:?}");
        assert!(log.contains("ACKED") && log.contains("BOUNDARY"), "{log}");
        let c = Core::open(dir.path(), test_opts()).unwrap();
        c.check().unwrap();
        assert!(c.fsck().unwrap().is_clean());
        let fs = c.snapshot_view("s").unwrap();
        let safe = fs.lookup(ROOT_INO, b"safe").unwrap();
        assert_eq!(read_all(&fs, safe.ino), pattern(300_000, 1));
        let changed = fs.lookup(ROOT_INO, b"changed").unwrap();
        let bytes = read_all(&fs, changed.ino);
        let new = bytes == pattern(700_000, 3);
        assert!(new || bytes == pattern(700_000, 2), "half file commit");
        match fs.lookup(ROOT_INO, b"pending") {
            Ok(a) => {
                assert!(new, "half namespace commit");
                assert_eq!(read_all(&fs, a.ino), pattern(400_000, 4));
            }
            Err(cowfs_vfs::Error::NotFound) => assert!(!new, "half namespace commit"),
            Err(e) => panic!("pending: {e}"),
        }
        println!("store sync boundary after={after}: SIGKILL, fsck clean, durable bytes intact");
    }
}
