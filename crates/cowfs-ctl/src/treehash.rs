use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

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
/// the child directory's hash. Symlinks are never followed.
pub fn hash_tree(dir: &Path) -> io::Result<TreeHash> {
    let (mut files, mut bytes) = (0, 0);
    let root = hash_dir(dir, &mut files, &mut bytes)?;
    Ok(TreeHash {
        root: root.to_hex().to_string(),
        files,
        bytes,
    })
}

fn hash_dir(dir: &Path, files: &mut u64, bytes: &mut u64) -> io::Result<blake3::Hash> {
    let mut entries = fs::read_dir(dir)?.collect::<io::Result<Vec<_>>>()?;
    entries.sort_by_key(|e| e.file_name());
    let mut h = blake3::Hasher::new();
    for e in entries {
        let name = e.file_name();
        let md = fs::symlink_metadata(e.path())?;
        h.update(&(name.as_bytes().len() as u64).to_le_bytes());
        h.update(name.as_bytes());
        h.update(&(md.mode() & 0o7777).to_le_bytes());
        let ty = md.file_type();
        if ty.is_dir() {
            h.update(b"d");
            h.update(hash_dir(&e.path(), files, bytes)?.as_bytes());
        } else if ty.is_symlink() {
            h.update(b"l");
            let target = fs::read_link(e.path())?;
            h.update(&(target.as_os_str().as_bytes().len() as u64).to_le_bytes());
            h.update(target.as_os_str().as_bytes());
        } else if ty.is_file() {
            h.update(b"f");
            let mut fh = blake3::Hasher::new();
            let n = io::copy(&mut fs::File::open(e.path())?, &mut fh)?;
            *files += 1;
            *bytes += n;
            h.update(&n.to_le_bytes());
            h.update(fh.finalize().as_bytes());
        } else {
            h.update(b"o");
        }
    }
    Ok(h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
