//! `Meta::check`: recomputes every invariant from the stored rows.

use crate::db::{decode_snap, meta_get, Inner, META, NODES, REFS, SNAPSHOTS, SNAP_NAMES};
use crate::node::NodeId;
use crate::ptree::{load, Cursor};
use crate::types::*;
use crate::{Error, Result};
use redb::{ReadOnlyTable, ReadableDatabase, ReadableTable, ReadableTableMetadata};
use std::collections::{HashMap, HashSet};

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

type Nodes = ReadOnlyTable<[u8; 32], &'static [u8]>;

pub(crate) fn check(inner: &Inner) -> Result<()> {
    let rtx = inner.db.begin_read()?;
    let nodes = rtx.open_table(NODES)?;
    let refs = rtx.open_table(REFS)?;
    let snaps = rtx.open_table(SNAPSHOTS)?;
    let names = rtx.open_table(SNAP_NAMES)?;
    let meta = rtx.open_table(META)?;
    let mut errs = Errs::default();

    let next_ino = meta_get(&meta, "next_ino")?;
    let next_snapshot = meta_get(&meta, "next_snapshot")?;
    if next_ino < 2 {
        errs.push(format!("next_ino {next_ino} below 2"));
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

    let mut expected: HashMap<NodeId, u64> = HashMap::new();
    let mut heights: HashMap<NodeId, u32> = HashMap::new();
    for info in &infos {
        *expected.entry(info.root).or_default() += 1;
        walk(&nodes, &info.root, &mut heights, &mut expected, &mut errs)?;
    }
    for r in refs.iter()? {
        let (k, v) = r?;
        let id = NodeId::from_bytes(k.value());
        match expected.get(&id) {
            Some(&n) if n == v.value() => {}
            Some(&n) => errs.push(format!(
                "node {id} has count {} but {n} references",
                v.value()
            )),
            None => errs.push(format!("count stored for unreachable node {id}")),
        }
    }
    if refs.len()? != expected.len() as u64 {
        errs.push(format!(
            "{} counts stored for {} reachable nodes",
            refs.len()?,
            expected.len()
        ));
    }
    if nodes.len()? != heights.len() as u64 {
        errs.push(format!(
            "{} nodes stored but {} reachable",
            nodes.len()?,
            heights.len()
        ));
    }

    for info in &infos {
        let mut e = Errs::default();
        check_snapshot(&nodes, &info.root, next_ino, &mut e)?;
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
    nodes: &Nodes,
    id: &NodeId,
    heights: &mut HashMap<NodeId, u32>,
    expected: &mut HashMap<NodeId, u64>,
    errs: &mut Errs,
) -> Result<u32> {
    if let Some(&h) = heights.get(id) {
        return Ok(h);
    }
    let n = load(nodes, id)?;
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
            *expected.entry(c).or_default() += 1;
            let h = walk(nodes, &c, heights, expected, errs)?;
            if *first.get_or_insert(h) != h {
                errs.push(format!("node {id} has children of different heights"));
            }
        }
        height = first.map_or(0, |h| h + 1);
    }
    heights.insert(*id, height);
    Ok(height)
}

struct Group {
    ino: u64,
    rec: Option<InodeRec>,
    names: Vec<(Vec<u8>, Ino, FileType, u64)>,
    cookies: HashMap<u64, (Ino, FileType, Vec<u8>)>,
    segs: Vec<(u32, usize, u64)>,
    link: Option<usize>,
}

impl Group {
    fn new(ino: u64) -> Self {
        Self {
            ino,
            rec: None,
            names: Vec::new(),
            cookies: HashMap::new(),
            segs: Vec::new(),
            link: None,
        }
    }
}

#[derive(Default)]
struct Snap {
    inodes: HashMap<u64, InodeRec>,
    dirents: Vec<(u64, Ino, FileType)>,
}

fn finish(g: Group, next_ino: u64, s: &mut Snap, errs: &mut Errs) {
    let ino = g.ino;
    if ino == 0 || ino >= next_ino {
        errs.push(format!("inode number {ino} outside 1..{next_ino}"));
    }
    let Some(rec) = g.rec else {
        errs.push(format!("records for inode {ino} but no inode record"));
        return;
    };
    let dir = rec.kind == FileType::Dir;
    if !dir && (!g.names.is_empty() || !g.cookies.is_empty()) {
        errs.push(format!("non-directory inode {ino} has directory entries"));
    }
    if g.names.len() != g.cookies.len() {
        errs.push(format!("directory {ino} has mismatched entry indexes"));
    }
    for (name, child, kind, cookie) in &g.names {
        match g.cookies.get(cookie) {
            Some((c, k, n)) if c == child && k == kind && n == name => {}
            _ => errs.push(format!(
                "directory {ino} entry {name:?} disagrees with its cookie row"
            )),
        }
        if *cookie == 0 || *cookie >= rec.next_cookie {
            errs.push(format!(
                "directory {ino} cookie {cookie} outside next_cookie"
            ));
        }
        s.dirents.push((ino, *child, *kind));
    }
    match rec.kind {
        FileType::File => {
            if g.link.is_some() {
                errs.push(format!("file {ino} has a link target"));
            }
            let mut total = 0u64;
            for (i, (idx, n, sum)) in g.segs.iter().enumerate() {
                if *idx as usize != i || *n == 0 || *n > CHUNKS_PER_SEGMENT {
                    errs.push(format!("file {ino} chunk segment {idx} malformed"));
                }
                total += sum;
            }
            if total > rec.size {
                errs.push(format!(
                    "file {ino} chunks cover {total} bytes but size is {}",
                    rec.size
                ));
            }
        }
        FileType::Symlink => {
            if !g.segs.is_empty() {
                errs.push(format!("symlink {ino} has chunks"));
            }
            if g.link != Some(rec.size as usize) || rec.size == 0 {
                errs.push(format!("symlink {ino} target does not match its size"));
            }
        }
        FileType::Dir => {
            if !g.segs.is_empty() || g.link.is_some() || rec.size != 0 {
                errs.push(format!("directory {ino} has file data"));
            }
        }
    }
    s.inodes.insert(ino, rec);
}

fn check_snapshot(nodes: &Nodes, root: &NodeId, next_ino: u64, errs: &mut Errs) -> Result<()> {
    let mut s = Snap::default();
    let mut cur = Cursor::seek(nodes, root, &[])?;
    let mut prev: Option<Vec<u8>> = None;
    let mut group: Option<Group> = None;
    while let Some((k, v)) = cur.next(nodes)? {
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
                finish(g, next_ino, &mut s, errs);
            }
        }
        let g = group.get_or_insert_with(|| Group::new(ino.0));
        let mut bad = |what: &str, e: Error| errs.push(format!("inode {}: {what}: {e}", ino.0));
        match kind {
            K_INODE => match InodeRec::decode(&v) {
                Ok(r) => g.rec = Some(r),
                Err(e) => bad("inode record", e),
            },
            K_NAME => match decode_name_val(&v) {
                Ok((c, t, cookie)) => g.names.push((suffix.to_vec(), c, t, cookie)),
                Err(e) => bad("dirent", e),
            },
            K_COOKIE => match (decode_cookie_val(&v), <[u8; 8]>::try_from(suffix)) {
                (Ok((c, t, n)), Ok(ck)) => {
                    g.cookies.insert(u64::from_be_bytes(ck), (c, t, n.to_vec()));
                }
                _ => errs.push(format!("inode {}: bad cookie row", ino.0)),
            },
            K_XATTR => {}
            K_CHUNK => match (decode_chunks(&v), <[u8; 4]>::try_from(suffix)) {
                (Ok(c), Ok(idx)) => g.segs.push((
                    u32::from_be_bytes(idx),
                    c.len(),
                    c.iter().map(|c| u64::from(c.len)).sum(),
                )),
                _ => errs.push(format!("inode {}: bad chunk segment", ino.0)),
            },
            K_LINK => g.link = Some(v.len()),
            other => errs.push(format!("inode {}: unknown record kind {other}", ino.0)),
        }
        prev = Some(k);
    }
    if let Some(g) = group.take() {
        finish(g, next_ino, &mut s, errs);
    }
    check_graph(&s, errs);
    Ok(())
}

fn check_graph(s: &Snap, errs: &mut Errs) {
    match s.inodes.get(&ROOT_INO.0) {
        Some(r) if r.kind == FileType::Dir && r.parent == ROOT_INO.0 => {}
        _ => errs.push("root directory missing or malformed".into()),
    }
    let mut named: HashMap<u64, u32> = HashMap::new();
    let mut parents: HashMap<u64, u64> = HashMap::new();
    let mut subdirs: HashMap<u64, u32> = HashMap::new();
    let mut children: HashMap<u64, Vec<u64>> = HashMap::new();
    for (dir, child, kind) in &s.dirents {
        match s.inodes.get(&child.0) {
            None => errs.push(format!("directory {dir} names missing inode {child}")),
            Some(r) if r.kind != *kind => errs.push(format!(
                "directory {dir} entry for {child} has the wrong type"
            )),
            Some(_) => {}
        }
        *named.entry(child.0).or_default() += 1;
        if *kind == FileType::Dir {
            *subdirs.entry(*dir).or_default() += 1;
            parents.insert(child.0, *dir);
            children.entry(*dir).or_default().push(child.0);
        }
    }
    for (&ino, r) in &s.inodes {
        let n = named.get(&ino).copied().unwrap_or(0);
        if ino == ROOT_INO.0 {
            if n != 0 {
                errs.push("root directory is named by an entry".into());
            }
        } else if n == 0 {
            errs.push(format!("orphaned inode {ino}"));
            continue;
        }
        if r.kind == FileType::Dir {
            let want = 2 + subdirs.get(&ino).copied().unwrap_or(0);
            if r.nlink != want {
                errs.push(format!(
                    "directory {ino} nlink {} but expected {want}",
                    r.nlink
                ));
            }
            if ino != ROOT_INO.0 {
                if n != 1 {
                    errs.push(format!("directory {ino} has {n} names"));
                }
                if parents.get(&ino) != Some(&r.parent) {
                    errs.push(format!(
                        "directory {ino} parent field disagrees with its entry"
                    ));
                }
            }
        } else if r.nlink != n {
            errs.push(format!("inode {ino} nlink {} but {n} names", r.nlink));
        }
    }
    let mut seen: HashSet<u64> = HashSet::from([ROOT_INO.0]);
    let mut work = vec![ROOT_INO.0];
    while let Some(d) = work.pop() {
        for &c in children.get(&d).into_iter().flatten() {
            if seen.insert(c) {
                work.push(c);
            }
        }
    }
    for (&ino, r) in &s.inodes {
        if r.kind == FileType::Dir && !seen.contains(&ino) {
            errs.push(format!("directory {ino} is not reachable from the root"));
        }
    }
}
