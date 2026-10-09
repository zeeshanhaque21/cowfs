use std::cmp::Ordering;

use async_trait::async_trait;

use crate::nfs::*;

/// One READDIR or READDIRPLUS entry. `cookie` is the resume point: the client passes it back to
/// continue after this entry. It must be stable while the directory changes and never 0.
#[derive(Default, Debug)]
pub struct DirEntry {
    pub fileid: fileid3,
    pub name: filename3,
    /// Filled when the caller asked for attributes.
    pub attr: Option<fattr3>,
    pub cookie: cookie3,
}

#[derive(Default, Debug)]
pub struct ReadDirResult {
    pub entries: Vec<DirEntry>,
    pub end: bool,
}

/// What capabilities are supported
#[derive(Debug, Clone, Copy)]
pub enum VFSCapabilities {
    ReadOnly,
    ReadWrite,
}

/// The API to implement to serve a file system over NFSv3.
///
/// Files are identified by a 64-bit file id (an inode number, never 0). File handles are
/// `generation || fileid`, so they stay valid for the lifetime of the server and a restarted
/// server rejects old handles with `NFS3ERR_STALE`.
///
/// readdir pagination: no cookie verifier. A cookie is the position after an entry and must keep
/// working while entries are added and removed, including hardlinks that share a file id.
#[async_trait]
pub trait NFSFileSystem: Send + Sync {
    fn capabilities(&self) -> VFSCapabilities {
        VFSCapabilities::ReadWrite
    }

    /// The id of the root directory "/".
    fn root_dir(&self) -> fileid3;

    /// Fixed for the lifetime of the server, different for every start.
    fn generation(&self) -> u64;

    /// Looks up `filename` in `dirid`, including "." and "..".
    async fn lookup(
        &self,
        dirid: fileid3,
        filename: &filename3,
    ) -> Result<(fileid3, fattr3), nfsstat3>;

    async fn getattr(&self, id: fileid3) -> Result<fattr3, nfsstat3>;

    /// Never follows symlinks: the attributes are those of the object `id` itself.
    async fn setattr(&self, id: fileid3, setattr: sattr3) -> Result<fattr3, nfsstat3>;

    async fn readlink(&self, id: fileid3) -> Result<nfspath3, nfsstat3>;

    /// Returns (bytes, EOF). Reads past the end return what exists.
    async fn read(&self, id: fileid3, offset: u64, count: u32)
        -> Result<(Vec<u8>, bool), nfsstat3>;

    /// Returns the number of bytes written and the new attributes.
    async fn write(
        &self,
        id: fileid3,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<(u32, fattr3), nfsstat3>;

    /// Makes written data durable (COMMIT, and stable WRITEs).
    async fn commit(&self, id: fileid3) -> Result<(), nfsstat3>;

    /// CREATE with mode UNCHECKED (`guarded` false: an existing file is reused and `attr` applied,
    /// which truncates it if a size is given) or GUARDED (`NFS3ERR_EXIST` if the name exists).
    async fn create(
        &self,
        dirid: fileid3,
        filename: &filename3,
        attr: sattr3,
        guarded: bool,
    ) -> Result<(fileid3, fattr3), nfsstat3>;

    /// CREATE with mode EXCLUSIVE. Retrying with the same verifier succeeds and returns the file.
    async fn create_exclusive(
        &self,
        dirid: fileid3,
        filename: &filename3,
        verf: createverf3,
    ) -> Result<(fileid3, fattr3), nfsstat3>;

    async fn mkdir(
        &self,
        dirid: fileid3,
        dirname: &filename3,
        attr: &sattr3,
    ) -> Result<(fileid3, fattr3), nfsstat3>;

    async fn symlink(
        &self,
        dirid: fileid3,
        linkname: &filename3,
        symlink: &nfspath3,
        attr: &sattr3,
    ) -> Result<(fileid3, fattr3), nfsstat3>;

    /// MKNOD: a fifo, socket, character device or block device. `ftype` is one of `NF3FIFO`,
    /// `NF3SOCK`, `NF3CHR`, `NF3BLK`; `rdev` is zero unless it is a device. The default answers
    /// `NFS3ERR_NOTSUPP`, which is what a file system without special files says.
    async fn mknod(
        &self,
        _dirid: fileid3,
        _name: &filename3,
        _ftype: ftype3,
        _attr: &sattr3,
        _rdev: specdata3,
    ) -> Result<(fileid3, fattr3), nfsstat3> {
        Err(nfsstat3::NFS3ERR_NOTSUPP)
    }

    /// Creates a hard link `name` in `dir_id` to `file_id` and returns the file's attributes.
    async fn link(
        &self,
        file_id: fileid3,
        dir_id: fileid3,
        name: &filename3,
    ) -> Result<fattr3, nfsstat3>;

    /// Removes a non-directory.
    async fn remove(&self, dirid: fileid3, filename: &filename3) -> Result<(), nfsstat3>;

    /// Removes an empty directory.
    async fn rmdir(&self, dirid: fileid3, filename: &filename3) -> Result<(), nfsstat3>;

    async fn rename(
        &self,
        from_dirid: fileid3,
        from_filename: &filename3,
        to_dirid: fileid3,
        to_filename: &filename3,
    ) -> Result<(), nfsstat3>;

    /// Up to `max_entries` entries after `cookie` (0 starts at the beginning), without "." and "..".
    /// Entries carry attributes when `with_attrs` is set.
    async fn readdir(
        &self,
        dirid: fileid3,
        cookie: cookie3,
        max_entries: usize,
        with_attrs: bool,
    ) -> Result<ReadDirResult, nfsstat3>;

    /// Dynamic file system information (FSSTAT). `obj_attributes` is filled by the caller.
    async fn fsstat(&self, id: fileid3) -> Result<fsstat3, nfsstat3>;

    /// Static file system information (FSINFO).
    async fn fsinfo(&self, root_fileid: fileid3) -> Result<fsinfo3, nfsstat3> {
        let dir_attr = match self.getattr(root_fileid).await {
            Ok(v) => post_op_attr::attributes(v),
            Err(_) => post_op_attr::Void,
        };
        Ok(fsinfo3 {
            obj_attributes: dir_attr,
            rtmax: 1024 * 1024,
            rtpref: 1024 * 1024,
            rtmult: 4096,
            wtmax: 1024 * 1024,
            wtpref: 1024 * 1024,
            wtmult: 4096,
            dtpref: 64 * 1024,
            maxfilesize: i64::MAX as u64,
            time_delta: nfstime3 {
                seconds: 0,
                nseconds: 1,
            },
            properties: FSF_LINK | FSF_SYMLINK | FSF_HOMOGENEOUS | FSF_CANSETTIME,
        })
    }

    /// Path configuration (PATHCONF). `obj_attributes` is filled by the caller.
    async fn pathconf(&self, _id: fileid3) -> Result<pathconf3, nfsstat3> {
        Ok(pathconf3 {
            obj_attributes: post_op_attr::Void,
            linkmax: 65000,
            name_max: 255,
            no_trunc: true,
            chown_restricted: true,
            case_insensitive: false,
            case_preserving: true,
        })
    }

    /// Converts the fileid to an opaque NFS file handle.
    fn id_to_fh(&self, id: fileid3) -> nfs_fh3 {
        let mut data = Vec::with_capacity(16);
        data.extend_from_slice(&self.generation().to_le_bytes());
        data.extend_from_slice(&id.to_le_bytes());
        nfs_fh3 { data }
    }

    /// Converts an opaque NFS file handle to a fileid.
    fn fh_to_id(&self, fh: &nfs_fh3) -> Result<fileid3, nfsstat3> {
        let Ok(bytes) = <[u8; 16]>::try_from(fh.data.as_slice()) else {
            return Err(nfsstat3::NFS3ERR_BADHANDLE);
        };
        let (gen, id) = bytes.split_at(8);
        let gen = u64::from_le_bytes(gen.try_into().map_err(|_| nfsstat3::NFS3ERR_BADHANDLE)?);
        let id = u64::from_le_bytes(id.try_into().map_err(|_| nfsstat3::NFS3ERR_BADHANDLE)?);
        match gen.cmp(&self.generation()) {
            Ordering::Less => Err(nfsstat3::NFS3ERR_STALE),
            Ordering::Greater => Err(nfsstat3::NFS3ERR_BADHANDLE),
            Ordering::Equal => Ok(id),
        }
    }

    /// Converts a complete path to a fileid by walking the directory structure with `lookup`.
    async fn path_to_id(&self, path: &[u8]) -> Result<fileid3, nfsstat3> {
        let mut fid = self.root_dir();
        for component in path.split(|&r| r == b'/').filter(|c| !c.is_empty()) {
            fid = self.lookup(fid, &component.into()).await?.0;
        }
        Ok(fid)
    }

    /// Write verifier: changes when the server restarts so clients resend uncommitted writes.
    fn serverid(&self) -> cookieverf3 {
        self.generation().to_le_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Gen(u64);

    #[async_trait]
    impl NFSFileSystem for Gen {
        fn root_dir(&self) -> fileid3 {
            1
        }
        fn generation(&self) -> u64 {
            self.0
        }
        async fn lookup(&self, _: fileid3, _: &filename3) -> Result<(fileid3, fattr3), nfsstat3> {
            Err(nfsstat3::NFS3ERR_NOENT)
        }
        async fn getattr(&self, _: fileid3) -> Result<fattr3, nfsstat3> {
            Err(nfsstat3::NFS3ERR_STALE)
        }
        async fn setattr(&self, _: fileid3, _: sattr3) -> Result<fattr3, nfsstat3> {
            Err(nfsstat3::NFS3ERR_STALE)
        }
        async fn readlink(&self, _: fileid3) -> Result<nfspath3, nfsstat3> {
            Err(nfsstat3::NFS3ERR_STALE)
        }
        async fn read(&self, _: fileid3, _: u64, _: u32) -> Result<(Vec<u8>, bool), nfsstat3> {
            Err(nfsstat3::NFS3ERR_STALE)
        }
        async fn write(&self, _: fileid3, _: u64, _: Vec<u8>) -> Result<(u32, fattr3), nfsstat3> {
            Err(nfsstat3::NFS3ERR_STALE)
        }
        async fn commit(&self, _: fileid3) -> Result<(), nfsstat3> {
            Ok(())
        }
        async fn create(
            &self,
            _: fileid3,
            _: &filename3,
            _: sattr3,
            _: bool,
        ) -> Result<(fileid3, fattr3), nfsstat3> {
            Err(nfsstat3::NFS3ERR_ROFS)
        }
        async fn create_exclusive(
            &self,
            _: fileid3,
            _: &filename3,
            _: createverf3,
        ) -> Result<(fileid3, fattr3), nfsstat3> {
            Err(nfsstat3::NFS3ERR_ROFS)
        }
        async fn mkdir(
            &self,
            _: fileid3,
            _: &filename3,
            _: &sattr3,
        ) -> Result<(fileid3, fattr3), nfsstat3> {
            Err(nfsstat3::NFS3ERR_ROFS)
        }
        async fn symlink(
            &self,
            _: fileid3,
            _: &filename3,
            _: &nfspath3,
            _: &sattr3,
        ) -> Result<(fileid3, fattr3), nfsstat3> {
            Err(nfsstat3::NFS3ERR_ROFS)
        }
        async fn link(&self, _: fileid3, _: fileid3, _: &filename3) -> Result<fattr3, nfsstat3> {
            Err(nfsstat3::NFS3ERR_ROFS)
        }
        async fn remove(&self, _: fileid3, _: &filename3) -> Result<(), nfsstat3> {
            Err(nfsstat3::NFS3ERR_ROFS)
        }
        async fn rmdir(&self, _: fileid3, _: &filename3) -> Result<(), nfsstat3> {
            Err(nfsstat3::NFS3ERR_ROFS)
        }
        async fn rename(
            &self,
            _: fileid3,
            _: &filename3,
            _: fileid3,
            _: &filename3,
        ) -> Result<(), nfsstat3> {
            Err(nfsstat3::NFS3ERR_ROFS)
        }
        async fn readdir(
            &self,
            _: fileid3,
            _: cookie3,
            _: usize,
            _: bool,
        ) -> Result<ReadDirResult, nfsstat3> {
            Ok(ReadDirResult::default())
        }
        async fn fsstat(&self, _: fileid3) -> Result<fsstat3, nfsstat3> {
            Err(nfsstat3::NFS3ERR_STALE)
        }
    }

    #[test]
    fn handle_round_trip_and_generation_checks() {
        let fs = Gen(10);
        let fh = fs.id_to_fh(42);
        assert_eq!(fs.fh_to_id(&fh), Ok(42));
        assert_eq!(Gen(11).fh_to_id(&fh), Err(nfsstat3::NFS3ERR_STALE));
        assert_eq!(Gen(9).fh_to_id(&fh), Err(nfsstat3::NFS3ERR_BADHANDLE));
        assert_eq!(
            fs.fh_to_id(&nfs_fh3 { data: vec![1; 15] }),
            Err(nfsstat3::NFS3ERR_BADHANDLE)
        );
        assert_eq!(fs.serverid(), 10u64.to_le_bytes());
    }

    #[tokio::test]
    async fn path_walk_reports_the_first_missing_component() {
        let fs = Gen(1);
        assert_eq!(fs.path_to_id(b"/").await, Ok(1));
        assert_eq!(fs.path_to_id(b"//").await, Ok(1));
        assert_eq!(fs.path_to_id(b"/a/b").await, Err(nfsstat3::NFS3ERR_NOENT));
    }

    #[test]
    fn default_fsinfo_and_pathconf_advertise_link_support() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let fs = Gen(1);
        let info = rt.block_on(fs.fsinfo(1)).unwrap();
        assert_ne!(info.properties & FSF_LINK, 0);
        let conf = rt.block_on(fs.pathconf(1)).unwrap();
        assert_eq!(conf.name_max, 255);
    }
}
