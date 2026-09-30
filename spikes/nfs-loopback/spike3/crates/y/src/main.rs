use async_trait::async_trait;
use nfsserve::nfs::*;
use nfsserve::tcp::{NFSTcp, NFSTcpListener};
use nfsserve::vfs::{DirEntry, NFSFileSystem, ReadDirResult, VFSCapabilities};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const ROOT_ID: u64 = 1;

struct Mirror {
    root: PathBuf,
    hide_appledouble: bool,
    trace: bool,
    paths: Mutex<HashMap<u64, PathBuf>>,
}

fn err(e: io::Error) -> nfsstat3 {
    match e.raw_os_error() {
        Some(libc::ENOENT) => nfsstat3::NFS3ERR_NOENT,
        Some(libc::EEXIST) => nfsstat3::NFS3ERR_EXIST,
        Some(libc::EACCES) => nfsstat3::NFS3ERR_ACCES,
        Some(libc::EPERM) => nfsstat3::NFS3ERR_PERM,
        Some(libc::ENOTDIR) => nfsstat3::NFS3ERR_NOTDIR,
        Some(libc::EISDIR) => nfsstat3::NFS3ERR_ISDIR,
        Some(libc::EINVAL) => nfsstat3::NFS3ERR_INVAL,
        Some(libc::ENOSPC) => nfsstat3::NFS3ERR_NOSPC,
        Some(libc::ENOTEMPTY) => nfsstat3::NFS3ERR_NOTEMPTY,
        Some(libc::EROFS) => nfsstat3::NFS3ERR_ROFS,
        Some(libc::ENAMETOOLONG) => nfsstat3::NFS3ERR_NAMETOOLONG,
        Some(libc::EXDEV) => nfsstat3::NFS3ERR_XDEV,
        _ => nfsstat3::NFS3ERR_IO,
    }
}

fn ftime(secs: i64, nsecs: i64) -> nfstime3 {
    nfstime3 { seconds: secs as u32, nseconds: nsecs as u32 }
}

impl Mirror {
    fn tr(&self, msg: String) {
        if self.trace {
            let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
            eprintln!("{}.{:06} {}", t.as_secs() % 1000, t.subsec_micros(), msg);
        }
    }

    fn path(&self, id: u64) -> Result<PathBuf, nfsstat3> {
        self.paths.lock().unwrap().get(&id).cloned().ok_or(nfsstat3::NFS3ERR_STALE)
    }

    fn remember(&self, id: u64, p: PathBuf) {
        self.paths.lock().unwrap().insert(id, p);
    }

    fn attr_of(&self, id_hint: Option<u64>, p: &Path) -> Result<fattr3, nfsstat3> {
        let md = fs::symlink_metadata(p).map_err(err)?;
        let ft = md.file_type();
        let ftype = if ft.is_dir() {
            ftype3::NF3DIR
        } else if ft.is_symlink() {
            ftype3::NF3LNK
        } else {
            ftype3::NF3REG
        };
        Ok(fattr3 {
            ftype,
            mode: md.mode() & 0o7777,
            nlink: md.nlink() as u32,
            uid: md.uid(),
            gid: md.gid(),
            size: md.size(),
            used: md.blocks() * 512,
            rdev: specdata3::default(),
            fsid: 0,
            fileid: id_hint.unwrap_or(md.ino()),
            atime: ftime(md.atime(), md.atime_nsec()),
            mtime: ftime(md.mtime(), md.mtime_nsec()),
            ctime: ftime(md.ctime(), md.ctime_nsec()),
        })
    }

    fn attr_by_id(&self, id: u64) -> Result<fattr3, nfsstat3> {
        let p = self.path(id)?;
        self.attr_of(if id == ROOT_ID { Some(ROOT_ID) } else { None }, &p)
    }

    fn child(&self, dirid: u64, name: &filename3) -> Result<PathBuf, nfsstat3> {
        Ok(self.path(dirid)?.join(OsStr::from_bytes(name)))
    }

    fn register(&self, p: &Path) -> Result<u64, nfsstat3> {
        let id = fs::symlink_metadata(p).map_err(err)?.ino();
        self.remember(id, p.to_path_buf());
        Ok(id)
    }

    fn open_for_write(p: &Path) -> io::Result<File> {
        match OpenOptions::new().write(true).open(p) {
            Err(e) if e.raw_os_error() == Some(libc::EACCES) => {
                let orig = fs::metadata(p)?.permissions();
                fs::set_permissions(p, fs::Permissions::from_mode(orig.mode() | 0o200))?;
                let r = OpenOptions::new().write(true).open(p);
                let _ = fs::set_permissions(p, orig);
                r
            }
            r => r,
        }
    }

    fn apply(&self, p: &Path, a: &sattr3) -> Result<(), nfsstat3> {
        if let set_mode3::mode(m) = a.mode {
            fs::set_permissions(p, fs::Permissions::from_mode(m & 0o7777)).map_err(err)?;
        }
        if let set_size3::size(s) = a.size {
            Self::open_for_write(p).and_then(|f| f.set_len(s)).map_err(err)?;
        }
        let at = match a.atime {
            set_atime::SET_TO_CLIENT_TIME(t) => Some(filetime::FileTime::from(t)),
            set_atime::SET_TO_SERVER_TIME => Some(filetime::FileTime::now()),
            set_atime::DONT_CHANGE => None,
        };
        let mt = match a.mtime {
            set_mtime::SET_TO_CLIENT_TIME(t) => Some(filetime::FileTime::from(t)),
            set_mtime::SET_TO_SERVER_TIME => Some(filetime::FileTime::now()),
            set_mtime::DONT_CHANGE => None,
        };
        if at.is_some() || mt.is_some() {
            let md = fs::metadata(p).map_err(err)?;
            let at = at.unwrap_or_else(|| filetime::FileTime::from_last_access_time(&md));
            let mt = mt.unwrap_or_else(|| filetime::FileTime::from_last_modification_time(&md));
            filetime::set_file_times(p, at, mt).map_err(err)?;
        }
        Ok(())
    }
}

#[async_trait]
impl NFSFileSystem for Mirror {
    fn capabilities(&self) -> VFSCapabilities {
        VFSCapabilities::ReadWrite
    }
    fn root_dir(&self) -> fileid3 {
        ROOT_ID
    }

    async fn lookup(&self, dirid: fileid3, filename: &filename3) -> Result<fileid3, nfsstat3> {
        let dir = self.path(dirid)?;
        match filename.as_ref() {
            b"." => return Ok(dirid),
            b".." => {
                if dirid == ROOT_ID {
                    return Ok(ROOT_ID);
                }
                let parent = dir.parent().ok_or(nfsstat3::NFS3ERR_NOENT)?.to_path_buf();
                return if parent == self.root { Ok(ROOT_ID) } else { self.register(&parent) };
            }
            _ => {}
        }
        self.register(&dir.join(OsStr::from_bytes(filename)))
    }

    async fn getattr(&self, id: fileid3) -> Result<fattr3, nfsstat3> {
        self.attr_by_id(id)
    }

    async fn setattr(&self, id: fileid3, setattr: sattr3) -> Result<fattr3, nfsstat3> {
        let p = self.path(id)?;
        self.tr(format!("SETATTR {} mode={} size={} atime={} mtime={}", p.display(), matches!(setattr.mode, set_mode3::mode(_)), match setattr.size { set_size3::size(n) => n as i64, _ => -1 }, !matches!(setattr.atime, set_atime::DONT_CHANGE), !matches!(setattr.mtime, set_mtime::DONT_CHANGE)));
        self.apply(&p, &setattr)?;
        self.attr_by_id(id)
    }

    async fn read(&self, id: fileid3, offset: u64, count: u32) -> Result<(Vec<u8>, bool), nfsstat3> {
        let f = File::open(self.path(id)?).map_err(err)?;
        let size = f.metadata().map_err(err)?.len();
        let mut buf = vec![0u8; count as usize];
        let n = f.read_at(&mut buf, offset).map_err(err)?;
        buf.truncate(n);
        Ok((buf, offset + n as u64 >= size))
    }

    async fn write(&self, id: fileid3, offset: u64, data: &[u8]) -> Result<fattr3, nfsstat3> {
        self.tr(format!("WRITE id={id} off={offset} len={}", data.len()));
        let path = self.path(id)?;
        self.tr(format!("  WRITE path={}", path.display()));
        let f = Self::open_for_write(&path).map_err(|e| {
            self.tr(format!("  WRITE open err {e}"));
            err(e)
        })?;
        f.write_all_at(data, offset).map_err(|e| {
            self.tr(format!("  WRITE write err {e}"));
            err(e)
        })?;
        self.attr_by_id(id)
    }

    async fn create(&self, dirid: fileid3, filename: &filename3, attr: sattr3) -> Result<(fileid3, fattr3), nfsstat3> {
        self.tr(format!("CREATE dir={dirid} name={:?}", filename));
        let p = self.child(dirid, filename)?;
        OpenOptions::new().write(true).create(true).truncate(false).open(&p).map_err(err)?;
        self.apply(&p, &attr)?;
        let id = self.register(&p)?;
        Ok((id, self.attr_of(None, &p)?))
    }

    async fn create_exclusive(&self, dirid: fileid3, filename: &filename3) -> Result<fileid3, nfsstat3> {
        self.tr(format!("CREATE_EXCL dir={dirid} name={:?}", filename));
        let p = self.child(dirid, filename)?;
        OpenOptions::new().write(true).create_new(true).open(&p).map_err(err)?;
        self.register(&p)
    }

    async fn mkdir(&self, dirid: fileid3, dirname: &filename3) -> Result<(fileid3, fattr3), nfsstat3> {
        let p = self.child(dirid, dirname)?;
        fs::create_dir(&p).map_err(err)?;
        let id = self.register(&p)?;
        Ok((id, self.attr_of(None, &p)?))
    }

    async fn remove(&self, dirid: fileid3, filename: &filename3) -> Result<(), nfsstat3> {
        self.tr(format!("REMOVE dir={dirid} name={:?}", filename));
        let p = self.child(dirid, filename)?;
        let md = fs::symlink_metadata(&p).map_err(err)?;
        if md.is_dir() {
            fs::remove_dir(&p).map_err(err)
        } else {
            fs::remove_file(&p).map_err(err)
        }
    }

    async fn rename(&self, from_dirid: fileid3, from_filename: &filename3, to_dirid: fileid3, to_filename: &filename3) -> Result<(), nfsstat3> {
        self.tr(format!("RENAME {:?} -> {:?}", from_filename, to_filename));
        let from = self.child(from_dirid, from_filename)?;
        let to = self.child(to_dirid, to_filename)?;
        fs::rename(&from, &to).map_err(err)?;
        let md = fs::symlink_metadata(&to).map_err(err)?;
        if !md.is_dir() {
            self.remember(md.ino(), to);
            return Ok(());
        }
        let mut m = self.paths.lock().unwrap();
        for v in m.values_mut() {
            if let Ok(rest) = v.strip_prefix(&from) {
                *v = if rest.as_os_str().is_empty() { to.clone() } else { to.join(rest) };
            }
        }
        Ok(())
    }

    async fn readdir(&self, dirid: fileid3, start_after: fileid3, max_entries: usize) -> Result<ReadDirResult, nfsstat3> {
        let dir = self.path(dirid)?;
        let mut ents: Vec<(Vec<u8>, PathBuf)> = fs::read_dir(&dir)
            .map_err(err)?
            .filter_map(|e| e.ok())
            .filter(|e| !(self.hide_appledouble && e.file_name().as_bytes().starts_with(b"._")))
            .map(|e| (e.file_name().as_bytes().to_vec(), e.path()))
            .collect();
        ents.sort();
        let mut out = ReadDirResult::default();
        for i in (start_after as usize).min(ents.len())..ents.len() {
            if out.entries.len() >= max_entries {
                return Ok(out);
            }
            let (name, p) = &ents[i];
            if let Ok(attr) = self.attr_of(None, p) {
                self.remember(attr.fileid, p.clone());
                out.entries.push(DirEntry { fileid: attr.fileid, name: name.clone().into(), attr, cookie: i as u64 + 1 });
            }
        }
        out.end = true;
        Ok(out)
    }

    async fn symlink(&self, dirid: fileid3, linkname: &filename3, symlink: &nfspath3, attr: &sattr3) -> Result<(fileid3, fattr3), nfsstat3> {
        let p = self.child(dirid, linkname)?;
        std::os::unix::fs::symlink(OsStr::from_bytes(symlink), &p).map_err(err)?;
        let _ = attr;
        let id = self.register(&p)?;
        Ok((id, self.attr_of(None, &p)?))
    }

    async fn link(&self, file_id: fileid3, dir_id: fileid3, name: &filename3) -> Result<fattr3, nfsstat3> {
        self.tr(format!("LINK {} -> {:?}", file_id, name));
        let src = self.path(file_id)?;
        let dst = self.child(dir_id, name)?;
        fs::hard_link(&src, &dst).map_err(err)?;
        self.attr_of(None, &src)
    }

    async fn readlink(&self, id: fileid3) -> Result<nfspath3, nfsstat3> {
        let t = fs::read_link(self.path(id)?).map_err(err)?;
        Ok(t.as_os_str().as_bytes().to_vec().into())
    }
}

#[tokio::main]
async fn main() -> io::Result<()> {
    let mut root = None;
    let mut port = 11111u16;
    let mut hide = false;
    let mut trace = false;
    let mut a = std::env::args().skip(1);
    while let Some(x) = a.next() {
        match x.as_str() {
            "--root" => root = Some(PathBuf::from(a.next().unwrap())),
            "--hide-appledouble" => hide = true,
            "--trace" => trace = true,
            "--port" => port = a.next().unwrap().parse().unwrap(),
            _ => panic!("unknown arg {x}"),
        }
    }
    let root = fs::canonicalize(root.expect("--root required"))?;
    let mut paths = HashMap::new();
    paths.insert(ROOT_ID, root.clone());
    let fs = Mirror { root, hide_appledouble: hide, trace, paths: Mutex::new(paths) };
    let listener = NFSTcpListener::bind(&format!("127.0.0.1:{port}"), fs).await?;
    eprintln!("listening on 127.0.0.1:{}", listener.get_listen_port());
    tokio::spawn(async {
        let mut s = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined1()).unwrap();
        while s.recv().await.is_some() {
            eprint!("STATS\n{}", nfsserve::take_stats());
        }
    });
    listener.handle_forever().await
}
