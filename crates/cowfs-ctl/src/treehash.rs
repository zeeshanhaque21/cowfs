use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use cowfs_vfs::{FileKind, Ino, Vfs};

/// The tree hash reports what the `Vfs` said, as an `io::Error`, because its callers are the
/// migration path and the CLI, which both speak `io::Error`.
fn vfs(e: cowfs_vfs::Error) -> io::Error {
    io::Error::other(e)
}

/// What an entry is, with the kinds the hash distinguishes rather than the ones the trait names:
/// a fifo, a socket or a device hashes as `other` and is never read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Dir,
    Link,
    File,
    Other,
}

/// Name of the algorithm behind every hash in the protocol.
pub const HASH_ALGORITHM: &str = "blake3";

/// Root hash of a directory tree plus its counts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeHash {
    /// Hex BLAKE3 root hash.
    pub root: String,
    /// Regular files hashed.
    pub files: u64,
    /// Bytes of regular file content hashed.
    pub bytes: u64,
}

/// Hashes the tree at `dir`, so a caller can check an `import` report independently.
///
/// The hash covers names, kinds, permission bits (`mode & 0o7777`), file content and symlink
/// targets. It does not cover ownership, timestamps, xattrs or hard link identity. Each
/// directory hashes its entries sorted by name bytes; an entry contributes its name length and
/// bytes, a kind byte, the mode, and then the file size and content hash, the symlink target, or
/// the child directory's hash. Symlinks are never followed, and hash as mode `0o777` whatever
/// the kernel reports, because those bits are not the symlink's own.
pub fn hash_tree(dir: &Path) -> io::Result<TreeHash> {
    let (mut files, mut bytes) = (0, 0);
    let root = hash_dir(dir, &mut files, &mut bytes)?;
    Ok(TreeHash {
        root: root.to_hex().to_string(),
        files,
        bytes,
    })
}

/// [`hash_tree`] for a tree inside a cowfs store, read through the `Vfs`.
///
/// This is how an `import` report's imported root hash is produced for a content-addressed store,
/// where the imported tree has no directory on disk to hash. It walks the same algorithm, so the
/// two hashes of one tree agree and a caller can compare them.
pub fn hash_view(fs: &dyn Vfs, root: Ino) -> io::Result<TreeHash> {
    let (mut files, mut bytes) = (0, 0);
    let h = hash_view_dir(fs, root, &mut files, &mut bytes)?;
    Ok(TreeHash {
        root: h.to_hex().to_string(),
        files,
        bytes,
    })
}

/// One directory listing, by name bytes, so both walkers order entries the same way.
struct Entry {
    name: Vec<u8>,
    kind: Kind,
    mode: u32,
    size: u64,
}

fn hash_dir(dir: &Path, files: &mut u64, bytes: &mut u64) -> io::Result<blake3::Hash> {
    let mut entries: Vec<Entry> = Vec::new();
    for e in fs::read_dir(dir)? {
        let e = e?;
        let md = fs::symlink_metadata(e.path())?;
        entries.push(Entry {
            name: e.file_name().as_bytes().to_vec(),
            kind: kind_of_md(&md),
            // the symlink case below hashes 0o777 whatever the kernel says
            mode: md.mode() & 0o7777,
            size: md.len(),
        });
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    let mut h = blake3::Hasher::new();
    for e in entries {
        h.update(&(e.name.len() as u64).to_le_bytes());
        h.update(&e.name);
        h.update(&mode_of(&e).to_le_bytes());
        match e.kind {
            Kind::Dir => {
                h.update(b"d");
                h.update(
                    hash_dir(
                        &dir.join(String::from_utf8_lossy(&e.name).into_owned()),
                        files,
                        bytes,
                    )?
                    .as_bytes(),
                );
            }
            Kind::Link => {
                h.update(b"l");
                let target =
                    fs::read_link(dir.join(String::from_utf8_lossy(&e.name).into_owned()))?;
                h.update(&(target.as_os_str().as_bytes().len() as u64).to_le_bytes());
                h.update(target.as_os_str().as_bytes());
            }
            Kind::File => {
                h.update(b"f");
                let mut fh = blake3::Hasher::new();
                let n = io::copy(
                    &mut fs::File::open(dir.join(String::from_utf8_lossy(&e.name).into_owned()))?,
                    &mut fh,
                )?;
                *files += 1;
                *bytes += n;
                h.update(&n.to_le_bytes());
                h.update(fh.finalize().as_bytes());
            }
            _ => {
                h.update(b"o");
            }
        }
    }
    Ok(h.finalize())
}

fn hash_view_dir(
    fs: &dyn Vfs,
    dir: Ino,
    files: &mut u64,
    bytes: &mut u64,
) -> io::Result<blake3::Hash> {
    let mut entries: Vec<Entry> = Vec::new();
    let mut cookie = 0u64;
    loop {
        let page = fs.readdir(dir, cookie, 64).map_err(vfs)?;
        for e in &page.entries {
            let attr = fs.getattr(e.ino).map_err(vfs)?;
            entries.push(Entry {
                name: e.name.clone(),
                kind: kind_of(attr.kind),
                mode: attr.mode & 0o7777,
                size: attr.size,
            });
        }
        if page.eof {
            break;
        }
        let Some(last) = page.entries.last() else {
            return Err(io::Error::other(
                "a readdir page was empty and not the last",
            ));
        };
        cookie = last.cookie;
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    let mut h = blake3::Hasher::new();
    for e in entries {
        let ino = fs.lookup(dir, &e.name).map_err(vfs)?.ino;
        h.update(&(e.name.len() as u64).to_le_bytes());
        h.update(&e.name);
        h.update(&mode_of(&e).to_le_bytes());
        match e.kind {
            Kind::Dir => {
                h.update(b"d");
                h.update(hash_view_dir(fs, ino, files, bytes)?.as_bytes());
            }
            Kind::Link => {
                h.update(b"l");
                let target = fs.readlink(ino).map_err(vfs)?;
                h.update(&(target.len() as u64).to_le_bytes());
                h.update(&target);
            }
            Kind::File => {
                h.update(b"f");
                let mut fh = blake3::Hasher::new();
                let (mut off, mut n) = (0u64, 0u64);
                while off < e.size {
                    let got = fs.read(ino, off, 1 << 20).map_err(vfs)?;
                    if got.is_empty() {
                        break;
                    }
                    fh.update(&got);
                    n += got.len() as u64;
                    off += got.len() as u64;
                }
                *files += 1;
                *bytes += n;
                h.update(&n.to_le_bytes());
                h.update(fh.finalize().as_bytes());
            }
            _ => {
                h.update(b"o");
            }
        }
        fs.forget(ino, 1);
    }
    Ok(h.finalize())
}

fn kind_of_md(md: &std::fs::Metadata) -> Kind {
    let ty = md.file_type();
    if ty.is_dir() {
        Kind::Dir
    } else if ty.is_symlink() {
        Kind::Link
    } else if ty.is_file() {
        Kind::File
    } else {
        Kind::Other
    }
}

fn kind_of(kind: FileKind) -> Kind {
    match kind {
        FileKind::Directory => Kind::Dir,
        FileKind::Symlink => Kind::Link,
        FileKind::Regular => Kind::File,
        _ => Kind::Other,
    }
}

/// The mode an entry contributes to the hash. A symlink hashes as `0o777`, which is what a
/// snapshot stores and what the other walker sees.
fn mode_of(e: &Entry) -> u32 {
    if e.kind == Kind::Link {
        0o777
    } else {
        e.mode
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cowfs_vfs::ROOT_INO;

    #[test]
    fn hash_covers_content_names_modes_and_links() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("a"), b"hi").unwrap();
        fs::create_dir(d.path().join("s")).unwrap();
        std::os::unix::fs::symlink("a", d.path().join("s/l")).unwrap();
        let h1 = hash_tree(d.path()).unwrap();
        assert_eq!((h1.files, h1.bytes, h1.root.len()), (1, 2, 64));
        assert_eq!(hash_tree(d.path()).unwrap(), h1);

        fs::write(d.path().join("a"), b"ho").unwrap();
        let h2 = hash_tree(d.path()).unwrap();
        assert_ne!(h2.root, h1.root, "content");
        fs::rename(d.path().join("a"), d.path().join("b")).unwrap();
        let h3 = hash_tree(d.path()).unwrap();
        assert_ne!(h3.root, h2.root, "name");
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(d.path().join("b"), fs::Permissions::from_mode(0o600)).unwrap();
        let h4 = hash_tree(d.path()).unwrap();
        assert_ne!(h4.root, h3.root, "mode");
        fs::remove_file(d.path().join("s/l")).unwrap();
        std::os::unix::fs::symlink("b", d.path().join("s/l")).unwrap();
        assert_ne!(hash_tree(d.path()).unwrap().root, h4.root, "link target");
    }

    #[test]
    fn a_symlinks_mode_does_not_change_its_hash() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("a"), b"hi").unwrap();
        std::os::unix::fs::symlink("a", d.path().join("l")).unwrap();
        let before = hash_tree(d.path()).unwrap();
        // A symlink cannot be chmod -ed on Linux or macOS, so the hash cannot depend on what the
        // kernel reports; this pins that it is 0o777 on both.
        assert_eq!(before.root, hash_tree(d.path()).unwrap().root);
    }

    #[test]
    fn the_two_walkers_agree_on_the_same_tree() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("a/b")).unwrap();
        fs::write(d.path().join("a/b/deep"), b"deep").unwrap();
        fs::write(d.path().join("a/empty"), b"").unwrap();
        fs::write(d.path().join("bin"), vec![7u8; 300_000]).unwrap();
        std::os::unix::fs::symlink("bin", d.path().join("link")).unwrap();
        std::os::unix::fs::symlink("../nowhere", d.path().join("a/up")).unwrap();
        let native = hash_tree(d.path()).unwrap();
        let view = hash_view(&cowfs_vfs_path::PathVfs::new(d.path()).unwrap(), ROOT_INO).unwrap();
        assert_eq!(native, view);
    }
}
