#![allow(dead_code)]

use cowfs_meta::{BlockId, ChunkRef, FileType, Ino, Meta, SetAttr, ROOT_INO};

pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

const NAMES: [&[u8]; 12] = [
    b"a", b"b", b"c", b"d", b"e", b"f", b"g", b"h", b"i", b"j", b"k", b"l",
];

#[derive(Default)]
pub struct WState {
    dirs: Vec<u64>,
    files: Vec<u64>,
    next_snap: u32,
}

impl WState {
    pub fn new() -> Self {
        Self {
            dirs: vec![ROOT_INO.0],
            files: Vec::new(),
            next_snap: 1,
        }
    }
}

fn chunks(rng: &mut Rng) -> Vec<ChunkRef> {
    let n = if rng.below(10) == 0 {
        100 + rng.below(100)
    } else {
        1 + rng.below(3)
    };
    let seed = rng.below(50) as u8;
    (0..n)
        .map(|j| ChunkRef {
            id: BlockId::of(&[seed, j as u8]),
            len: 1 + (j as u32 % 4),
        })
        .collect()
}

/// One random mutation on a random snapshot. Errors are expected and ignored.
pub fn step(m: &Meta, rng: &mut Rng, st: &mut WState) {
    let snaps = m.snapshots().unwrap();
    let info = &snaps[rng.below(snaps.len())];
    let s = m.snapshot(&info.name).unwrap();
    let dir = Ino(st.dirs[rng.below(st.dirs.len())]);
    let name = NAMES[rng.below(NAMES.len())];
    match rng.below(16) {
        0..=2 => {
            if let Ok(a) = s.create(dir, name, 0o644) {
                st.files.push(a.ino.0);
            }
        }
        3 => {
            if let Ok(a) = s.mkdir(dir, name, 0o755) {
                st.dirs.push(a.ino.0);
            }
        }
        4 | 5 => {
            if !st.files.is_empty() {
                let f = Ino(st.files[rng.below(st.files.len())]);
                let cs = chunks(rng);
                let size = cs.iter().map(|c| u64::from(c.len)).sum::<u64>();
                let _ = s.set_content(f, &cs, size);
            }
        }
        6 => {
            if !st.files.is_empty() {
                let f = Ino(st.files[rng.below(st.files.len())]);
                let _ = s.link(f, dir, name);
            }
        }
        7 => {
            let _ = s.unlink(dir, name);
        }
        8 => {
            let td = Ino(st.dirs[rng.below(st.dirs.len())]);
            let tn = NAMES[rng.below(NAMES.len())];
            let _ = s.rename(dir, name, td, tn);
        }
        9 => {
            let _ = s.rmdir(dir, name);
        }
        10 => {
            let _ = s.symlink(dir, name, b"target");
        }
        11 => {
            if !st.files.is_empty() {
                let f = Ino(st.files[rng.below(st.files.len())]);
                if rng.below(3) == 0 {
                    let _ = s.removexattr(f, b"user.x");
                } else {
                    let _ = s.setxattr(f, b"user.x", &rng.next().to_le_bytes());
                }
            }
        }
        12 => {
            if snaps.len() < 4 {
                let n = format!("f{}", st.next_snap);
                st.next_snap += 1;
                let _ = s.fork(&n);
            } else {
                let victim = &snaps[1 + rng.below(snaps.len() - 1)];
                let _ = m.remove_snapshot(victim.id);
            }
        }
        13 => {
            if !st.files.is_empty() {
                let f = Ino(st.files[rng.below(st.files.len())]);
                let _ = s.setattr(
                    f,
                    SetAttr {
                        mode: Some(rng.below(0o1000) as u32),
                        ..SetAttr::default()
                    },
                );
            }
        }
        _ => {
            let _ = s.mkdir(dir, name, 0o700);
        }
    }
}

/// Hash of everything observable about every snapshot, ignoring timestamps.
pub fn digest(m: &Meta) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    for info in m.snapshots().unwrap() {
        h.update(info.name.as_bytes());
        h.update(&info.id.0.to_le_bytes());
        let s = m.snapshot(&info.name).unwrap();
        let mut work = vec![ROOT_INO];
        while let Some(d) = work.pop() {
            let mut cookie = 0;
            loop {
                let page = s.readdir(d, cookie, 64).unwrap();
                for e in &page.entries {
                    let a = s.getattr(e.ino).unwrap();
                    h.update(&d.0.to_le_bytes());
                    h.update(&e.name);
                    h.update(&e.ino.0.to_le_bytes());
                    h.update(&[a.kind as u8]);
                    h.update(&a.mode.to_le_bytes());
                    h.update(&a.nlink.to_le_bytes());
                    h.update(&a.size.to_le_bytes());
                    match a.kind {
                        FileType::File => {
                            for c in s.chunks(e.ino).unwrap() {
                                h.update(c.id.as_bytes());
                                h.update(&c.len.to_le_bytes());
                            }
                        }
                        FileType::Symlink => {
                            h.update(&s.readlink(e.ino).unwrap());
                        }
                        FileType::Dir => work.push(e.ino),
                    }
                    for x in s.listxattr(e.ino).unwrap() {
                        h.update(&x);
                        h.update(&s.getxattr(e.ino, &x).unwrap());
                    }
                }
                cookie = page.next_cookie;
                if page.end {
                    break;
                }
            }
        }
    }
    *h.finalize().as_bytes()
}
