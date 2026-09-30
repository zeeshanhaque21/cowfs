//! Recording storage backend shared by the crash tests.

use redb::StorageBackend;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug)]
pub enum Ev {
    W(u64, Vec<u8>),
    L(u64),
    S(usize),
}

#[derive(Default, Debug)]
pub struct Rec {
    pub data: Vec<u8>,
    pub log: Vec<Ev>,
}

#[derive(Clone, Default, Debug)]
pub struct Be {
    pub r: Arc<Mutex<Rec>>,
    pub tag: Arc<AtomicUsize>,
    pub flaky: Arc<AtomicBool>,
    pub reads: Arc<AtomicUsize>,
    pub flip_every: usize,
}

pub fn apply(img: &mut Vec<u8>, ev: &Ev) {
    match ev {
        Ev::W(off, d) => {
            let end = *off as usize + d.len();
            if img.len() < end {
                img.resize(end, 0);
            }
            img[*off as usize..end].copy_from_slice(d);
        }
        Ev::L(n) => img.resize(*n as usize, 0),
        Ev::S(_) => {}
    }
}

impl Be {
    pub fn from_image(img: Vec<u8>) -> Self {
        let b = Be::default();
        b.r.lock().unwrap().data = img;
        b
    }
    pub fn log(&self) -> Vec<Ev> {
        self.r.lock().unwrap().log.clone()
    }
    pub fn image(&self) -> Vec<u8> {
        self.r.lock().unwrap().data.clone()
    }
}

impl StorageBackend for Be {
    fn len(&self) -> Result<u64, io::Error> {
        Ok(self.r.lock().unwrap().data.len() as u64)
    }
    fn read(&self, offset: u64, out: &mut [u8]) -> Result<(), io::Error> {
        let r = self.r.lock().unwrap();
        let end = offset as usize + out.len();
        if end > r.data.len() {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        out.copy_from_slice(&r.data[offset as usize..end]);
        if self.flaky.load(SeqCst) && self.flip_every > 0 {
            let n = self.reads.fetch_add(1, SeqCst);
            if n.is_multiple_of(self.flip_every) && !out.is_empty() {
                let i = (n / self.flip_every * 7919) % out.len();
                out[i] ^= 1 << (n % 8);
            }
        }
        Ok(())
    }
    fn set_len(&self, len: u64) -> Result<(), io::Error> {
        let mut r = self.r.lock().unwrap();
        let ev = Ev::L(len);
        apply(&mut r.data, &ev);
        r.log.push(ev);
        Ok(())
    }
    fn sync_data(&self) -> Result<(), io::Error> {
        let t = self.tag.load(SeqCst);
        self.r.lock().unwrap().log.push(Ev::S(t));
        Ok(())
    }
    fn write(&self, offset: u64, data: &[u8]) -> Result<(), io::Error> {
        let mut r = self.r.lock().unwrap();
        let ev = Ev::W(offset, data.to_vec());
        apply(&mut r.data, &ev);
        r.log.push(ev);
        Ok(())
    }
}
