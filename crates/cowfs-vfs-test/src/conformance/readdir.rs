//! `readdir`: paging, cookies and listing while the directory changes.
//!
//! Spike 2 and 4 lessons: a cookie that is an inode number duplicates or drops entries
//! when hardlinked names share an inode, and a cookie that is a position drops entries
//! when earlier entries are deleted between pages.
//!
//! Decisions pinned here: `.` and `..` are never listed; a cookie is never 0 (0 means
//! "from the start") and no two entries of one directory share a cookie; `eof` is true
//! exactly when no entry follows the last one returned, including when a page is exactly
//! full; resuming from the cookie of an entry that was since removed continues after the
//! position that entry had; an entry that exists for the whole listing appears exactly
//! once, an entry added or removed meanwhile appears at most once.

use std::collections::{HashMap, HashSet};

use cowfs_vfs::{DirEntry, Error, FileKind, Ino, ROOT_INO};

use super::{Ctx, Failure, Outcome};

type Names = Vec<Vec<u8>>;

/// Creates `n` entries (files, directories and symlinks, in rotation) named `{prefix}{i:06}`.
fn populate(c: &Ctx, dir: Ino, n: usize, prefix: &str) -> Result<Names, Failure> {
    let mut names = Vec::with_capacity(n);
    for i in 0..n {
        let name = format!("{prefix}{i:06}");
        match i % 3 {
            0 => c.create(dir, name.as_bytes(), 0o644).map(|_| ()),
            1 => c.mkdir(dir, name.as_bytes(), 0o755).map(|_| ()),
            _ => c.symlink(dir, name.as_bytes(), b"target").map(|_| ()),
        }?;
        names.push(name.into_bytes());
    }
    Ok(names)
}

fn sorted(mut v: Names) -> Names {
    v.sort();
    v
}

/// Lists with several page sizes and checks each listing holds exactly `want`, once.
fn check_pages(c: &Ctx, dir: Ino, want: &Names, pages: &[usize]) -> Outcome {
    let want = sorted(want.clone());
    let mut first: Option<Names> = None;
    for &page in pages {
        let got = c.list_paged(dir, page)?;
        let names: Names = got.iter().map(|e| e.name.clone()).collect();
        let set: HashSet<&Vec<u8>> = names.iter().collect();
        ensure_eq!(
            set.len(),
            names.len(),
            "duplicate names in a listing with page size {page}"
        );
        ensure_eq!(sorted(names.clone()), want, "listing with page size {page}");
        let cookies: HashSet<u64> = got.iter().map(|e| e.cookie).collect();
        ensure_eq!(
            cookies.len(),
            got.len(),
            "duplicate cookies with page size {page}"
        );
        ensure!(
            !cookies.contains(&0),
            "an entry has cookie 0 with page size {page}"
        );
        match &first {
            None => first = Some(names),
            Some(f) => ensure!(*f == names, "page size {page} lists in a different order"),
        }
    }
    Ok(())
}

pub fn readdir_empty_directory(c: &Ctx) -> Outcome {
    for max in [1, 10, 1000] {
        let r = c.fs.readdir(ROOT_INO, 0, max)?;
        ensure!(
            r.entries.is_empty() && r.eof,
            "empty root, max {max}: {r:?}"
        );
    }
    let d = c.dir(ROOT_INO, "d")?;
    for max in [1, 10, 1000] {
        let r = c.fs.readdir(d, 0, max)?;
        ensure!(
            r.entries.is_empty() && r.eof,
            "empty directory, max {max}: {r:?}"
        );
    }
    let r = c.fs.readdir(d, 12345, 10)?;
    ensure!(
        r.entries.is_empty() && r.eof,
        "empty directory resumed from an arbitrary cookie: {r:?}"
    );
    Ok(())
}

pub fn readdir_one_entry(c: &Ctx) -> Outcome {
    let f = c.file(ROOT_INO, "only")?;
    for max in [1, 2, 100] {
        let r = c.fs.readdir(ROOT_INO, 0, max)?;
        ensure_eq!(r.entries.len(), 1, "entries with max {max}");
        ensure!(r.eof, "eof is false after the only entry with max {max}");
        ensure_eq!(r.entries[0].ino, f, "entry inode");
        ensure_eq!(r.entries[0].name.clone(), b"only".to_vec(), "entry name");
        let rest = c.fs.readdir(ROOT_INO, r.entries[0].cookie, max)?;
        ensure!(
            rest.entries.is_empty() && rest.eof,
            "resume after the only entry: {rest:?}"
        );
    }
    Ok(())
}

pub fn readdir_5000_entries(c: &Ctx) -> Outcome {
    let names = populate(c, ROOT_INO, 5000, "e")?;
    check_pages(c, ROOT_INO, &names, &[1, 7, 100, 1000, 5000, 100_000])
}

/// Heavy: 50,000 entries.
pub fn readdir_50000_entries(c: &Ctx) -> Outcome {
    let names = populate(c, ROOT_INO, 50_000, "e")?;
    check_pages(c, ROOT_INO, &names, &[97, 4096, 100_000])
}

pub fn readdir_never_lists_dot_entries(c: &Ctx) -> Outcome {
    let d = c.dir(ROOT_INO, "d")?;
    populate(c, d, 30, "x")?;
    populate(c, ROOT_INO, 5, "y")?;
    for dir in [ROOT_INO, d] {
        for page in [1, 4, 1000] {
            for e in c.list_paged(dir, page)? {
                ensure!(
                    e.name != b"." && e.name != b"..",
                    "listing contains {:?}",
                    e.name
                );
            }
        }
    }
    Ok(())
}

pub fn readdir_cookies_resume_after_every_entry(c: &Ctx) -> Outcome {
    populate(c, ROOT_INO, 60, "e")?;
    let full = c.list(ROOT_INO)?;
    ensure_eq!(full.len(), 60, "entries");
    for (i, e) in full.iter().enumerate() {
        let r = c.fs.readdir(ROOT_INO, e.cookie, 1000)?;
        let got: Vec<&Vec<u8>> = r.entries.iter().map(|x| &x.name).collect();
        let want: Vec<&Vec<u8>> = full[i + 1..].iter().map(|x| &x.name).collect();
        ensure_eq!(got, want, "listing resumed after entry {i}");
        ensure!(
            r.eof,
            "eof is false when resuming after entry {i} with room to spare"
        );
    }
    Ok(())
}

pub fn readdir_max_one(c: &Ctx) -> Outcome {
    populate(c, ROOT_INO, 40, "e")?;
    let one: Names = c
        .list_paged(ROOT_INO, 1)?
        .into_iter()
        .map(|e| e.name)
        .collect();
    let all: Names = c
        .list_paged(ROOT_INO, 1000)?
        .into_iter()
        .map(|e| e.name)
        .collect();
    ensure_eq!(one, all, "max 1 listing versus one big page");
    Ok(())
}

pub fn readdir_eof_flag_is_exact(c: &Ctx) -> Outcome {
    populate(c, ROOT_INO, 10, "e")?;
    for max in 1..=12usize {
        let mut cookie = 0;
        let mut seen = 0;
        loop {
            let r = c.fs.readdir(ROOT_INO, cookie, max)?;
            seen += r.entries.len();
            ensure_eq!(
                r.eof,
                seen == 10,
                "eof after {seen} of 10 entries with max {max}"
            );
            match r.entries.last() {
                Some(l) => cookie = l.cookie,
                None => break,
            }
            if r.eof {
                break;
            }
        }
        ensure_eq!(seen, 10, "entries listed with max {max}");
    }
    let last = c.list(ROOT_INO)?.pop().map(|e| e.cookie).unwrap_or(0);
    let r = c.fs.readdir(ROOT_INO, last, 5)?;
    ensure!(
        r.entries.is_empty() && r.eof,
        "resume after the last entry: {r:?}"
    );
    Ok(())
}

pub fn readdir_cookies_distinct_and_nonzero(c: &Ctx) -> Outcome {
    let d = c.dir(ROOT_INO, "d")?;
    populate(c, d, 300, "e")?;
    let cookies: Vec<u64> = c.list(d)?.iter().map(|e| e.cookie).collect();
    ensure!(
        !cookies.contains(&0),
        "cookie 0 is reserved for 'from the start'"
    );
    ensure_eq!(
        cookies.iter().collect::<HashSet<_>>().len(),
        cookies.len(),
        "cookies are not distinct"
    );
    Ok(())
}

pub fn readdir_stable_order(c: &Ctx) -> Outcome {
    let names = populate(c, ROOT_INO, 100, "e")?;
    let a: Names = c.list(ROOT_INO)?.into_iter().map(|e| e.name).collect();
    let b: Names = c
        .list_paged(ROOT_INO, 3)?
        .into_iter()
        .map(|e| e.name)
        .collect();
    ensure_eq!(a, b, "two listings differ in order");
    ensure_eq!(sorted(a.clone()), sorted(names), "listing content");
    populate(c, ROOT_INO, 20, "new")?;
    remove(c, ROOT_INO, &a[10])?;
    let after: Names = c.list(ROOT_INO)?.into_iter().map(|e| e.name).collect();
    let old: Names = after
        .iter()
        .filter(|n| !n.starts_with(b"new"))
        .cloned()
        .collect();
    let want: Names = a
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != 10)
        .map(|(_, n)| n.clone())
        .collect();
    ensure_eq!(
        old,
        want,
        "the relative order of existing entries changed after adds and a removal"
    );
    Ok(())
}

pub fn readdir_entries_match_lookup(c: &Ctx) -> Outcome {
    populate(c, ROOT_INO, 30, "e")?;
    for e in c.list(ROOT_INO)? {
        let a = c.lookup(ROOT_INO, &e.name)?;
        ensure_eq!(
            (e.ino, e.kind),
            (a.ino, a.kind),
            "entry {:?} versus lookup",
            e.name
        );
        let i: usize = String::from_utf8_lossy(&e.name[1..]).parse().unwrap_or(0);
        let want = match i % 3 {
            0 => FileKind::Regular,
            1 => FileKind::Directory,
            _ => FileKind::Symlink,
        };
        ensure_eq!(e.kind, want, "kind of {:?}", e.name);
    }
    Ok(())
}

pub fn readdir_on_file_is_not_dir(c: &Ctx) -> Outcome {
    let f = c.file(ROOT_INO, "f")?;
    ensure_err!(c.fs.readdir(f, 0, 10), Error::NotDir, "readdir of a file");
    let s = c.symlink(ROOT_INO, b"s", b"/")?.ino;
    ensure_err!(
        c.fs.readdir(s, 0, 10),
        Error::NotDir,
        "readdir of a symlink"
    );
    Ok(())
}

fn remove(c: &Ctx, dir: Ino, name: &[u8]) -> Outcome {
    match c.fs.unlink(dir, name) {
        Err(Error::IsDir) => Ok(c.fs.rmdir(dir, name)?),
        r => Ok(r?),
    }
}

/// Walks a listing page by page, calling `between` after each page with the page and the
/// names not yet seen in the original order. Returns every name seen, in order.
fn walk(
    c: &Ctx,
    dir: Ino,
    page: usize,
    original: &Names,
    mut between: impl FnMut(&[DirEntry], &[Vec<u8>]) -> Outcome,
) -> Result<Names, Failure> {
    let mut seen: Names = Vec::new();
    let mut cookie = 0;
    for _ in 0..1_000_000 {
        let r = c.fs.readdir(dir, cookie, page)?;
        seen.extend(r.entries.iter().map(|e| e.name.clone()));
        let done: HashSet<&Vec<u8>> = seen.iter().collect();
        let upcoming: Vec<Vec<u8>> = original
            .iter()
            .filter(|n| !done.contains(n))
            .cloned()
            .collect();
        between(&r.entries, &upcoming)?;
        if r.eof {
            return Ok(seen);
        }
        let Some(last) = r.entries.last() else {
            return Err(Failure(
                "readdir returned no entries and eof is false".into(),
            ));
        };
        cookie = last.cookie;
    }
    Err(Failure("listing does not terminate".into()))
}

fn check_walk(seen: &Names, survivors: &Names, what: &str) -> Outcome {
    let counts = seen
        .iter()
        .fold(HashMap::new(), |mut m: HashMap<&Vec<u8>, u32>, n| {
            *m.entry(n).or_default() += 1;
            m
        });
    for (n, k) in &counts {
        ensure!(
            *k == 1,
            "{what}: {:?} listed {k} times",
            String::from_utf8_lossy(n)
        );
    }
    for s in survivors {
        ensure!(
            counts.contains_key(s),
            "{what}: surviving entry {:?} was dropped",
            String::from_utf8_lossy(s)
        );
    }
    Ok(())
}

pub fn readdir_delete_returned_entries_between_pages(c: &Ctx) -> Outcome {
    let names = populate(c, ROOT_INO, 200, "e")?;
    let seen = walk(c, ROOT_INO, 10, &names, |page, _| {
        page.iter().try_for_each(|e| remove(c, ROOT_INO, &e.name))
    })?;
    check_walk(&seen, &names, "deleting returned entries")?;
    ensure!(
        c.list(ROOT_INO)?.is_empty(),
        "directory not empty after deleting everything listed"
    );
    Ok(())
}

pub fn readdir_delete_upcoming_entries_between_pages(c: &Ctx) -> Outcome {
    let names = populate(c, ROOT_INO, 200, "e")?;
    let mut removed: HashSet<Vec<u8>> = HashSet::new();
    let seen = walk(c, ROOT_INO, 10, &names, |_, upcoming| {
        let fresh: Vec<&Vec<u8>> = upcoming
            .iter()
            .filter(|n| !removed.contains(*n))
            .take(5)
            .collect();
        for n in fresh {
            remove(c, ROOT_INO, n)?;
            removed.insert(n.clone());
        }
        Ok(())
    })?;
    let survivors: Names = names
        .iter()
        .filter(|n| !removed.contains(*n))
        .cloned()
        .collect();
    check_walk(&seen, &survivors, "deleting upcoming entries")?;
    ensure_eq!(
        sorted(c.names(ROOT_INO)?),
        sorted(survivors),
        "directory content after the walk"
    );
    Ok(())
}

pub fn readdir_delete_everything_between_pages(c: &Ctx) -> Outcome {
    let names = populate(c, ROOT_INO, 100, "e")?;
    let mut wiped = false;
    let seen = walk(c, ROOT_INO, 10, &names, |page, upcoming| {
        if !wiped {
            wiped = true;
            for e in page {
                remove(c, ROOT_INO, &e.name)?;
            }
            for n in upcoming {
                remove(c, ROOT_INO, n)?;
            }
        }
        Ok(())
    })?;
    check_walk(&seen, &Vec::new(), "deleting everything")?;
    ensure!(
        c.list(ROOT_INO)?.is_empty(),
        "directory not empty after deleting everything"
    );
    Ok(())
}

pub fn readdir_add_entries_between_pages(c: &Ctx) -> Outcome {
    let names = populate(c, ROOT_INO, 100, "e")?;
    let mut n = 0;
    let seen = walk(c, ROOT_INO, 10, &names, |_, _| {
        for _ in 0..3 {
            c.file(ROOT_INO, &format!("added{n:05}"))?;
            n += 1;
        }
        Ok(())
    })?;
    check_walk(&seen, &names, "adding entries")?;
    ensure_eq!(
        c.list(ROOT_INO)?.len(),
        100 + n,
        "directory size after the walk"
    );
    Ok(())
}

pub fn readdir_resume_from_removed_entry_cookie(c: &Ctx) -> Outcome {
    populate(c, ROOT_INO, 20, "e")?;
    let full = c.list(ROOT_INO)?;
    let first = c.fs.readdir(ROOT_INO, 0, 5)?;
    let Some(last) = first.entries.last() else {
        return Err(Failure("first page is empty".into()));
    };
    remove(c, ROOT_INO, &last.name)?;
    let rest = c.fs.readdir(ROOT_INO, last.cookie, 100)?;
    let got: Names = rest.entries.into_iter().map(|e| e.name).collect();
    let want: Names = full[5..].iter().map(|e| e.name.clone()).collect();
    ensure_eq!(got, want, "entries after the cookie of a removed entry");
    Ok(())
}

/// cowfs contract: `max` is at least 1. A caller that loops until `eof` would spin forever on a
/// backend that answers 0 with no entries and `eof` false, so 0 is `InvalidArgument`.
pub fn readdir_max_zero_is_invalid(c: &Ctx) -> Outcome {
    ensure_err!(
        c.fs.readdir(ROOT_INO, 0, 0),
        Error::InvalidArgument,
        "readdir with max 0 of an empty directory"
    );
    populate(c, ROOT_INO, 3, "e")?;
    ensure_err!(
        c.fs.readdir(ROOT_INO, 0, 0),
        Error::InvalidArgument,
        "readdir with max 0 of a non-empty directory"
    );
    Ok(())
}
