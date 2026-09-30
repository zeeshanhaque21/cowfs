use std::str::FromStr;
use std::time::Duration;

use crate::MountError;

/// Who may change the tree, which decides how long the kernel may trust its caches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MountMode {
    /// Anything may change the tree behind the mount. Cache lifetimes are the short
    /// `entry_ttl` and `attr_ttl`, file pages are dropped on every open, and appends are
    /// placed at the size the `Vfs` reports, not the size the kernel has cached.
    Shared,
    /// Every mutation goes through this mount, or is announced through the `Invalidator`.
    /// Names and attributes are cached for `ttl` and file pages are kept across opens.
    /// A change made behind the mount and not announced is served stale for up to `ttl`,
    /// and can corrupt data (an append lands at a stale size).
    SoleWriter {
        /// Cache lifetime for names and attributes.
        ttl: Duration,
    },
}

/// How a cowfs `Vfs` is mounted through FUSE.
///
/// The defaults are safe when the tree changes behind the mount (snapshot, gc and control
/// operations): everything is cached for one second, nothing longer. Opt into
/// [`MountMode::SoleWriter`] for the long lifetimes the spike 3 numbers relied on, and only
/// when every mutation is announced through the `Invalidator`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MountOptions {
    /// Name shown in `/proc/mounts` and `mount(8)`. Default `cowfs`.
    pub fs_name: String,
    /// Cache regime. Default [`MountMode::Shared`].
    pub mode: MountMode,
    /// Kernel cache lifetime of a successful name lookup in `Shared` mode. Default 1 s.
    pub entry_ttl: Duration,
    /// Kernel cache lifetime of attributes in `Shared` mode. Default 1 s.
    pub attr_ttl: Duration,
    /// Kernel cache lifetime of a "no such name" answer, zero to disable, in every mode.
    /// Default 1 s. The kernel ignores `invalidate_entry` for negative entries (verified on
    /// Linux 7.0), so a name created behind the mount, such as a new snapshot directory at
    /// the mount root, becomes visible only after this long.
    pub negative_ttl: Duration,
    /// Mount read-only. The kernel then rejects every write with `EROFS`. Default false.
    pub read_only: bool,
    /// Let the kernel enforce mode bits. When false, only `access(2)` is checked by the
    /// adapter and `open` follows no mode bits. Default true.
    pub default_permissions: bool,
    /// Let users other than the mounter access the mount. Non-root mounters need
    /// `user_allow_other` in `/etc/fuse.conf`. Default false.
    pub allow_other: bool,
    /// Ask `fusermount3` to unmount if this process dies. Implies `allow_other`, so non-root
    /// mounters need `user_allow_other` in `/etc/fuse.conf`; the mount fails fast with an
    /// explanation otherwise. Default false.
    pub auto_unmount: bool,
    /// Worker threads (lanes) for slow operations, each a FIFO. Requests for one inode always
    /// use one lane, so they stay ordered. 0 runs everything on the request loop thread.
    /// Default `min(8, cpus)`.
    pub workers: usize,
    /// `Vfs` panics tolerated before the mount is marked failed and answers `ENOTCONN`.
    /// Default 3.
    pub max_panics: u32,
    /// How long `unmount` waits for a busy mount (open files, working directories) before
    /// falling back to a lazy unmount. Default 5 s.
    pub unmount_timeout: Duration,
    /// Detect a `Vfs` that reuses an inode number while the kernel still holds a reference
    /// to the old file, and fail those requests with `EIO`. Default false.
    pub paranoid_ino: bool,
}

impl Default for MountOptions {
    fn default() -> Self {
        Self {
            fs_name: "cowfs".into(),
            mode: MountMode::Shared,
            entry_ttl: Duration::from_secs(1),
            attr_ttl: Duration::from_secs(1),
            negative_ttl: Duration::from_secs(1),
            read_only: false,
            default_permissions: true,
            allow_other: false,
            auto_unmount: false,
            workers: std::thread::available_parallelism().map_or(4, |n| n.get().min(8)),
            max_panics: 3,
            unmount_timeout: Duration::from_secs(5),
            paranoid_ino: false,
        }
    }
}

impl MountOptions {
    /// The `MountMode::SoleWriter` regime with the given cache lifetime.
    pub fn sole_writer(ttl: Duration) -> Self {
        Self {
            mode: MountMode::SoleWriter { ttl },
            ..Self::default()
        }
    }

    /// Cache lifetime the kernel gets for a successful lookup.
    pub fn entry_lifetime(&self) -> Duration {
        match self.mode {
            MountMode::Shared => self.entry_ttl,
            MountMode::SoleWriter { ttl } => ttl,
        }
    }

    /// Cache lifetime the kernel gets for attributes.
    pub fn attr_lifetime(&self) -> Duration {
        match self.mode {
            MountMode::Shared => self.attr_ttl,
            MountMode::SoleWriter { ttl } => ttl,
        }
    }

    /// Whether file pages stay cached across opens.
    pub fn keep_cache(&self) -> bool {
        matches!(self.mode, MountMode::SoleWriter { .. })
    }

    /// Applies one `key` or `key=value` token.
    fn apply(&mut self, token: &str) -> Result<(), MountError> {
        let bad = || MountError::InvalidOption(token.to_owned());
        let (key, value) = match token.split_once('=') {
            Some((k, v)) => (k, Some(v)),
            None => (token, None),
        };
        let secs = |v: Option<&str>| -> Result<Duration, MountError> {
            let f: f64 = v.and_then(|s| s.parse().ok()).ok_or_else(bad)?;
            Duration::try_from_secs_f64(f).map_err(|_| bad())
        };
        let count = |v: Option<&str>| -> Result<u32, MountError> {
            v.and_then(|s| s.parse().ok()).ok_or_else(bad)
        };
        let flag = |v: Option<&str>| v.is_none().then_some(()).ok_or_else(bad);
        match key {
            "fsname" => self.fs_name = value.filter(|v| !v.is_empty()).ok_or_else(bad)?.into(),
            "ttl" => {
                let d = secs(value)?;
                self.entry_ttl = d;
                self.attr_ttl = d;
            }
            "entry_ttl" => self.entry_ttl = secs(value)?,
            "attr_ttl" => self.attr_ttl = secs(value)?,
            "neg_ttl" => self.negative_ttl = secs(value)?,
            "noneg" => flag(value).map(|_| self.negative_ttl = Duration::ZERO)?,
            "shared" => flag(value).map(|_| self.mode = MountMode::Shared)?,
            "sole_writer" => {
                let ttl = match value {
                    None => Duration::from_secs(3600),
                    v => secs(v)?,
                };
                self.mode = MountMode::SoleWriter { ttl };
            }
            "ro" => flag(value).map(|_| self.read_only = true)?,
            "rw" => flag(value).map(|_| self.read_only = false)?,
            "default_permissions" => flag(value).map(|_| self.default_permissions = true)?,
            "nodefault_permissions" => flag(value).map(|_| self.default_permissions = false)?,
            "allow_other" => flag(value).map(|_| self.allow_other = true)?,
            "auto_unmount" => flag(value).map(|_| self.auto_unmount = true)?,
            "workers" => self.workers = usize::try_from(count(value)?).map_err(|_| bad())?,
            "max_panics" => self.max_panics = count(value)?.max(1),
            "unmount_timeout" => self.unmount_timeout = secs(value)?,
            "paranoid_ino" => flag(value).map(|_| self.paranoid_ino = true)?,
            _ => return Err(bad()),
        }
        Ok(())
    }
}

/// Parses a comma separated option string such as `ttl=60,noneg,ro` on top of the defaults.
/// Keys: `fsname=<name>`, `ttl=<secs>` (both `Shared` lifetimes), `entry_ttl=<secs>`,
/// `attr_ttl=<secs>`, `neg_ttl=<secs>`, `noneg`, `shared`, `sole_writer[=<secs>]` (default
/// 3600), `ro`/`rw`, `default_permissions`/`nodefault_permissions`, `allow_other`,
/// `auto_unmount`, `workers=<n>`, `max_panics=<n>`, `unmount_timeout=<secs>`, `paranoid_ino`.
/// Later tokens win.
impl FromStr for MountOptions {
    type Err = MountError;

    fn from_str(s: &str) -> Result<Self, MountError> {
        let mut opts = Self::default();
        for token in s.split(',').filter(|t| !t.is_empty()) {
            opts.apply(token)?;
        }
        Ok(opts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Result<MountOptions, MountError> {
        s.parse()
    }

    #[test]
    fn defaults_are_short_lived_and_shared() {
        let o = MountOptions::default();
        let one = Duration::from_secs(1);
        assert_eq!(
            (o.entry_lifetime(), o.attr_lifetime(), o.negative_ttl),
            (one, one, one)
        );
        assert_eq!(o.mode, MountMode::Shared);
        assert!(!o.keep_cache() && o.default_permissions);
        assert!(!o.read_only && !o.allow_other && !o.auto_unmount && !o.paranoid_ino);
        assert_eq!(
            (o.max_panics, o.unmount_timeout),
            (3, Duration::from_secs(5))
        );
        assert!((1..=8).contains(&o.workers));
        assert_eq!(parse("").unwrap(), o);
    }

    #[test]
    fn sole_writer_opts_into_long_lifetimes_and_keep_cache() {
        let o = parse("sole_writer").unwrap();
        assert_eq!(o.entry_lifetime(), Duration::from_secs(3600));
        assert_eq!(o.attr_lifetime(), Duration::from_secs(3600));
        assert!(o.keep_cache());
        assert_eq!(
            o.negative_ttl,
            Duration::from_secs(1),
            "negative stays short"
        );
        let o = parse("sole_writer=30,shared").unwrap();
        assert_eq!(o.mode, MountMode::Shared);
        assert_eq!(
            MountOptions::sole_writer(Duration::from_secs(9)).attr_lifetime(),
            Duration::from_secs(9)
        );
    }

    #[test]
    fn ttl_sets_both_and_specific_keys_override_in_order() {
        let o = parse("ttl=10,attr_ttl=2.5").unwrap();
        assert_eq!(o.entry_ttl, Duration::from_secs(10));
        assert_eq!(o.attr_ttl, Duration::from_millis(2500));
        let o = parse("attr_ttl=1,ttl=7").unwrap();
        assert_eq!(
            (o.entry_ttl, o.attr_ttl),
            (Duration::from_secs(7), Duration::from_secs(7))
        );
    }

    #[test]
    fn flags_and_numbers() {
        let o =
            parse("noneg,ro,nodefault_permissions,allow_other,auto_unmount,paranoid_ino").unwrap();
        assert!(o.negative_ttl.is_zero() && o.read_only && !o.default_permissions);
        assert!(o.allow_other && o.auto_unmount && o.paranoid_ino);
        let o = parse("ro,rw,noneg,neg_ttl=5,workers=0,max_panics=0,unmount_timeout=0.5").unwrap();
        assert!(!o.read_only && o.negative_ttl == Duration::from_secs(5));
        assert_eq!((o.workers, o.max_panics), (0, 1));
        assert_eq!(o.unmount_timeout, Duration::from_millis(500));
    }

    #[test]
    fn fsname() {
        assert_eq!(parse("fsname=snap1").unwrap().fs_name, "snap1");
        assert!(parse("fsname=").is_err());
    }

    #[test]
    fn rejects_bad_tokens() {
        for bad in [
            "bogus",
            "ttl",
            "ttl=abc",
            "ttl=-1",
            "ttl=nan",
            "ro=1",
            "neg=yes",
            "neg_ttl",
            "noneg=1",
            "allow_other=1",
            "workers=-1",
            "workers",
            "sole_writer=x",
            "keep_cache",
        ] {
            assert!(
                matches!(parse(bad), Err(MountError::InvalidOption(t)) if t == bad),
                "{bad}"
            );
        }
    }
}
