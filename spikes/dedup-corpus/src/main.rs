use fastcdc::v2020::StreamCDC;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::hash::{BuildHasherDefault, Hasher};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::Mutex;
use std::time::Instant;
use walkdir::WalkDir;

const MIN: usize = 16 * 1024;
const AVG: usize = 64 * 1024;
const MAX: usize = 256 * 1024;
const ZLEVEL: i32 = 3;
const SHARDS: usize = 256;
const CATS: [&str; 4] = ["target", "node_modules", ".git", "source"];
const CHECKPOINT_SECS: u64 = 180;
const MAGIC: &[u8; 8] = b"CFSPIKE1";

type Key = u128;

#[derive(Default)]
struct KeyHasher(u64);
impl Hasher for KeyHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, _: &[u8]) {
        unreachable!()
    }
    fn write_u128(&mut self, i: u128) {
        self.0 = i as u64;
    }
}
type KeyMap<V> = HashMap<Key, V, BuildHasherDefault<KeyHasher>>;

#[derive(Clone, Copy)]
struct ChunkVal {
    len: u32,
    clen: u32,
    pools: u32,
    cats: u8,
}

struct Sharded<V>(Vec<Mutex<KeyMap<V>>>);
impl<V> Sharded<V> {
    fn new() -> Self {
        Sharded((0..SHARDS).map(|_| Mutex::new(KeyMap::default())).collect())
    }
    fn shard(&self, k: Key) -> &Mutex<KeyMap<V>> {
        &self.0[(k >> 120) as usize]
    }
    fn len(&self) -> usize {
        self.0.iter().map(|m| m.lock().unwrap().len()).sum()
    }
}

struct Global {
    chunks: Sharded<ChunkVal>,
    files: Sharded<u64>,
    inodes: Mutex<HashSet<(u64, u64)>>,
}

#[derive(Default, Serialize, Deserialize, Clone)]
struct SlotStats {
    name: String,
    pool: usize,
    files: u64,
    raw_bytes: u64,
    perfile_comp_bytes: u64,
    chunks: u64,
    new_chunks: u64,
    new_raw: u64,
    new_comp: u64,
    whole_file_new_raw: u64,
    symlinks: u64,
    hardlink_dupes: u64,
    special_skipped: u64,
    walk_errors: u64,
    read_errors: u64,
    changed_during_read: u64,
    cat_raw: [u64; 4],
    secs: f64,
    error_samples: Vec<String>,
}

#[derive(Default)]
struct Acc {
    files: AtomicU64,
    raw_bytes: AtomicU64,
    perfile_comp_bytes: AtomicU64,
    chunks: AtomicU64,
    new_chunks: AtomicU64,
    new_raw: AtomicU64,
    new_comp: AtomicU64,
    whole_file_new_raw: AtomicU64,
    hardlink_dupes: AtomicU64,
    read_errors: AtomicU64,
    changed_during_read: AtomicU64,
    cat_raw: [AtomicU64; 4],
    error_samples: Mutex<Vec<String>>,
}

#[derive(Serialize, Deserialize)]
struct Header {
    pools: Vec<String>,
    done: Vec<SlotStats>,
}

struct CountSink(u64);
impl Write for CountSink {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0 += b.len() as u64;
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn key_of(h: &blake3::Hash) -> Key {
    u128::from_le_bytes(h.as_bytes()[..16].try_into().unwrap())
}

fn classify(rel: &Path) -> usize {
    let mut cat = 3;
    for c in rel.components() {
        match c.as_os_str().to_str() {
            Some("target") => return 0,
            Some("node_modules") => return 1,
            Some(".git") => cat = 2,
            _ => {}
        }
    }
    cat
}

fn process_file(path: &Path, cat: usize, pool_bit: u32, g: &Global, a: &Acc) -> io::Result<()> {
    let f = File::open(path)?;
    let expected = f.metadata()?.len();
    let mut fh = blake3::Hasher::new();
    let mut enc = zstd::stream::write::Encoder::new(CountSink(0), ZLEVEL)?;
    let mut read = 0u64;
    let (mut chunks, mut new_chunks, mut new_raw, mut new_comp) = (0u64, 0u64, 0u64, 0u64);
    for c in StreamCDC::new(BufReader::with_capacity(1 << 20, f), MIN, AVG, MAX) {
        let c = c.map_err(|e| io::Error::other(e.to_string()))?;
        let data = &c.data;
        read += data.len() as u64;
        fh.update(data);
        enc.write_all(data)?;
        let k = key_of(&blake3::hash(data));
        let shard = g.chunks.shard(k);
        let mut seen = false;
        {
            let mut m = shard.lock().unwrap();
            if let Some(v) = m.get_mut(&k) {
                v.pools |= pool_bit;
                v.cats |= 1 << cat;
                seen = true;
            }
        }
        chunks += 1;
        if seen {
            continue;
        }
        let clen = zstd::bulk::compress(data, ZLEVEL)?.len() as u32;
        let mut m = shard.lock().unwrap();
        match m.entry(k) {
            std::collections::hash_map::Entry::Occupied(mut e) => {
                let v = e.get_mut();
                v.pools |= pool_bit;
                v.cats |= 1 << cat;
            }
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(ChunkVal { len: data.len() as u32, clen, pools: pool_bit, cats: 1 << cat });
                new_chunks += 1;
                new_raw += data.len() as u64;
                new_comp += clen as u64;
            }
        }
    }
    let comp = enc.finish()?.0;
    if read != expected {
        a.changed_during_read.fetch_add(1, Relaxed);
    }
    let fk = key_of(&fh.finalize());
    if g.files.shard(fk).lock().unwrap().insert(fk, read).is_none() {
        a.whole_file_new_raw.fetch_add(read, Relaxed);
    }
    a.files.fetch_add(1, Relaxed);
    a.raw_bytes.fetch_add(read, Relaxed);
    a.cat_raw[cat].fetch_add(read, Relaxed);
    a.perfile_comp_bytes.fetch_add(comp, Relaxed);
    a.chunks.fetch_add(chunks, Relaxed);
    a.new_chunks.fetch_add(new_chunks, Relaxed);
    a.new_raw.fetch_add(new_raw, Relaxed);
    a.new_comp.fetch_add(new_comp, Relaxed);
    Ok(())
}

fn process_slot(name: &str, pool: usize, root: &Path, g: &Global) -> SlotStats {
    let t0 = Instant::now();
    let mut s = SlotStats { name: name.to_string(), pool, ..Default::default() };
    let mut files: Vec<(PathBuf, usize)> = Vec::new();
    for e in WalkDir::new(root).follow_links(false) {
        let e = match e {
            Ok(e) => e,
            Err(_) => {
                s.walk_errors += 1;
                continue;
            }
        };
        let ft = e.file_type();
        if ft.is_dir() {
            continue;
        }
        if ft.is_symlink() {
            s.symlinks += 1;
            continue;
        }
        if !ft.is_file() {
            s.special_skipped += 1;
            continue;
        }
        let md = match e.metadata() {
            Ok(m) => m,
            Err(_) => {
                s.walk_errors += 1;
                continue;
            }
        };
        if md.nlink() > 1 && !g.inodes.lock().unwrap().insert((md.dev(), md.ino())) {
            s.hardlink_dupes += 1;
            continue;
        }
        let rel = e.path().strip_prefix(root).unwrap_or(e.path());
        files.push((e.path().to_path_buf(), classify(rel)));
    }
    let acc = Acc::default();
    let pool_bit = 1u32 << pool;
    files.par_iter().for_each(|(p, cat)| {
        let mut r = process_file(p, *cat, pool_bit, g, &acc);
        if r.is_err() {
            std::thread::sleep(std::time::Duration::from_millis(50));
            r = process_file(p, *cat, pool_bit, g, &acc);
        }
        if let Err(e) = r {
            acc.read_errors.fetch_add(1, Relaxed);
            let mut v = acc.error_samples.lock().unwrap();
            if v.len() < 10 {
                v.push(format!("{}: {}", p.display(), e));
            }
        }
    });
    s.files = acc.files.load(Relaxed);
    s.raw_bytes = acc.raw_bytes.load(Relaxed);
    s.perfile_comp_bytes = acc.perfile_comp_bytes.load(Relaxed);
    s.chunks = acc.chunks.load(Relaxed);
    s.new_chunks = acc.new_chunks.load(Relaxed);
    s.new_raw = acc.new_raw.load(Relaxed);
    s.new_comp = acc.new_comp.load(Relaxed);
    s.whole_file_new_raw = acc.whole_file_new_raw.load(Relaxed);
    s.hardlink_dupes += acc.hardlink_dupes.load(Relaxed);
    s.read_errors = acc.read_errors.load(Relaxed);
    s.changed_during_read = acc.changed_during_read.load(Relaxed);
    for i in 0..4 {
        s.cat_raw[i] = acc.cat_raw[i].load(Relaxed);
    }
    s.error_samples = acc.error_samples.into_inner().unwrap();
    s.secs = t0.elapsed().as_secs_f64();
    s
}

fn w64(w: &mut impl Write, v: u64) -> io::Result<()> {
    w.write_all(&v.to_le_bytes())
}
fn r_buf<const N: usize>(r: &mut impl Read) -> io::Result<[u8; N]> {
    let mut b = [0u8; N];
    r.read_exact(&mut b)?;
    Ok(b)
}
fn r64(r: &mut impl Read) -> io::Result<u64> {
    Ok(u64::from_le_bytes(r_buf(r)?))
}
fn r32(r: &mut impl Read) -> io::Result<u32> {
    Ok(u32::from_le_bytes(r_buf(r)?))
}

fn save(path: &Path, h: &Header, g: &Global) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    let mut w = BufWriter::with_capacity(1 << 20, File::create(&tmp)?);
    w.write_all(MAGIC)?;
    let j = serde_json::to_vec(h).map_err(io::Error::other)?;
    w64(&mut w, j.len() as u64)?;
    w.write_all(&j)?;
    w64(&mut w, g.chunks.len() as u64)?;
    for s in &g.chunks.0 {
        for (k, v) in s.lock().unwrap().iter() {
            w.write_all(&k.to_le_bytes())?;
            w.write_all(&v.len.to_le_bytes())?;
            w.write_all(&v.clen.to_le_bytes())?;
            w.write_all(&v.pools.to_le_bytes())?;
            w.write_all(&[v.cats])?;
        }
    }
    w64(&mut w, g.files.len() as u64)?;
    for s in &g.files.0 {
        for (k, v) in s.lock().unwrap().iter() {
            w.write_all(&k.to_le_bytes())?;
            w64(&mut w, *v)?;
        }
    }
    let ino = g.inodes.lock().unwrap();
    w64(&mut w, ino.len() as u64)?;
    for (d, i) in ino.iter() {
        w64(&mut w, *d)?;
        w64(&mut w, *i)?;
    }
    drop(ino);
    w.flush()?;
    w.get_ref().sync_all()?;
    fs::rename(tmp, path)
}

fn load(path: &Path) -> io::Result<(Header, Global)> {
    let mut r = BufReader::with_capacity(1 << 20, File::open(path)?);
    if r_buf::<8>(&mut r)? != *MAGIC {
        return Err(io::Error::other("bad magic"));
    }
    let n = r64(&mut r)? as usize;
    let mut j = vec![0u8; n];
    r.read_exact(&mut j)?;
    let h: Header = serde_json::from_slice(&j).map_err(io::Error::other)?;
    let g = Global { chunks: Sharded::new(), files: Sharded::new(), inodes: Mutex::new(HashSet::new()) };
    for _ in 0..r64(&mut r)? {
        let k = u128::from_le_bytes(r_buf(&mut r)?);
        let v = ChunkVal {
            len: r32(&mut r)?,
            clen: r32(&mut r)?,
            pools: r32(&mut r)?,
            cats: r_buf::<1>(&mut r)?[0],
        };
        g.chunks.shard(k).lock().unwrap().insert(k, v);
    }
    for _ in 0..r64(&mut r)? {
        let k = u128::from_le_bytes(r_buf(&mut r)?);
        let v = r64(&mut r)?;
        g.files.shard(k).lock().unwrap().insert(k, v);
    }
    let mut ino = g.inodes.lock().unwrap();
    for _ in 0..r64(&mut r)? {
        let d = r64(&mut r)?;
        let i = r64(&mut r)?;
        ino.insert((d, i));
    }
    drop(ino);
    Ok((h, g))
}

fn slot_order(name: &str) -> (u64, String) {
    (name.parse::<u64>().unwrap_or(u64::MAX), name.to_string())
}

fn gib(b: u64) -> f64 {
    b as f64 / (1u64 << 30) as f64
}

#[derive(Serialize)]
struct PoolReport {
    pool: String,
    slots: usize,
    raw_bytes: u64,
    perfile_comp_bytes: u64,
    unique_within_pool_raw: u64,
    unique_within_pool_comp: u64,
}

#[derive(Serialize)]
struct Report {
    slots: usize,
    files: u64,
    raw_bytes: u64,
    baseline_perfile_zstd_bytes: u64,
    baseline_whole_file_dedup_bytes: u64,
    cdc_unique_raw_bytes: u64,
    cdc_unique_comp_bytes: u64,
    chunks_total: u64,
    chunks_unique: u64,
    avg_unique_chunk_bytes: f64,
    ratio_perfile_zstd: f64,
    ratio_whole_file_dedup: f64,
    ratio_cdc_raw: f64,
    ratio_cdc_zstd: f64,
    symlinks_skipped: u64,
    hardlink_dupes_skipped: u64,
    special_skipped: u64,
    walk_errors: u64,
    read_errors: u64,
    changed_during_read: u64,
    per_pool: Vec<PoolReport>,
    per_category: Vec<(String, u64, u64, u64)>,
    sharing_by_pool_count: Vec<(u32, u64)>,
    cross_pool_savings_raw: u64,
}

fn report(h: &Header, g: &Global) -> Report {
    let mut uraw = 0u64;
    let mut ucomp = 0u64;
    let mut uchunks = 0u64;
    let np = h.pools.len();
    let mut p_raw = vec![0u64; np];
    let mut p_comp = vec![0u64; np];
    let mut c_raw = [0u64; 4];
    let mut c_comp = [0u64; 4];
    let mut share = [0u64; 33];
    for s in &g.chunks.0 {
        for v in s.lock().unwrap().values() {
            uchunks += 1;
            uraw += v.len as u64;
            ucomp += v.clen as u64;
            for p in 0..np {
                if v.pools & (1 << p) != 0 {
                    p_raw[p] += v.len as u64;
                    p_comp[p] += v.clen as u64;
                }
            }
            for c in 0..4 {
                if v.cats & (1 << c) != 0 {
                    c_raw[c] += v.len as u64;
                    c_comp[c] += v.clen as u64;
                }
            }
            share[v.pools.count_ones() as usize] += v.len as u64;
        }
    }
    let mut r = Report {
        slots: h.done.len(),
        files: 0,
        raw_bytes: 0,
        baseline_perfile_zstd_bytes: 0,
        baseline_whole_file_dedup_bytes: 0,
        cdc_unique_raw_bytes: uraw,
        cdc_unique_comp_bytes: ucomp,
        chunks_total: 0,
        chunks_unique: uchunks,
        avg_unique_chunk_bytes: uraw as f64 / uchunks.max(1) as f64,
        ratio_perfile_zstd: 0.0,
        ratio_whole_file_dedup: 0.0,
        ratio_cdc_raw: 0.0,
        ratio_cdc_zstd: 0.0,
        symlinks_skipped: 0,
        hardlink_dupes_skipped: 0,
        special_skipped: 0,
        walk_errors: 0,
        read_errors: 0,
        changed_during_read: 0,
        per_pool: vec![],
        per_category: vec![],
        sharing_by_pool_count: vec![],
        cross_pool_savings_raw: 0,
    };
    let mut cat_raw = [0u64; 4];
    let mut pool_totals = vec![(0usize, 0u64, 0u64); np];
    for s in &h.done {
        r.files += s.files;
        r.raw_bytes += s.raw_bytes;
        r.baseline_perfile_zstd_bytes += s.perfile_comp_bytes;
        r.baseline_whole_file_dedup_bytes += s.whole_file_new_raw;
        r.chunks_total += s.chunks;
        r.symlinks_skipped += s.symlinks;
        r.hardlink_dupes_skipped += s.hardlink_dupes;
        r.special_skipped += s.special_skipped;
        r.walk_errors += s.walk_errors;
        r.read_errors += s.read_errors;
        r.changed_during_read += s.changed_during_read;
        for i in 0..4 {
            cat_raw[i] += s.cat_raw[i];
        }
        let t = &mut pool_totals[s.pool];
        t.0 += 1;
        t.1 += s.raw_bytes;
        t.2 += s.perfile_comp_bytes;
    }
    let ratio = |n: u64, d: u64| n as f64 / d.max(1) as f64;
    r.ratio_perfile_zstd = ratio(r.raw_bytes, r.baseline_perfile_zstd_bytes);
    r.ratio_whole_file_dedup = ratio(r.raw_bytes, r.baseline_whole_file_dedup_bytes);
    r.ratio_cdc_raw = ratio(r.raw_bytes, uraw);
    r.ratio_cdc_zstd = ratio(r.raw_bytes, ucomp);
    for p in 0..np {
        r.per_pool.push(PoolReport {
            pool: h.pools[p].clone(),
            slots: pool_totals[p].0,
            raw_bytes: pool_totals[p].1,
            perfile_comp_bytes: pool_totals[p].2,
            unique_within_pool_raw: p_raw[p],
            unique_within_pool_comp: p_comp[p],
        });
    }
    let sum_pool_unique: u64 = p_raw.iter().sum();
    r.cross_pool_savings_raw = sum_pool_unique.saturating_sub(uraw);
    for c in 0..4 {
        r.per_category.push((CATS[c].to_string(), cat_raw[c], c_raw[c], c_comp[c]));
    }
    for (n, b) in share.iter().enumerate() {
        if *b > 0 {
            r.sharing_by_pool_count.push((n as u32, *b));
        }
    }
    r
}

fn print_report(r: &Report) {
    println!("slots={} files={} raw={:.2} GiB", r.slots, r.files, gib(r.raw_bytes));
    println!("baseline per-file zstd-{ZLEVEL}   : {:.2} GiB  ratio {:.2}x", gib(r.baseline_perfile_zstd_bytes), r.ratio_perfile_zstd);
    println!("baseline whole-file dedup only   : {:.2} GiB  ratio {:.2}x", gib(r.baseline_whole_file_dedup_bytes), r.ratio_whole_file_dedup);
    println!("CDC dedup only (raw)             : {:.2} GiB  ratio {:.2}x", gib(r.cdc_unique_raw_bytes), r.ratio_cdc_raw);
    println!("CDC dedup + zstd-{ZLEVEL} (cowfs)   : {:.2} GiB  ratio {:.2}x", gib(r.cdc_unique_comp_bytes), r.ratio_cdc_zstd);
    println!("chunks total={} unique={} avg_unique={:.0} B", r.chunks_total, r.chunks_unique, r.avg_unique_chunk_bytes);
    println!("skipped: symlinks={} hardlink_dupes={} special={} walk_err={} read_err={} changed_mid_read={}", r.symlinks_skipped, r.hardlink_dupes_skipped, r.special_skipped, r.walk_errors, r.read_errors, r.changed_during_read);
    println!("cross-pool savings (raw)         : {:.2} GiB", gib(r.cross_pool_savings_raw));
    println!("-- per pool: slots raw_GiB perfile_zstd_GiB within_pool_unique_GiB within_pool_unique_zstd_GiB");
    for p in &r.per_pool {
        println!("{:<28} {:>3} {:>9.2} {:>9.2} {:>9.2} {:>9.2}", p.pool, p.slots, gib(p.raw_bytes), gib(p.perfile_comp_bytes), gib(p.unique_within_pool_raw), gib(p.unique_within_pool_comp));
    }
    println!("-- per category: raw_GiB unique_raw_GiB unique_zstd_GiB");
    for (n, raw, ur, uc) in &r.per_category {
        println!("{:<14} {:>9.2} {:>9.2} {:>9.2}", n, gib(*raw), gib(*ur), gib(*uc));
    }
    println!("-- unique bytes by number of pools sharing the chunk");
    for (n, b) in &r.sharing_by_pool_count {
        println!("{:>2} pools: {:.2} GiB", n, gib(*b));
    }
}

fn main() -> io::Result<()> {
    let mut out = PathBuf::from("out");
    let mut pools: Vec<(String, PathBuf, bool)> = Vec::new();
    let mut max_slots = usize::MAX;
    let mut resume = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = PathBuf::from(args.next().unwrap()),
            "--pool" | "--project" => {
                let p = PathBuf::from(args.next().unwrap());
                let base = p.file_name().unwrap().to_string_lossy().to_string();
                let is_project = a == "--project";
                pools.push((if is_project { format!("proj:{base}") } else { base }, p, is_project));
            }
            "--max-slots-per-pool" => max_slots = args.next().unwrap().parse().unwrap(),
            "--resume" => resume = true,
            _ => panic!("unknown arg {a}"),
        }
    }
    assert!(pools.len() <= 32, "at most 32 pools");
    fs::create_dir_all(&out)?;
    let ckpt = out.join("index.bin");
    let names: Vec<String> = pools.iter().map(|p| p.0.clone()).collect();
    let (mut header, g) = if resume && ckpt.exists() {
        let (h, g) = load(&ckpt)?;
        assert_eq!(h.pools, names, "pool list differs from checkpoint");
        eprintln!("resumed: {} slots done, {} unique chunks", h.done.len(), g.chunks.len());
        (h, g)
    } else {
        (
            Header { pools: names, done: vec![] },
            Global { chunks: Sharded::new(), files: Sharded::new(), inodes: Mutex::new(HashSet::new()) },
        )
    };
    let mut log = OpenOptions::new().create(true).append(true).open(out.join("slots.jsonl"))?;
    let mut last_ckpt = Instant::now();
    let done: HashSet<String> = header.done.iter().map(|s| s.name.clone()).collect();
    let t0 = Instant::now();
    for (pi, (pname, path, is_project)) in pools.iter().enumerate() {
        let mut slots: Vec<(String, PathBuf)> = if *is_project {
            vec![(format!("{pname}/-"), path.clone())]
        } else {
            let mut v: Vec<String> = fs::read_dir(path)?
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect();
            v.sort_by_key(|n| slot_order(n));
            v.truncate(max_slots);
            v.into_iter().map(|s| (format!("{pname}/{s}"), path.join(&s))).collect()
        };
        slots.retain(|(n, _)| !done.contains(n));
        for (name, root) in slots {
            let s = process_slot(&name, pi, &root, &g);
            eprintln!(
                "[{:>7.0}s] {:<32} files={:>7} raw={:>7.2}GiB new_comp={:>7.2}GiB errs={} {:.0}MiB/s",
                t0.elapsed().as_secs_f64(),
                s.name,
                s.files,
                gib(s.raw_bytes),
                gib(s.new_comp),
                s.read_errors + s.walk_errors,
                s.raw_bytes as f64 / (1 << 20) as f64 / s.secs.max(0.001)
            );
            writeln!(log, "{}", serde_json::to_string(&s).unwrap())?;
            log.flush()?;
            header.done.push(s);
            if last_ckpt.elapsed().as_secs() >= CHECKPOINT_SECS {
                save(&ckpt, &header, &g)?;
                last_ckpt = Instant::now();
            }
        }
    }
    save(&ckpt, &header, &g)?;
    let r = report(&header, &g);
    fs::write(out.join("report.json"), serde_json::to_vec_pretty(&r).unwrap())?;
    print_report(&r);
    Ok(())
}
