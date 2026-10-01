use std::collections::{HashMap, HashSet};

/// Stable `readdir` positions for one directory.
///
/// A backing filesystem's own directory offsets are not stable across removals on every
/// platform, so each name gets a sequence number the first time it is listed, and a cookie is
/// that number. A listing is the current names ordered by number, so a cookie stays valid after
/// its entry is removed, and names added later sort after every name already listed.
#[derive(Debug, Default)]
pub(crate) struct Cookies {
    seq: HashMap<Vec<u8>, u64>,
    next: u64,
}

impl Cookies {
    /// Brings the table in line with the directory's current `names` and returns them as
    /// `(cookie, name)` ordered by cookie. Names first seen now are numbered in byte order.
    pub(crate) fn sync(&mut self, mut names: Vec<Vec<u8>>) -> Vec<(u64, Vec<u8>)> {
        names.sort_unstable();
        let present: HashSet<&[u8]> = names.iter().map(Vec::as_slice).collect();
        self.seq.retain(|k, _| present.contains(k.as_slice()));
        for name in &names {
            if !self.seq.contains_key(name) {
                self.next += 1;
                self.seq.insert(name.clone(), self.next);
            }
        }
        let mut out: Vec<(u64, Vec<u8>)> = names
            .into_iter()
            .map(|n| (self.seq.get(&n).copied().unwrap_or(0), n))
            .collect();
        out.sort_unstable_by_key(|(c, _)| *c);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<Vec<u8>> {
        v.iter().map(|s| s.as_bytes().to_vec()).collect()
    }

    fn cookie_of(list: &[(u64, Vec<u8>)], name: &str) -> u64 {
        list.iter()
            .find(|(_, n)| n == name.as_bytes())
            .map_or(0, |(c, _)| *c)
    }

    #[test]
    fn first_listing_is_byte_order_with_nonzero_distinct_cookies() {
        let mut c = Cookies::default();
        let l = c.sync(names(&["b", "a", "c"]));
        let order: Vec<&[u8]> = l.iter().map(|(_, n)| n.as_slice()).collect();
        assert_eq!(order, [b"a".as_slice(), b"b", b"c"]);
        let cookies: HashSet<u64> = l.iter().map(|(c, _)| *c).collect();
        assert_eq!(cookies.len(), 3);
        assert!(!cookies.contains(&0));
    }

    #[test]
    fn cookies_survive_removal_and_new_names_sort_last() {
        let mut c = Cookies::default();
        let first = c.sync(names(&["a", "b", "c", "d"]));
        let cb = cookie_of(&first, "b");
        let second = c.sync(names(&["a", "c", "d", "0new"]));
        assert_eq!(cookie_of(&second, "a"), cookie_of(&first, "a"));
        assert_eq!(cookie_of(&second, "d"), cookie_of(&first, "d"));
        assert_eq!(
            second.last().map(|(_, n)| n.as_slice()),
            Some(b"0new".as_slice())
        );
        let after: Vec<&[u8]> = second
            .iter()
            .filter(|(k, _)| *k > cb)
            .map(|(_, n)| n.as_slice())
            .collect();
        assert_eq!(after, [b"c".as_slice(), b"d", b"0new"]);
    }

    #[test]
    fn removed_then_recreated_name_gets_a_new_position() {
        let mut c = Cookies::default();
        let first = c.sync(names(&["a", "b"]));
        c.sync(names(&["b"]));
        let third = c.sync(names(&["a", "b"]));
        assert!(cookie_of(&third, "a") > cookie_of(&first, "b"));
    }

    #[test]
    fn empty_directory_lists_nothing() {
        assert!(Cookies::default().sync(Vec::new()).is_empty());
    }
}
