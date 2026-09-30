//! A tiny NFSv3 client over raw RPC frames, enough to drive the server in-process.
#![allow(dead_code)]
pub mod counting;
pub mod reuse;

use std::io::{Cursor, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;

use cowfs_nfs::{MountOptions, Server};
use cowfs_vfs::Vfs;
use cowfs_vfs_test::MemVfs;
use nfsserve::nfs::*;
use nfsserve::xdr::XDR;

pub type Rd = Cursor<Vec<u8>>;

pub const OK: u32 = nfsstat3::NFS3_OK as u32;
pub const NOENT: u32 = nfsstat3::NFS3ERR_NOENT as u32;
pub const EXIST: u32 = nfsstat3::NFS3ERR_EXIST as u32;
pub const NOTDIR: u32 = nfsstat3::NFS3ERR_NOTDIR as u32;
pub const ISDIR: u32 = nfsstat3::NFS3ERR_ISDIR as u32;
pub const NOTEMPTY: u32 = nfsstat3::NFS3ERR_NOTEMPTY as u32;
pub const STALE: u32 = nfsstat3::NFS3ERR_STALE as u32;
pub const BADHANDLE: u32 = nfsstat3::NFS3ERR_BADHANDLE as u32;
pub const INVAL: u32 = nfsstat3::NFS3ERR_INVAL as u32;
pub const NAMETOOLONG: u32 = nfsstat3::NFS3ERR_NAMETOOLONG as u32;
pub const NOTSUPP: u32 = nfsstat3::NFS3ERR_NOTSUPP as u32;
pub const ROFS: u32 = nfsstat3::NFS3ERR_ROFS as u32;

const NFS: u32 = 100_003;
const MOUNT: u32 = 100_005;

#[derive(Default)]
pub struct Args(Vec<u8>);

impl Args {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn put<T: XDR>(mut self, v: &T) -> Self {
        v.serialize(&mut self.0).unwrap();
        self
    }
}

pub fn dec<T: XDR + Default>(r: &mut Rd) -> T {
    let mut t = T::default();
    t.deserialize(r).unwrap();
    t
}

pub fn attr(p: post_op_attr) -> Option<fattr3> {
    match p {
        post_op_attr::attributes(a) => Some(a),
        post_op_attr::Void => None,
    }
}

pub fn fh_of(p: post_op_fh3) -> Option<nfs_fh3> {
    match p {
        post_op_fh3::handle(h) => Some(h),
        post_op_fh3::Void => None,
    }
}

pub fn name(s: &str) -> filename3 {
    s.as_bytes().into()
}

pub fn dirop(dir: &nfs_fh3, n: &str) -> diropargs3 {
    diropargs3 {
        dir: dir.clone(),
        name: name(n),
    }
}

pub fn sattr_mode(m: u32) -> sattr3 {
    sattr3 {
        mode: set_mode3::mode(m),
        ..sattr3::default()
    }
}

pub fn sattr_size(n: u64) -> sattr3 {
    sattr3 {
        size: set_size3::size(n),
        ..sattr3::default()
    }
}

pub fn sattr_mtime(secs: u32, nsecs: u32) -> sattr3 {
    sattr3 {
        mtime: set_mtime::SET_TO_CLIENT_TIME(nfstime3 {
            seconds: secs,
            nseconds: nsecs,
        }),
        ..sattr3::default()
    }
}

#[derive(Debug, Clone)]
pub struct Listed {
    pub name: String,
    pub fileid: u64,
    pub cookie: u64,
    pub attr: Option<fattr3>,
    pub fh: Option<nfs_fh3>,
}

pub struct Nfs {
    s: TcpStream,
    xid: u32,
    pub root: nfs_fh3,
}

/// A running server over `vfs` and a connected, mounted client.
pub fn serve(vfs: Arc<dyn Vfs>, mut opts: MountOptions) -> (Server, Nfs) {
    opts.check_peer_uid = false;
    let server = Server::start(vfs, &opts, None).unwrap();
    let nfs = Nfs::connect(server.port());
    (server, nfs)
}

pub fn memfs() -> Arc<MemVfs> {
    Arc::new(MemVfs::new())
}

impl Nfs {
    /// A connection that has not mounted: `root` is empty until `mount` succeeds.
    pub fn attach(port: u16, root: nfs_fh3) -> Nfs {
        let s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_nodelay(true).unwrap();
        s.set_read_timeout(Some(std::time::Duration::from_secs(20)))
            .unwrap();
        Nfs { s, xid: 0, root }
    }

    /// MNT of "/": the mount status and, on success, the root handle.
    pub fn mount(&mut self) -> (u32, Option<nfs_fh3>) {
        let (acc, mut r) = self.raw(MOUNT, 3, 1, Args::new().put(&b"/".to_vec()));
        assert_eq!(acc, 0);
        let st = dec::<u32>(&mut r);
        if st != 0 {
            return (st, None);
        }
        let h: Vec<u8> = dec(&mut r);
        (st, Some(nfs_fh3 { data: h }))
    }

    /// Connects and mounts: the first client of a one-shot server.
    pub fn connect(port: u16) -> Nfs {
        let mut n = Nfs::attach(port, nfs_fh3::default());
        let (st, root) = n.mount();
        assert_eq!(st, 0, "MNT refused");
        n.root = root.unwrap();
        n
    }

    /// Sends one call and returns (accept_stat, reply body after it).
    pub fn raw(&mut self, prog: u32, vers: u32, proc: u32, args: Args) -> (u32, Rd) {
        self.xid += 1;
        let mut m = Vec::new();
        for w in [self.xid, 0, 2, prog, vers, proc, 0, 0, 0, 0] {
            m.extend_from_slice(&w.to_be_bytes());
        }
        m.extend_from_slice(&args.0);
        self.send(&m);
        let mut r = Cursor::new(self.recv());
        assert_eq!(dec::<u32>(&mut r), self.xid);
        assert_eq!(dec::<u32>(&mut r), 1, "reply");
        assert_eq!(dec::<u32>(&mut r), 0, "accepted");
        let _flavor: u32 = dec(&mut r);
        let _verf: Vec<u8> = dec(&mut r);
        let acc: u32 = dec(&mut r);
        (acc, r)
    }

    /// The next call uses this xid, to build retransmissions.
    pub fn set_next_xid(&mut self, xid: u32) {
        self.xid = xid.wrapping_sub(1);
    }

    pub fn send(&mut self, m: &[u8]) {
        self.s
            .write_all(&(m.len() as u32 | 1 << 31).to_be_bytes())
            .unwrap();
        self.s.write_all(m).unwrap();
    }

    pub fn recv(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let mut h = [0u8; 4];
            self.s.read_exact(&mut h).unwrap();
            let h = u32::from_be_bytes(h);
            let mut b = vec![0u8; (h & 0x7fff_ffff) as usize];
            self.s.read_exact(&mut b).unwrap();
            out.extend_from_slice(&b);
            if h >> 31 == 1 {
                return out;
            }
        }
    }

    /// An accepted NFS call: returns (nfsstat3 as u32, the rest of the reply).
    pub fn call(&mut self, proc: u32, args: Args) -> (u32, Rd) {
        let (acc, mut r) = self.raw(NFS, 3, proc, args);
        assert_eq!(acc, 0, "rpc not accepted");
        (dec(&mut r), r)
    }

    pub fn getattr(&mut self, fh: &nfs_fh3) -> (u32, Option<fattr3>) {
        let (st, mut r) = self.call(1, Args::new().put(fh));
        (st, (st == OK).then(|| dec(&mut r)))
    }

    /// GETATTR that reports a closed or silent connection as `None` instead of panicking.
    pub fn try_getattr(&mut self, fh: &nfs_fh3) -> Option<u32> {
        self.xid += 1;
        let mut m = Vec::new();
        for w in [self.xid, 0, 2, 100_003, 3, 1, 0, 0, 0, 0] {
            m.extend_from_slice(&w.to_be_bytes());
        }
        Args::new().put(fh).0.iter().for_each(|b| m.push(*b));
        self.s
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .ok()?;
        self.s
            .write_all(&(m.len() as u32 | 1 << 31).to_be_bytes())
            .ok()?;
        self.s.write_all(&m).ok()?;
        let mut h = [0u8; 4];
        self.s.read_exact(&mut h).ok()?;
        let mut b = vec![0u8; (u32::from_be_bytes(h) & 0x7fff_ffff) as usize];
        self.s.read_exact(&mut b).ok()?;
        Some(0)
    }

    pub fn attrs(&mut self, fh: &nfs_fh3) -> fattr3 {
        let (st, a) = self.getattr(fh);
        assert_eq!(st, OK);
        a.unwrap()
    }

    /// (status, handle, object attributes, directory attributes)
    pub fn lookup(
        &mut self,
        dir: &nfs_fh3,
        n: &str,
    ) -> (u32, Option<nfs_fh3>, Option<fattr3>, Option<fattr3>) {
        let (st, mut r) = self.call(3, Args::new().put(&dirop(dir, n)));
        if st == OK {
            let fh: nfs_fh3 = dec(&mut r);
            let obj = attr(dec(&mut r));
            let d = attr(dec(&mut r));
            (st, Some(fh), obj, d)
        } else {
            (st, None, None, attr(dec(&mut r)))
        }
    }

    pub fn must_lookup(&mut self, dir: &nfs_fh3, n: &str) -> nfs_fh3 {
        let (st, fh, _, _) = self.lookup(dir, n);
        assert_eq!(st, OK, "lookup {n}");
        fh.unwrap()
    }

    /// mode 0 unchecked, 1 guarded, 2 exclusive (attr ignored, `verf` used)
    pub fn create(
        &mut self,
        dir: &nfs_fh3,
        n: &str,
        mode: u32,
        sattr: sattr3,
        verf: [u8; 8],
    ) -> (u32, Option<nfs_fh3>, Option<fattr3>) {
        let mut a = Args::new().put(&dirop(dir, n)).put(&mode);
        a = if mode == 2 {
            a.put(&verf)
        } else {
            a.put(&sattr)
        };
        let (st, mut r) = self.call(8, a);
        if st == OK {
            (st, fh_of(dec(&mut r)), attr(dec(&mut r)))
        } else {
            (st, None, None)
        }
    }

    pub fn create_file(&mut self, dir: &nfs_fh3, n: &str) -> nfs_fh3 {
        let (st, fh, _) = self.create(dir, n, 1, sattr_mode(0o644), [0; 8]);
        assert_eq!(st, OK, "create {n}");
        fh.unwrap()
    }

    /// (status, count, committed)
    pub fn write(&mut self, fh: &nfs_fh3, off: u64, data: &[u8], stable: u32) -> (u32, u32, u32) {
        let a = Args::new()
            .put(fh)
            .put(&off)
            .put(&(data.len() as u32))
            .put(&stable)
            .put(&data.to_vec());
        let (st, mut r) = self.call(7, a);
        if st != OK {
            return (st, 0, 0);
        }
        let _wcc: wcc_data = dec(&mut r);
        (st, dec(&mut r), dec(&mut r))
    }

    /// (status, data, eof)
    pub fn read(&mut self, fh: &nfs_fh3, off: u64, count: u32) -> (u32, Vec<u8>, bool) {
        let (st, mut r) = self.call(6, Args::new().put(fh).put(&off).put(&count));
        let _a: post_op_attr = dec(&mut r);
        if st != OK {
            return (st, vec![], false);
        }
        let n: u32 = dec(&mut r);
        let eof: bool = dec(&mut r);
        let data: Vec<u8> = dec(&mut r);
        assert_eq!(n as usize, data.len());
        (st, data, eof)
    }

    pub fn setattr(&mut self, fh: &nfs_fh3, s: sattr3) -> (u32, Option<fattr3>) {
        let (st, mut r) = self.call(2, Args::new().put(fh).put(&s).put(&false));
        let w: wcc_data = dec(&mut r);
        (st, attr(w.after))
    }

    pub fn commit(&mut self, fh: &nfs_fh3) -> u32 {
        self.call(21, Args::new().put(fh).put(&0u64).put(&0u32)).0
    }

    pub fn mkdir(&mut self, dir: &nfs_fh3, n: &str) -> (u32, Option<nfs_fh3>) {
        let (st, mut r) = self.call(9, Args::new().put(&dirop(dir, n)).put(&sattr_mode(0o755)));
        (st, (st == OK).then(|| fh_of(dec(&mut r)).unwrap()))
    }

    pub fn symlink(&mut self, dir: &nfs_fh3, n: &str, target: &str) -> (u32, Option<nfs_fh3>) {
        let a = Args::new()
            .put(&dirop(dir, n))
            .put(&sattr3::default())
            .put(&target.as_bytes().to_vec());
        let (st, mut r) = self.call(10, a);
        (st, (st == OK).then(|| fh_of(dec(&mut r)).unwrap()))
    }

    pub fn readlink(&mut self, fh: &nfs_fh3) -> (u32, Vec<u8>) {
        let (st, mut r) = self.call(5, Args::new().put(fh));
        let _a: post_op_attr = dec(&mut r);
        (st, if st == OK { dec(&mut r) } else { vec![] })
    }

    /// (status, attributes of the linked file)
    pub fn link(&mut self, file: &nfs_fh3, dir: &nfs_fh3, n: &str) -> (u32, Option<fattr3>) {
        let (st, mut r) = self.call(15, Args::new().put(file).put(&dirop(dir, n)));
        (st, attr(dec(&mut r)))
    }

    pub fn remove(&mut self, dir: &nfs_fh3, n: &str) -> u32 {
        self.call(12, Args::new().put(&dirop(dir, n))).0
    }

    pub fn rmdir(&mut self, dir: &nfs_fh3, n: &str) -> u32 {
        self.call(13, Args::new().put(&dirop(dir, n))).0
    }

    pub fn rename(&mut self, fd: &nfs_fh3, from: &str, td: &nfs_fh3, to: &str) -> u32 {
        self.call(14, Args::new().put(&dirop(fd, from)).put(&dirop(td, to)))
            .0
    }

    /// One READDIR or READDIRPLUS page: (status, entries, eof).
    pub fn readdir_page(
        &mut self,
        dir: &nfs_fh3,
        cookie: u64,
        plus: bool,
        dircount: u32,
    ) -> (u32, Vec<Listed>, bool) {
        let mut a = Args::new()
            .put(dir)
            .put(&cookie)
            .put(&[0u8; 8])
            .put(&dircount);
        if plus {
            a = a.put(&(dircount * 8));
        }
        let (st, mut r) = self.call(if plus { 17 } else { 16 }, a);
        let _dir: post_op_attr = dec(&mut r);
        if st != OK {
            return (st, vec![], false);
        }
        let _verf: [u8; 8] = dec(&mut r);
        let mut out = Vec::new();
        while dec::<bool>(&mut r) {
            let fileid: u64 = dec(&mut r);
            let n: filename3 = dec(&mut r);
            let cookie: u64 = dec(&mut r);
            let (attr, fh) = if plus {
                (attr(dec(&mut r)), fh_of(dec(&mut r)))
            } else {
                (None, None)
            };
            out.push(Listed {
                name: String::from_utf8_lossy(&n).into_owned(),
                fileid,
                cookie,
                attr,
                fh,
            });
        }
        (st, out, dec(&mut r))
    }

    /// The whole listing in pages of `dircount` bytes, calling `between` after each page.
    pub fn list_with(
        &mut self,
        dir: &nfs_fh3,
        plus: bool,
        dircount: u32,
        mut between: impl FnMut(&mut Nfs, &[Listed]),
    ) -> Vec<Listed> {
        let mut all: Vec<Listed> = Vec::new();
        let mut cookie = 0;
        for _ in 0..100_000 {
            let (st, page, eof) = self.readdir_page(dir, cookie, plus, dircount);
            assert_eq!(st, OK);
            if let Some(last) = page.last() {
                cookie = last.cookie;
            } else {
                assert!(eof, "empty page without eof");
            }
            between(self, &page);
            all.extend(page);
            if eof {
                return all;
            }
        }
        panic!("listing did not finish");
    }

    pub fn list(&mut self, dir: &nfs_fh3, plus: bool, dircount: u32) -> Vec<Listed> {
        self.list_with(dir, plus, dircount, |_, _| {})
    }

    pub fn names(&mut self, dir: &nfs_fh3) -> Vec<String> {
        let mut v: Vec<String> = self
            .list(dir, true, 4096)
            .into_iter()
            .map(|e| e.name)
            .collect();
        v.sort();
        v
    }

    pub fn access(&mut self, fh: &nfs_fh3, want: u32) -> (u32, u32) {
        let (st, mut r) = self.call(4, Args::new().put(fh).put(&want));
        let _a: post_op_attr = dec(&mut r);
        (st, if st == OK { dec(&mut r) } else { 0 })
    }
}

/// Resident memory of this process in bytes, from `ps`.
pub fn rss_bytes() -> u64 {
    let out = std::process::Command::new("/bin/ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<u64>()
        .unwrap_or(0)
        * 1024
}
