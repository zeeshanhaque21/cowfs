//! `Meta::check`: recomputes every invariant from the stored rows, in one pass per snapshot.
//!
//! Memory is about 40 bytes per inode plus about 50 bytes per tree node (counters, not data), so
//! a million inodes need tens of megabytes. A directory's entries are checked with a streaming
//! multiset hash, not by holding them.

use crate::db::{
    decode_snap, meta_get, Inner, FORMAT_VERSION, MAGIC, META, NODES, REAP, REFS, SNAPSHOTS,
    SNAP_NAMES,
};
use crate::node::NodeId;
use crate::ptree::{Cursor, IdHasher, NodeSource, TableSource};
use crate::types::*;
use crate::{Error, Result};
use redb::{ReadableDatabase, ReadableTable, ReadableTableMetadata};
use std::collections::HashMap;
use std::hash::BuildHasherDefault;

const MAX_REPORTED: usize = 50;

#[derive(Default)]
struct Errs(Vec<String>);

impl Errs {
    fn push(&mut self, msg: String) {
        if self.0.len() < MAX_REPORTED {
            self.0.push(msg);
        }
    }
}

type NodeMap<V> = HashMap<NodeId, V, BuildHasherDefault<IdHasher>>;

pub(crate) fn check(inner: &Inner) -> Result<()> {
    let rtx = inner.db.begin_read()?;
    let nodes_t = rtx.open_table(NODES)?;
    let refs = rtx.open_table(REFS)?;
    let snaps = rtx.open_table(SNAPSHOTS)?;
    let names = rtx.open_table(SNAP_NAMES)?;
    let meta = rtx.open_table(META)?;
    let reap = rtx.open_table(REAP)?;
    let mut errs = Errs::default();
    let src = TableSource {
        table: &nodes_t,
        cache: None,
    };

    if meta.get("magic")?.map(|g| g.value()) != Some(MAGIC) {
        errs.push("missing or wrong magic".into());
    }
    if meta_get(&meta, "version")? != FORMAT_VERSION {
        errs.push("unsupported format version".into());
    }
    let reserved = meta_get(&meta, "ino_reserved")?;
    let next_snapshot = meta_get(&meta, "next_snapshot")?;
    let next_reap = meta_get(&meta, "next_reap")?;
    if !(2..=INO_LIMIT).contains(&reserved) {
        errs.push(format!("ino_reserved {reserved} outside 2..={INO_LIMIT}"));
    }

    let mut infos = Vec::new();
    for r in snaps.iter()? {
        let (k, v) = r?;
        let info = decode_snap(k.value(), v.value())?;
        if info.id.0 >= next_snapshot {
            errs.push(format!("snapshot {} at or above next_snapshot", info.id.0));
        }
        infos.push(info);
    }
    let mut name_rows = 0u64;
    for r in names.iter()? {
        let (k, v) = r?;
        name_rows += 1;
        match infos.iter().find(|i| i.id.0 == v.value()) {
            Some(i) if i.name == k.value() => {}
            _ => errs.push(format!(
                "snapshot name {:?} does not match its row",
                k.value()
            )),
        }
    }
    if name_rows != infos.len() as u64 {
        errs.push(format!(
            "{} snapshot rows but {name_rows} name rows",
            infos.len()
        ));
    }

    let mut roots: Vec<NodeId> = infos.iter().map(|i| i.root).collect();
    let mut reap_rows = 0u64;
    for r in reap.iter()? {
        let (k, v) = r?;
        reap_rows += 1;
        if k.value() >= next_reap {
            errs.push(format!("reap entry {} at or above next_reap", k.value()));
        }
        roots.push(NodeId::from_bytes(v.value()));
    }
    let _ = reap_rows;

    let mut graph: NodeMap<(u32, u64)> = NodeMap::default();
    for root in &roots {
        graph.entry(*root).or_insert((u32::MAX, 0)).1 += 1;
    }
    for root in &roots {
        walk(&src, root, &mut graph, &mut errs)?;
    }
    for r in refs.iter()? {
        let (k, v) = r?;
        let id = NodeId::from_bytes(k.value());
        match graph.get(&id) {
            Some(&(_, n)) if n == v.value() => {}
            Some(&(_, n)) => errs.push(format!(
                "node {id} has count {} but {n} references",
                v.value()
            )),
            None => errs.push(format!("count stored for unreachable node {id}")),
        }
    }
    if refs.len()? != graph.len() as u64 {
        errs.push(format!(
            "{} counts stored for {} reachable nodes",
            refs.len()?,
            graph.len()
        ));
    }
    if nodes_t.len()? != graph.len() as u64 {
        errs.push(format!(
            "{} nodes stored but {} reachable",
            nodes_t.len()?,
            graph.len()
        ));
    }
    drop(graph);

    for info in &infos {
        let mut e = Errs::default();
        check_snapshot(&src, &info.root, reserved, &mut e)?;
        for m in e.0 {
            errs.push(format!("snapshot {:?}: {m}", info.name));
        }
    }

    if errs.0.is_empty() {
        Ok(())
    } else {
        Err(Error::Inconsistent(errs.0))
    }
}

fn walk(
    src: &dyn NodeSource,
    id: &NodeId,
    graph: &mut NodeMap<(u32, u64)>,
    errs: &mut Errs,
) -> Result<u32> {
    if let Some(&(h, _)) = graph.get(id) {
        if h != u32::MAX {
            return Ok(h);
        }
    }
    let n = src.node(id)?;
    let mut height = 0;
    if n.is_leaf() {
        for i in 1..n.len() {
            if n.key(i - 1) >= n.key(i) {
                errs.push(format!("leaf {id} keys not strictly increasing"));
                break;
            }
        }
    } else {
        if n.len() == 0 {
            errs.push(format!("internal node {id} is empty"));
        }
        for i in 2..n.len() {
            if n.key(i - 1) >= n.key(i) {
                errs.push(format!("internal node {id} separators not increasing"));
                break;
            }
        }
        let mut first = None;
        for i in 0..n.len() {
            let c = n.child(i);
            graph.entry(c).or_insert((u32::MAX, 0)).1 += 1;
            let h = walk(src, &c, graph, errs)?;
            if *first.get_or_insert(h) != h {
                errs.push(format!("node {id} has children of different heights"));
            }
        }
        height = first.map_or(0, |h| h + 1);
    }
    graph.entry(*id).or_insert((u32::MAX, 0)).0 = height;
    Ok(height)
}

struct Group {
    ino: u64,
    rec: Option<InodeRec>,
    name_rows: u64,
    cookie_rows: u64,
    name_sum: u64,
    cookie_sum: u64,
    next_off: u64,
    extents: u64,
    link: Option<usize>,
}

impl Group {
    fn new(ino: u64) -> Self {
        Self {
            ino,
            rec: None,
            name_rows: 0,
            cookie_rows: 0,
            name_sum: 0,
            cookie_sum: 0,
            next_off: 0,
            extents: 0,
            link: None,
        }
    }
}

#[derive(Default)]
struct Info {
    kind: u8,
    nlink: u32,
    parent: u64,
    names: u32,
    subdirs: u32,
    dirent_parent: u64,
    declared: u8,
    mismatch: bool,
    seen: bool,
}

fn kind_code(k: FileType) -> u8 {
    match k {
        FileType::File => 1,
        FileType::Dir => 2,
        FileType::Symlink => 3,
    }
}

fn entry_hash(cookie: u64, child: u64, kind: FileType, name: &[u8]) -> u64 {
    let mut h = blake3::Hasher::new();
    h.update(&cookie.to_le_bytes());
    h.update(&child.to_le_bytes());
    h.update(&[kind_code(kind)]);
    h.update(name);
    u64::from_le_bytes(h.finalize().as_bytes()[..8].try_into().unwrap_or([0; 8]))
}

fn finish(g: Group, reserved: u64, infos: &mut HashMap<u64, Info>, errs: &mut Errs) {
    let ino = g.ino;
    if ino == 0 || ino >= reserved {
        errs.push(format!("inode number {ino} outside 1..{reserved}"));
    }
    let Some(rec) = g.rec else {
        errs.push(format!("records for inode {ino} but no inode record"));
        return;
    };
    let dir = rec.kind == FileType::Dir;
    if !dir && (g.name_rows != 0 || g.cookie_rows != 0) {
        errs.push(format!("non-directory inode {ino} has directory entries"));
    }
    if g.name_rows != g.cookie_rows || g.name_sum != g.cookie_sum {
        errs.push(format!(
            "directory {ino} name rows and cookie rows disagree"
        ));
    }
    match rec.kind {
        FileType::File => {
            if g.link.is_some() {
                errs.push(format!("file {ino} has a link target"));
            }
            if g.next_off != rec.covered {
                errs.push(format!(
                    "file {ino} extents cover {} bytes but covered is {}",
                    g.next_off, rec.covered
                ));
            }
            if rec.covered > rec.size {
                errs.push(format!(
                    "file {ino} covers {} bytes but size is {}",
                    rec.covered, rec.size
                ));
            }
        }
        FileType::Symlink => {
            if g.extents != 0 || rec.covered != 0 {
                errs.push(format!("symlink {ino} has chunks"));
            }
            if g.link != Some(rec.size as usize) || rec.size == 0 {
                errs.push(format!("symlink {ino} target does not match its size"));
            }
        }
        FileType::Dir => {
            if g.extents != 0 || g.link.is_some() || rec.size != 0 || rec.covered != 0 {
                errs.push(format!("directory {ino} has file data"));
            }
        }
    }
    let e = infos.entry(ino).or_default();
    e.kind = kind_code(rec.kind);
    e.nlink = rec.nlink;
    e.parent = rec.parent;
    e.seen = true;
}

fn check_snapshot(
    src: &TableSource<'_>,
    root: &NodeId,
    reserved: u64,
    errs: &mut Errs,
) -> Result<()> {
    let mut infos: HashMap<u64, Info> = HashMap::new();
    let mut cur = Cursor::seek(src, root, &[])?;
    let mut prev: Option<Vec<u8>> = None;
    let mut group: Option<Group> = None;
    while let Some((k, v)) = cur.next(src)? {
        if prev.as_ref().is_some_and(|p| *p >= k) {
            errs.push("keys out of order across the tree".into());
        }
        let (ino, kind, suffix) = match split_key(&k) {
            Ok(x) => x,
            Err(e) => {
                errs.push(e.to_string());
                continue;
            }
        };
        if group.as_ref().is_some_and(|g| g.ino != ino.0) {
            if let Some(g) = group.take() {
                finish(g, reserved, &mut infos, errs);
            }
        }
        let g = group.get_or_insert_with(|| Group::new(ino.0));
        match kind {
            K_INODE => match InodeRec::decode(&v) {
                Ok(r) => g.rec = Some(r),
                Err(e) => errs.push(format!("inode {}: inode record: {e}", ino.0)),
            },
            K_NAME => match decode_name_val(&v) {
                Ok((c, t, cookie)) => {
                    g.name_rows += 1;
                    g.name_sum = g.name_sum.wrapping_add(entry_hash(cookie, c.0, t, suffix));
                    match g.rec {
                        Some(r) if cookie == 0 || cookie >= r.next_cookie => {
                            errs.push(format!(
                                "directory {} cookie {cookie} outside next_cookie",
                                ino.0
                            ));
                        }
                        None => errs.push(format!(
                            "directory {} entries before its inode record",
                            ino.0
                        )),
                        _ => {}
                    }
                    let e = infos.entry(c.0).or_default();
                    e.names += 1;
                    let code = kind_code(t);
                    if e.declared != 0 && e.declared != code {
                        e.mismatch = true;
                    }
                    e.declared = code;
                    if t == FileType::Dir {
                        e.dirent_parent = ino.0;
                        infos.entry(ino.0).or_default().subdirs += 1;
                    }
                }
                Err(e) => errs.push(format!("inode {}: dirent: {e}", ino.0)),
            },
            K_COOKIE => match (decode_cookie_val(&v), <[u8; 8]>::try_from(suffix)) {
                (Ok((c, t, n)), Ok(ck)) => {
                    g.cookie_rows += 1;
                    g.cookie_sum =
                        g.cookie_sum
                            .wrapping_add(entry_hash(u64::from_be_bytes(ck), c.0, t, n));
                }
                _ => errs.push(format!("inode {}: bad cookie row", ino.0)),
            },
            K_XATTR => {}
            K_CHUNK => match (decode_chunks(&v), <[u8; 8]>::try_from(suffix)) {
                (Ok(c), Ok(off)) if c.len() == 1 => {
                    if u64::from_be_bytes(off) != g.next_off {
                        errs.push(format!(
                            "inode {}: extent at {} but expected {}",
                            ino.0,
                            u64::from_be_bytes(off),
                            g.next_off
                        ));
                    }
                    g.extents += 1;
                    g.next_off = g.next_off.saturating_add(u64::from(c[0].len));
                }
                _ => errs.push(format!("inode {}: bad extent", ino.0)),
            },
            K_LINK => g.link = Some(v.len()),
            other => errs.push(format!("inode {}: unknown record kind {other}", ino.0)),
        }
        prev = Some(k);
    }
    if let Some(g) = group.take() {
        finish(g, reserved, &mut infos, errs);
    }
    check_graph(&infos, errs);
    Ok(())
}

fn check_graph(infos: &HashMap<u64, Info>, errs: &mut Errs) {
    match infos.get(&ROOT_INO.0) {
        Some(r) if r.seen && r.kind == 2 && r.parent == ROOT_INO.0 => {}
        _ => errs.push("root directory missing or malformed".into()),
    }
    for (&ino, e) in infos {
        if !e.seen {
            errs.push(format!("directory entry names missing inode {ino}"));
            continue;
        }
        if e.declared != 0 && (e.declared != e.kind || e.mismatch) {
            errs.push(format!(
                "directory entry for inode {ino} has the wrong type"
            ));
        }
        if ino == ROOT_INO.0 {
            if e.names != 0 {
                errs.push("root directory is named by an entry".into());
            }
        } else if e.names == 0 {
            errs.push(format!("orphaned inode {ino}"));
            continue;
        }
        if e.kind == 2 {
            let want = 2 + e.subdirs;
            if e.nlink != want {
                errs.push(format!(
                    "directory {ino} nlink {} but expected {want}",
                    e.nlink
                ));
            }
            if ino != ROOT_INO.0 {
                if e.names != 1 {
                    errs.push(format!("directory {ino} has {} names", e.names));
                }
                if e.dirent_parent != e.parent {
                    errs.push(format!(
                        "directory {ino} parent field disagrees with its entry"
                    ));
                }
            }
        } else if e.nlink != e.names {
            errs.push(format!(
                "inode {ino} nlink {} but {} names",
                e.nlink, e.names
            ));
        }
    }
    let mut good: HashMap<u64, bool> = HashMap::from([(ROOT_INO.0, true)]);
    for (&ino, e) in infos {
        if e.kind != 2 || good.contains_key(&ino) {
            continue;
        }
        let mut chain = vec![ino];
        let mut cur = e.parent;
        let ok = loop {
            if let Some(&g) = good.get(&cur) {
                break g;
            }
            if chain.contains(&cur) {
                break false;
            }
            chain.push(cur);
            match infos.get(&cur) {
                Some(p) if p.kind == 2 => cur = p.parent,
                _ => break false,
            }
        };
        for c in chain {
            good.insert(c, ok);
        }
        if !ok {
            errs.push(format!("directory {ino} is not reachable from the root"));
        }
    }
}
