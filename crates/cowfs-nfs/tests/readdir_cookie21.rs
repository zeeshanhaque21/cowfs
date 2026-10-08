//! READDIR and READDIRPLUS cookies across create, remove and rename between pages (#21).
//!
//! A cookie is a stable per-directory entry id, so edits made between two pages must neither
//! repeat a name nor drop a surviving one. Which created or renamed names appear is not pinned.
mod common;

use std::collections::HashSet;

use common::*;
use cowfs_nfs::MountOptions;

/// dircount is in bytes; the server packs `dircount / 24` entries per READDIR page.
const PAGE: u32 = 5 * 24;

fn page_through_edits(plus: bool) {
    let (_s, mut c) = serve(memfs(), MountOptions::default());
    let root = c.root.clone();
    let originals: Vec<String> = (0..40).map(|i| format!("f{i:02}")).collect();
    for n in &originals {
        c.create_file(&root, n);
    }

    let (st, page1, eof) = c.readdir_page(&root, 0, plus, PAGE);
    assert_eq!(st, OK);
    assert!(
        !eof && !page1.is_empty() && page1.len() < 40,
        "page 1 had {} entries",
        page1.len()
    );
    let mut cookie = page1.last().unwrap().cookie;
    let returned: Vec<String> = page1.iter().map(|e| e.name.clone()).collect();
    let pending: Vec<String> = originals
        .iter()
        .filter(|n| !returned.contains(*n))
        .cloned()
        .collect();
    let gone_returned = returned[0].clone();
    let gone_pending = pending[0].clone();
    let renamed = pending[1].clone();

    assert_eq!(c.remove(&root, &gone_returned), OK);
    assert_eq!(c.remove(&root, &gone_pending), OK);
    assert_eq!(c.rename(&root, &renamed, &root, "moved"), OK);
    c.create_file(&root, "new");

    let mut all = returned.clone();
    let mut done = false;
    for _ in 0..100 {
        let (st, page, last) = c.readdir_page(&root, cookie, plus, PAGE);
        assert_eq!(st, OK);
        all.extend(page.iter().map(|e| e.name.clone()));
        if last {
            done = true;
            break;
        }
        cookie = page
            .last()
            .expect("a page that is not the last has entries")
            .cookie;
    }
    assert!(done, "listing did not reach EOF");

    let count = |n: &str| all.iter().filter(|x| x.as_str() == n).count();
    assert_eq!(
        count(&gone_returned),
        1,
        "returned before removal, must not repeat"
    );
    assert_eq!(count(&gone_pending), 0, "removed before it was returned");
    assert_eq!(
        count(&renamed),
        0,
        "renamed away, the old name must not list"
    );
    assert!(count("moved") <= 1 && count("new") <= 1, "{all:?}");
    for n in &originals {
        if [&gone_returned, &gone_pending, &renamed].contains(&n) {
            continue;
        }
        assert_eq!(count(n), 1, "survivor {n} in {all:?}");
    }
    let unique: HashSet<&String> = all.iter().collect();
    assert_eq!(unique.len(), all.len(), "a name repeated: {all:?}");
}

#[test]
fn readdir_cookies_stay_stable_across_edits_between_pages() {
    page_through_edits(false);
}

#[test]
fn readdirplus_cookies_stay_stable_across_edits_between_pages() {
    page_through_edits(true);
}
