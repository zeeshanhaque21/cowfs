//! Extended attributes.
//!
//! Every listing assertion here ignores `com.apple.*` names: macOS adds
//! `com.apple.provenance` to new files.
//!
//! Decisions pinned here: attributes belong to the inode (all hardlinked names share them);
//! an empty value is a real value; `create` fails with `Exists`, `replace` with `NoAttr`;
//! values up to 60,000 bytes work (larger ones are backend defined: `Range` or success);
//! `listxattr` returns each name once and the order is stable while the set is unchanged.

use cowfs_vfs::{Error, XattrFlags, NAME_MAX, ROOT_INO};

use super::{pattern, Ctx, Outcome};

/// macOS adds `com.apple.*` attributes (for example `com.apple.provenance`) to new files, so
/// every listing assertion ignores them.
fn without_apple(names: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
    names
        .into_iter()
        .filter(|n| !n.starts_with(b"com.apple."))
        .collect()
}

const NONE: XattrFlags = XattrFlags {
    create: false,
    replace: false,
};
const CREATE: XattrFlags = XattrFlags {
    create: true,
    replace: false,
};
const REPLACE: XattrFlags = XattrFlags {
    create: false,
    replace: true,
};

pub fn xattr_set_get_list_remove(c: &Ctx) -> Outcome {
    let f = c.file(ROOT_INO, "f")?;
    ensure!(
        without_apple(c.fs.listxattr(f)?).is_empty(),
        "a new file has attributes"
    );
    c.fs.setxattr(f, b"user.one", b"1", NONE)?;
    c.fs.setxattr(f, b"user.two", b"22", NONE)?;
    ensure_eq!(c.fs.getxattr(f, b"user.one")?, b"1".to_vec(), "get one");
    ensure_eq!(c.fs.getxattr(f, b"user.two")?, b"22".to_vec(), "get two");
    let mut names = without_apple(c.fs.listxattr(f)?);
    names.sort();
    ensure_eq!(
        names,
        vec![b"user.one".to_vec(), b"user.two".to_vec()],
        "list"
    );
    c.fs.setxattr(f, b"user.one", b"uno", NONE)?;
    ensure_eq!(c.fs.getxattr(f, b"user.one")?, b"uno".to_vec(), "overwrite");
    ensure_eq!(
        without_apple(c.fs.listxattr(f)?).len(),
        2,
        "overwrite must not add a name"
    );
    c.fs.removexattr(f, b"user.one")?;
    ensure_err!(
        c.fs.getxattr(f, b"user.one"),
        Error::NoAttr,
        "get after remove"
    );
    ensure_eq!(
        without_apple(c.fs.listxattr(f)?),
        vec![b"user.two".to_vec()],
        "list after remove"
    );
    Ok(())
}

pub fn xattr_create_and_replace_flags(c: &Ctx) -> Outcome {
    let f = c.file(ROOT_INO, "f")?;
    ensure_err!(
        c.fs.setxattr(f, b"user.a", b"v", REPLACE),
        Error::NoAttr,
        "replace of a missing attribute"
    );
    ensure_err!(
        c.fs.getxattr(f, b"user.a"),
        Error::NoAttr,
        "attribute created by a failed replace"
    );
    c.fs.setxattr(f, b"user.a", b"v1", CREATE)?;
    ensure_err!(
        c.fs.setxattr(f, b"user.a", b"v2", CREATE),
        Error::Exists,
        "create of an existing attribute"
    );
    ensure_eq!(
        c.fs.getxattr(f, b"user.a")?,
        b"v1".to_vec(),
        "value after a failed create"
    );
    c.fs.setxattr(f, b"user.a", b"v3", REPLACE)?;
    ensure_eq!(
        c.fs.getxattr(f, b"user.a")?,
        b"v3".to_vec(),
        "value after replace"
    );
    Ok(())
}

pub fn xattr_missing_is_no_attr(c: &Ctx) -> Outcome {
    let f = c.file(ROOT_INO, "f")?;
    ensure_err!(
        c.fs.getxattr(f, b"user.none"),
        Error::NoAttr,
        "get of a missing attribute"
    );
    ensure_err!(
        c.fs.removexattr(f, b"user.none"),
        Error::NoAttr,
        "remove of a missing attribute"
    );
    c.fs.setxattr(f, b"user.x", b"v", NONE)?;
    c.fs.removexattr(f, b"user.x")?;
    ensure_err!(
        c.fs.removexattr(f, b"user.x"),
        Error::NoAttr,
        "second remove"
    );
    Ok(())
}

pub fn xattr_empty_and_large_values(c: &Ctx) -> Outcome {
    let f = c.file(ROOT_INO, "f")?;
    c.fs.setxattr(f, b"user.empty", b"", NONE)?;
    ensure_eq!(
        c.fs.getxattr(f, b"user.empty")?,
        Vec::<u8>::new(),
        "empty value"
    );
    ensure_eq!(
        without_apple(c.fs.listxattr(f)?),
        vec![b"user.empty".to_vec()],
        "an empty value still lists"
    );
    for len in [1usize, 255, 4096, 60_000] {
        let v = pattern(len, len as u64);
        c.fs.setxattr(f, b"user.big", &v, NONE)?;
        ensure!(
            c.fs.getxattr(f, b"user.big")? == v,
            "value of {len} bytes differs"
        );
    }
    let bin = vec![0u8, 1, 0xff, 0, 0x80];
    c.fs.setxattr(f, b"user.bin", &bin, NONE)?;
    ensure_eq!(
        c.fs.getxattr(f, b"user.bin")?,
        bin,
        "binary value with NULs"
    );
    Ok(())
}

pub fn xattr_list_order_is_stable(c: &Ctx) -> Outcome {
    let f = c.file(ROOT_INO, "f")?;
    for i in [5, 3, 9, 1, 7, 2, 8, 4, 6, 0] {
        c.fs.setxattr(f, format!("user.n{i}").as_bytes(), b"v", NONE)?;
    }
    let first = without_apple(c.fs.listxattr(f)?);
    ensure_eq!(first.len(), 10, "attributes listed");
    for _ in 0..5 {
        ensure_eq!(
            without_apple(c.fs.listxattr(f)?),
            first,
            "list order changed between calls"
        );
    }
    c.fs.setxattr(f, b"user.n3", b"changed", NONE)?;
    ensure_eq!(
        without_apple(c.fs.listxattr(f)?),
        first,
        "list order changed after an overwrite"
    );
    Ok(())
}

pub fn xattr_on_directory_and_symlink(c: &Ctx) -> Outcome {
    let d = c.dir(ROOT_INO, "d")?;
    let s = c.symlink(ROOT_INO, b"s", b"d")?.ino;
    for (what, ino) in [("directory", d), ("root", ROOT_INO), ("symlink", s)] {
        c.fs.setxattr(ino, b"user.k", what.as_bytes(), NONE)?;
        ensure_eq!(
            c.fs.getxattr(ino, b"user.k")?,
            what.as_bytes().to_vec(),
            "{what} attribute"
        );
    }
    ensure_err!(
        c.fs.getxattr(d, b"user.other"),
        Error::NoAttr,
        "attribute leaked between inodes"
    );
    Ok(())
}

pub fn xattr_shared_by_hardlinks(c: &Ctx) -> Outcome {
    let f = c.file(ROOT_INO, "f")?;
    let g = c.link(f, ROOT_INO, b"g")?.ino;
    c.fs.setxattr(f, b"user.a", b"v", NONE)?;
    ensure_eq!(
        c.fs.getxattr(g, b"user.a")?,
        b"v".to_vec(),
        "attribute seen through the other name"
    );
    c.fs.unlink(ROOT_INO, b"f")?;
    ensure_eq!(
        c.fs.getxattr(c.lookup(ROOT_INO, b"g")?.ino, b"user.a")?,
        b"v".to_vec(),
        "attribute after unlinking one name"
    );
    let h = c.file(ROOT_INO, "h")?;
    ensure_err!(
        c.fs.getxattr(h, b"user.a"),
        Error::NoAttr,
        "a new file starts without attributes"
    );
    Ok(())
}

/// cowfs contract: an attribute name is 1 to `NAME_MAX` bytes without NUL. Linux and APFS
/// disagree on the errno for the rest (ERANGE, EINVAL), so `InvalidArgument`, `Range` and
/// `NameTooLong` are all accepted. Linux additionally requires a namespace prefix (`user.`,
/// `system.posix_acl_*`): a 255 byte name with no prefix is ENOTSUP there, and a `user.` plus
/// 250 byte name works. A backend that wants the name-prefixed cases set
/// `Options::xattr_names` (see the crate docs); the rejection cases are always checked.
pub fn xattr_name_validation(c: &Ctx) -> Outcome {
    let f = c.file(ROOT_INO, "f")?;
    ensure_err_any!(
        c.fs.setxattr(f, b"", b"v", NONE),
        [Error::InvalidArgument, Error::Range],
        "empty attribute name"
    );
    ensure_err_any!(
        c.fs.setxattr(f, b"user.a\0b", b"v", NONE),
        [Error::InvalidArgument, Error::Range],
        "attribute name with a NUL"
    );
    let long = vec![b'x'; NAME_MAX + 1];
    ensure_err_any!(
        c.fs.setxattr(f, &long, b"v", NONE),
        [Error::Range, Error::NameTooLong, Error::InvalidArgument],
        "attribute name of {} bytes",
        long.len()
    );
    ensure!(
        without_apple(c.fs.listxattr(f)?).is_empty(),
        "a rejected setxattr left an attribute behind"
    );
    let name: Vec<u8> = if c.xattr_names_prefixed {
        let mut n = b"user.".to_vec();
        n.extend(std::iter::repeat_n(b'y', NAME_MAX - 5));
        n
    } else {
        vec![b'y'; NAME_MAX]
    };
    ensure_eq!(name.len(), NAME_MAX, "test attribute name length");
    c.fs.setxattr(f, &name, b"v", NONE)?;
    ensure_eq!(
        c.fs.getxattr(f, &name)?,
        b"v".to_vec(),
        "attribute named {:?} ({NAME_MAX} bytes)",
        String::from_utf8_lossy(&name[..name.len().min(16)])
    );
    Ok(())
}
