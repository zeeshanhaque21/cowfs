use std::str::FromStr;
use std::time::Duration;

use crate::MountError;

/// How a cowfs `Vfs` is mounted through FUSE.
///
/// The defaults are the spike 3 findings: long kernel cache lifetimes for names and
/// attributes, negative lookup caching and `keep_cache`. The long lifetimes are only correct
/// while every change to the tree goes through this mount. A change made any other way (the
/// control API, a snapshot operation) must be announced with `Mount::invalidate_inode` and
/// `Mount::invalidate_entry`, or the kernel keeps serving the old answer until the timeout
/// expires. Measured on the Linux 7.0 kernel of the test VM, the kernel cannot be told to drop
/// a cached "no such name" answer, so those expire only by `negative_ttl`, which is short.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MountOptions {
    /// Name shown in `/proc/mounts` and `mount(8)`. Default `cowfs`.
    pub fs_name: String,
    /// How long the kernel may cache a successful name lookup. Default 3600 s.
    pub entry_ttl: Duration,
    /// How long the kernel may cache attributes. Default 3600 s.
    pub attr_ttl: Duration,
    /// How long the kernel may cache a "no such name" answer, zero to disable. Default 1 s.
    /// A name created behind the mount stays invisible for up to this long.
    pub negative_ttl: Duration,
    /// Keep cached file pages across `open` calls instead of dropping them on every open.
    /// Default true.
    pub keep_cache: bool,
    /// Mount read-only. The kernel then rejects every write with `EROFS`. Default false.
    pub read_only: bool,
    /// Let the kernel enforce mode bits against the uid and gid the `Vfs` reports.
    /// When false, the adapter answers `access` calls itself and no check happens on
    /// `open` or `lookup`. Default true.
    pub default_permissions: bool,
    /// Let users other than the mounter access the mount. Non-root mounters need
    /// `user_allow_other` in `/etc/fuse.conf`. Default false.
    pub allow_other: bool,
    /// Ask `fusermount3` to unmount if this process dies. Implies `allow_other`, so the same
    /// `/etc/fuse.conf` requirement applies to non-root mounters. Default false.
    pub auto_unmount: bool,
}

impl Default for MountOptions {
    fn default() -> Self {
        let hour = Duration::from_secs(3600);
        Self {
            fs_name: "cowfs".into(),
            entry_ttl: hour,
            attr_ttl: hour,
            negative_ttl: Duration::from_secs(1),
            keep_cache: true,
            read_only: false,
            default_permissions: true,
            allow_other: false,
            auto_unmount: false,
        }
    }
}

impl MountOptions {
    /// Applies one `key` or `key=value` token. Flags may be negated with a `no` prefix.
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
            "keep_cache" => flag(value).map(|_| self.keep_cache = true)?,
            "nokeep_cache" => flag(value).map(|_| self.keep_cache = false)?,
            "ro" => flag(value).map(|_| self.read_only = true)?,
            "rw" => flag(value).map(|_| self.read_only = false)?,
            "default_permissions" => flag(value).map(|_| self.default_permissions = true)?,
            "nodefault_permissions" => flag(value).map(|_| self.default_permissions = false)?,
            "allow_other" => flag(value).map(|_| self.allow_other = true)?,
            "auto_unmount" => flag(value).map(|_| self.auto_unmount = true)?,
            _ => return Err(bad()),
        }
        Ok(())
    }
}

/// Parses a comma separated option string such as `ttl=60,noneg,ro` on top of the defaults.
/// Keys: `fsname=<name>`, `ttl=<secs>` (both timeouts), `entry_ttl=<secs>`, `attr_ttl=<secs>`,
/// `neg_ttl=<secs>`, `noneg`, `keep_cache`/`nokeep_cache`, `ro`/`rw`, `default_permissions`/
/// `nodefault_permissions`, `allow_other`, `auto_unmount`. Later tokens win.
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
    fn defaults_follow_the_spike() {
        let o = MountOptions::default();
        assert_eq!(o.entry_ttl, Duration::from_secs(3600));
        assert_eq!(o.attr_ttl, Duration::from_secs(3600));
        assert_eq!(o.negative_ttl, Duration::from_secs(1));
        assert!(o.keep_cache && o.default_permissions);
        assert!(!o.read_only && !o.allow_other && !o.auto_unmount);
        assert_eq!(parse("").unwrap(), o);
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
    fn flags_toggle() {
        let o =
            parse("noneg,nokeep_cache,ro,nodefault_permissions,allow_other,auto_unmount").unwrap();
        assert!(o.negative_ttl.is_zero() && !o.keep_cache && o.read_only);
        assert!(!o.default_permissions && o.allow_other && o.auto_unmount);
        let o = parse("ro,rw,noneg,neg_ttl=5").unwrap();
        assert!(!o.read_only && o.negative_ttl == Duration::from_secs(5));
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
        ] {
            assert!(
                matches!(parse(bad), Err(MountError::InvalidOption(t)) if t == bad),
                "{bad}"
            );
        }
    }
}
