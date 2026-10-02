#![allow(dead_code)]

use cowfs_core::{Core, Options};
use cowfs_vfs::{Attr, Error, Ino, SetAttr, Vfs, ROOT_INO};

pub fn pattern(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8 | 1
        })
        .collect()
}

pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Options for tests: background flusher off unless a test wants it, small pending bound.
pub fn test_opts() -> Options {
    Options {
        background: false,
        ..Options::default()
    }
}

pub struct Fixture {
    pub dir: tempfile::TempDir,
    pub core: Core,
}

pub fn fixture() -> Fixture {
    fixture_with(test_opts())
}

pub fn fixture_with(opts: Options) -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let core = Core::open(dir.path(), opts).expect("open");
    Fixture { dir, core }
}

pub fn write_all(fs: &dyn Vfs, ino: Ino, off: u64, data: &[u8]) {
    let mut o = off;
    for c in data.chunks(1 << 20) {
        assert_eq!(fs.write(ino, o, c).expect("write") as usize, c.len());
        o += c.len() as u64;
    }
}

pub fn read_all(fs: &dyn Vfs, ino: Ino) -> Vec<u8> {
    let size = fs.getattr(ino).expect("getattr").size;
    let mut out = Vec::new();
    while (out.len() as u64) < size {
        let got = fs.read(ino, out.len() as u64, 1 << 20).expect("read");
        if got.is_empty() {
            break;
        }
        out.extend(got);
    }
    out
}

pub fn mkfile(fs: &dyn Vfs, parent: Ino, name: &str, data: &[u8]) -> Attr {
    let a = fs.create(parent, name.as_bytes(), 0o644).expect("create");
    write_all(fs, a.ino, 0, data);
    a
}

pub fn truncate(fs: &dyn Vfs, ino: Ino, size: u64) -> Result<Attr, Error> {
    fs.setattr(
        ino,
        SetAttr {
            size: Some(size),
            ..SetAttr::default()
        },
    )
}

pub fn root_entry(core: &Core, name: &str) -> Attr {
    core.lookup(ROOT_INO, name.as_bytes())
        .expect("snapshot dir")
}
