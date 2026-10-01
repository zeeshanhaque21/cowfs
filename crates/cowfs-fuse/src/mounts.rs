//! Parsing of `/proc/mounts` and `/etc/fuse.conf`. Pure text handling, no kernel access.

use std::path::{Path, PathBuf};

/// The filesystem type of cowfs mounts in `/proc/mounts` (`fuse.` plus the FUSE subtype).
pub(crate) const FSTYPE: &str = "fuse.cowfs";

/// One line of `/proc/mounts`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    pub mountpoint: PathBuf,
    pub fstype: String,
}

fn unescape(field: &str) -> String {
    let b = field.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let octal = b.get(i..i + 4).and_then(|w| match w {
            [b'\\', a @ b'0'..=b'3', b @ b'0'..=b'7', c @ b'0'..=b'7'] => {
                Some(((a - b'0') << 6) | ((b - b'0') << 3) | (c - b'0'))
            }
            _ => None,
        });
        match octal {
            Some(v) => {
                out.push(v);
                i += 4;
            }
            None => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parses `/proc/mounts` text. Spaces, tabs, newlines and backslashes in paths arrive as octal escapes.
pub(crate) fn parse(text: &str) -> Vec<Entry> {
    text.lines()
        .filter_map(|l| {
            let mut f = l.split(' ');
            let _device = f.next()?;
            let mountpoint = PathBuf::from(unescape(f.next()?));
            let fstype = f.next()?.to_owned();
            Some(Entry { mountpoint, fstype })
        })
        .collect()
}

/// Whether `path` is listed as a mount point.
pub(crate) fn is_mounted(text: &str, path: &Path) -> bool {
    parse(text).iter().any(|e| e.mountpoint == path)
}

/// Mount points of cowfs mounts at or below `prefix`.
pub(crate) fn cowfs_mounts(text: &str, prefix: &Path) -> Vec<PathBuf> {
    parse(text)
        .into_iter()
        .filter(|e| e.fstype == FSTYPE && e.mountpoint.starts_with(prefix))
        .map(|e| e.mountpoint)
        .collect()
}

/// Whether `/etc/fuse.conf` text enables `user_allow_other`.
pub(crate) fn fuse_conf_allows_other(text: &str) -> bool {
    text.lines().any(|l| l.trim() == "user_allow_other")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "proc /proc proc rw 0 0\n\
        cowfs /tmp/a\\040b/mnt fuse.cowfs rw,nosuid 0 0\n\
        cowfs /srv/snap fuse.cowfs rw 0 0\n\
        other /tmp/a fuse.other rw 0 0\n\
        garbage\n";

    #[test]
    fn parses_and_unescapes() {
        let e = parse(SAMPLE);
        assert_eq!(e.len(), 4);
        assert_eq!(e[1].mountpoint, Path::new("/tmp/a b/mnt"));
        assert_eq!(e[1].fstype, "fuse.cowfs");
        assert_eq!(unescape("a\\134b\\011c\\12x"), "a\\b\tc\\12x");
    }

    #[test]
    fn finds_cowfs_mounts_under_a_prefix() {
        assert_eq!(
            cowfs_mounts(SAMPLE, Path::new("/")),
            [PathBuf::from("/tmp/a b/mnt"), PathBuf::from("/srv/snap")]
        );
        assert_eq!(
            cowfs_mounts(SAMPLE, Path::new("/srv")),
            [PathBuf::from("/srv/snap")]
        );
        assert!(
            cowfs_mounts(SAMPLE, Path::new("/tmp/a")).is_empty(),
            "prefix is per component"
        );
        assert!(cowfs_mounts(SAMPLE, Path::new("/nowhere")).is_empty());
    }

    #[test]
    fn mounted_check_is_exact() {
        assert!(is_mounted(SAMPLE, Path::new("/srv/snap")));
        assert!(!is_mounted(SAMPLE, Path::new("/srv")));
    }

    #[test]
    fn fuse_conf() {
        assert!(fuse_conf_allows_other("# c\nuser_allow_other\n"));
        assert!(!fuse_conf_allows_other(
            "#user_allow_other\nmount_max = 5\n"
        ));
    }
}
