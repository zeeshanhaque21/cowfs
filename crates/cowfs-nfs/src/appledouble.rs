//! The AppleDouble (`._name`) file format the macOS client uses to keep extended attributes on
//! file systems without native support, as laid out by xnu (`bsd/vfs/vfs_xattr.c`):
//!
//! ```text
//! 0   magic 0x00051607, version 0x00020000, filler "Mac OS X        ", u16 entry count (2)
//! 26  entry 0: type 9 (Finder info), offset 50, length total_size - 50
//! 38  entry 1: type 2 (resource fork), offset total_size, length
//! 50  Finder info, 32 bytes, then 2 pad bytes
//! 84  'ATTR' header: tag, total_size, data_start, data_length, 3 reserved, flags, count
//! 120 count attribute entries: data offset, data length, flags, name length (with NUL), name
//! ..  attribute data, up to total_size, then the resource fork
//! ```
//!
//! All integers are big endian. Attributes named `com.apple.FinderInfo` and
//! `com.apple.ResourceFork` live in the two entries, everything else in the `ATTR` block.
//! `Sidecar::decode` applies the checks the kernel applies and returns `None` for anything
//! that is not a well formed file, including a file that is still being written.
use std::collections::BTreeMap;

/// The xattr that holds the 32 bytes of Finder info.
pub const FINDER_INFO: &[u8] = b"com.apple.FinderInfo";
/// The xattr that holds the resource fork.
pub const RESOURCE_FORK: &[u8] = b"com.apple.ResourceFork";
/// Longest attribute name the format holds, without the NUL.
pub const MAX_NAME: usize = 127;

const MAGIC: u32 = 0x0005_1607;
const VERSION: u32 = 0x0002_0000;
const FILLER: &[u8; 16] = b"Mac OS X        ";
const ATTR_MAGIC: u32 = 0x4154_5452;
const AD_RESOURCE: u32 = 2;
const AD_FINDERINFO: u32 = 9;
const ENTRIES_AT: usize = 26;
const FINFO_AT: usize = 50;
const FINFO_LEN: usize = 32;
const ATTR_HDR_AT: usize = 84;
const ATTR_HDR_LEN: usize = 36;
const FIRST_ENTRY: usize = ATTR_HDR_AT + ATTR_HDR_LEN;
const BUF_SIZE: usize = 4096;
const EMPTY_FORK_LEN: usize = 286;
const EMPTY_FORK_TAG: &[u8] = b"This resource fork intentionally left blank   \0";
const MAX_ATTRS: usize = 256;

/// The extended attributes of one file as an AppleDouble file holds them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sidecar {
    /// `None` when absent or all zero, which the kernel treats alike.
    pub finder_info: Option<[u8; 32]>,
    pub resource_fork: Vec<u8>,
    pub attrs: BTreeMap<Vec<u8>, Vec<u8>>,
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at.checked_add(4)?)
        .and_then(|s| <[u8; 4]>::try_from(s).ok())
        .map(u32::from_be_bytes)
}

fn be16(b: &[u8], at: usize) -> Option<u16> {
    b.get(at..at.checked_add(2)?)
        .and_then(|s| <[u8; 2]>::try_from(s).ok())
        .map(u16::from_be_bytes)
}

fn put32(b: &mut [u8], at: usize, v: u32) {
    if let Some(s) = b.get_mut(at..at + 4) {
        s.copy_from_slice(&v.to_be_bytes());
    }
}

fn entry_len(name_len_with_nul: usize) -> usize {
    (11 + name_len_with_nul + 3) & !3
}

/// True for names an ATTR block can hold and that are not the two special entries.
pub fn is_plain_attr(name: &[u8]) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && !name.contains(&0)
        && name != FINDER_INFO
        && name != RESOURCE_FORK
}

fn is_empty_fork(fork: &[u8]) -> bool {
    fork.len() == EMPTY_FORK_LEN && fork.get(16..16 + EMPTY_FORK_TAG.len()) == Some(EMPTY_FORK_TAG)
}

impl Sidecar {
    /// Sorts a file's xattrs into the sidecar's slots. Names the format cannot hold are left out.
    pub fn from_xattrs(xattrs: impl IntoIterator<Item = (Vec<u8>, Vec<u8>)>) -> Sidecar {
        let mut s = Sidecar::default();
        for (name, value) in xattrs {
            if name == FINDER_INFO {
                let mut f = [0u8; 32];
                let n = value.len().min(32);
                f[..n].copy_from_slice(&value[..n]);
                s.finder_info = (f != [0; 32]).then_some(f);
            } else if name == RESOURCE_FORK {
                s.resource_fork = value;
            } else if is_plain_attr(&name) {
                s.attrs.insert(name, value);
            }
        }
        s
    }

    /// The xattrs this sidecar stands for.
    pub fn to_xattrs(&self) -> Vec<(Vec<u8>, Vec<u8>)> {
        let mut out = Vec::new();
        if let Some(f) = self.finder_info {
            out.push((FINDER_INFO.to_vec(), f.to_vec()));
        }
        if !self.resource_fork.is_empty() {
            out.push((RESOURCE_FORK.to_vec(), self.resource_fork.clone()));
        }
        out.extend(self.attrs.iter().map(|(k, v)| (k.clone(), v.clone())));
        out
    }

    pub fn is_empty(&self) -> bool {
        self.finder_info.is_none() && self.resource_fork.is_empty() && self.attrs.is_empty()
    }

    /// The bytes of the `._` file: what the kernel itself would have written.
    pub fn encode(&self) -> Vec<u8> {
        Self::encode_capped(self, MAX_ATTRS)
    }

    #[cfg(test)]
    fn encode_all(s: &Sidecar) -> Vec<u8> {
        Sidecar::encode_capped(s, usize::MAX)
    }

    fn encode_capped(&self, cap: usize) -> Vec<u8> {
        let attrs: Vec<(&Vec<u8>, &Vec<u8>)> = self
            .attrs
            .iter()
            .filter(|(k, _)| is_plain_attr(k))
            .take(cap)
            .collect();
        let entries: usize = attrs.iter().map(|(k, _)| entry_len(k.len() + 1)).sum();
        let data_start = FIRST_ENTRY + entries;
        let data: usize = attrs.iter().map(|(_, v)| v.len()).sum();
        let attr_end = data_start + data;
        // The kernel reads the Finder info entry as ATTR_BUF_SIZE bytes, so total_size
        // must sit just under a multiple of it.
        let total_size =
            (attr_end + EMPTY_FORK_LEN).div_ceil(BUF_SIZE).max(1) * BUF_SIZE - EMPTY_FORK_LEN;
        let fork_len = if self.resource_fork.is_empty() {
            EMPTY_FORK_LEN
        } else {
            self.resource_fork.len()
        };
        let mut b = vec![0u8; total_size + fork_len];

        put32(&mut b, 0, MAGIC);
        put32(&mut b, 4, VERSION);
        b[8..24].copy_from_slice(FILLER);
        b[24..26].copy_from_slice(&2u16.to_be_bytes());
        put32(&mut b, ENTRIES_AT, AD_FINDERINFO);
        put32(&mut b, ENTRIES_AT + 4, FINFO_AT as u32);
        put32(&mut b, ENTRIES_AT + 8, (total_size - FINFO_AT) as u32);
        put32(&mut b, ENTRIES_AT + 12, AD_RESOURCE);
        put32(&mut b, ENTRIES_AT + 16, total_size as u32);
        put32(&mut b, ENTRIES_AT + 20, fork_len as u32);
        if let Some(f) = self.finder_info {
            b[FINFO_AT..FINFO_AT + FINFO_LEN].copy_from_slice(&f);
        }
        put32(&mut b, ATTR_HDR_AT, ATTR_MAGIC);
        put32(&mut b, ATTR_HDR_AT + 8, total_size as u32);
        put32(&mut b, ATTR_HDR_AT + 12, data_start as u32);
        put32(&mut b, ATTR_HDR_AT + 16, data as u32);
        b[ATTR_HDR_AT + 34..ATTR_HDR_AT + 36].copy_from_slice(&(attrs.len() as u16).to_be_bytes());

        let (mut entry_at, mut data_at) = (FIRST_ENTRY, data_start);
        for (name, value) in attrs {
            put32(&mut b, entry_at, data_at as u32);
            put32(&mut b, entry_at + 4, value.len() as u32);
            b[entry_at + 10] = (name.len() + 1) as u8;
            b[entry_at + 11..entry_at + 11 + name.len()].copy_from_slice(name);
            entry_at += entry_len(name.len() + 1);
            b[data_at..data_at + value.len()].copy_from_slice(value);
            data_at += value.len();
        }

        if self.resource_fork.is_empty() {
            let at = total_size;
            put32(&mut b, at, 256);
            put32(&mut b, at + 4, 256);
            put32(&mut b, at + 12, 30);
            b[at + 16..at + 16 + EMPTY_FORK_TAG.len()].copy_from_slice(EMPTY_FORK_TAG);
            put32(&mut b, at + 256, 256);
            put32(&mut b, at + 260, 256);
            put32(&mut b, at + 268, 30);
            b[at + 278..at + 280].copy_from_slice(&28u16.to_be_bytes());
            b[at + 280..at + 282].copy_from_slice(&30u16.to_be_bytes());
            b[at + 284..at + 286].copy_from_slice(&0xffffu16.to_be_bytes());
        } else {
            b[total_size..].copy_from_slice(&self.resource_fork);
        }
        b
    }

    /// Parses a `._` file. `None` means it is not (yet) a well formed AppleDouble file.
    pub fn decode(buf: &[u8]) -> Option<Sidecar> {
        if be32(buf, 0)? != MAGIC || be32(buf, 4)? != VERSION {
            return None;
        }
        let count = usize::from(be16(buf, 24)?);
        if !(1..=15).contains(&count) {
            return None;
        }
        let header_end = ENTRIES_AT + 12 * count;
        if buf.len() < header_end {
            return None;
        }
        let mut entries: Vec<(u32, usize, usize)> = Vec::new();
        for i in 0..count {
            let at = ENTRIES_AT + 12 * i;
            let kind = be32(buf, at)?;
            let off = usize::try_from(be32(buf, at + 4)?).ok()?;
            let len = usize::try_from(be32(buf, at + 8)?).ok()?;
            let end = off.checked_add(len)?;
            if off < header_end || end > buf.len() {
                return None;
            }
            if entries.iter().any(|&(_, o, l)| end > o && o + l > off) {
                return None;
            }
            entries.push((kind, off, len));
        }

        let mut s = Sidecar::default();
        for &(kind, off, len) in &entries {
            if kind == AD_FINDERINFO && len >= FINFO_LEN {
                let f: [u8; 32] = buf.get(off..off + FINFO_LEN)?.try_into().ok()?;
                s.finder_info = (f != [0; 32]).then_some(f);
            } else if kind == AD_RESOURCE {
                let fork = buf.get(off..off + len)?;
                if !fork.is_empty() && !is_empty_fork(fork) {
                    s.resource_fork = fork.to_vec();
                }
            }
        }

        let plain_layout = count == 2
            && entries[0].0 == AD_FINDERINFO
            && entries[1].0 == AD_RESOURCE
            && entries[0].1 == FINFO_AT
            && entries[0].2 >= FIRST_ENTRY - FINFO_AT;
        if plain_layout && be32(buf, ATTR_HDR_AT)? == ATTR_MAGIC {
            s.attrs = decode_attrs(buf, entries[0].1 + entries[0].2)?;
        }
        Some(s)
    }
}

fn decode_attrs(buf: &[u8], finfo_end: usize) -> Option<BTreeMap<Vec<u8>, Vec<u8>>> {
    let total_size = usize::try_from(be32(buf, ATTR_HDR_AT + 8)?).ok()?;
    let data_start = usize::try_from(be32(buf, ATTR_HDR_AT + 12)?).ok()?;
    let data_len = usize::try_from(be32(buf, ATTR_HDR_AT + 16)?).ok()?;
    let count = usize::from(be16(buf, ATTR_HDR_AT + 34)?);
    let end = data_start.checked_add(data_len)?;
    if total_size > finfo_end
        || data_start < FIRST_ENTRY
        || end > total_size
        || total_size > buf.len()
        || count > MAX_ATTRS
    {
        return None;
    }
    let mut out = BTreeMap::new();
    let mut at = FIRST_ENTRY;
    let mut header_size = FIRST_ENTRY;
    for _ in 0..count {
        let off = usize::try_from(be32(buf, at)?).ok()?;
        let len = usize::try_from(be32(buf, at + 4)?).ok()?;
        let namelen = usize::from(*buf.get(at + 10)?);
        if namelen == 0 || at + 11 + namelen > data_start {
            return None;
        }
        let name = buf.get(at + 11..at + 10 + namelen)?;
        if buf.get(at + 10 + namelen) != Some(&0) || name.contains(&0) {
            return None;
        }
        let data_end = off.checked_add(len)?;
        if off < data_start || data_end > total_size {
            return None;
        }
        if name != FINDER_INFO && name != RESOURCE_FORK {
            out.insert(name.to_vec(), buf.get(off..data_end)?.to_vec());
        }
        let step = entry_len(namelen);
        header_size += step;
        at += step;
    }
    (data_start >= header_size).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Sidecar {
        let mut s = Sidecar::default();
        s.attrs
            .insert(b"com.apple.provenance".to_vec(), vec![1, 2, 3, 4, 5]);
        s.attrs.insert(b"user.note".to_vec(), b"hello".to_vec());
        s
    }

    #[test]
    fn an_empty_sidecar_is_what_the_kernel_writes() {
        let b = Sidecar::default().encode();
        assert_eq!(b.len(), BUF_SIZE);
        assert_eq!(be32(&b, 0), Some(MAGIC));
        assert_eq!(&b[8..24], FILLER);
        assert_eq!(Sidecar::decode(&b), Some(Sidecar::default()));
        assert!(Sidecar::decode(&b).unwrap().is_empty());
    }

    #[test]
    fn every_pairing_of_slots_round_trips() {
        for (fi, rf) in [(false, false), (true, false), (false, true), (true, true)] {
            let mut s = sample();
            if fi {
                s.finder_info = Some([9; 32]);
            }
            if rf {
                s.resource_fork = vec![3; 100];
            }
            let b = s.encode();
            assert_eq!(
                Sidecar::decode(&b).as_ref(),
                Some(&s),
                "fi={fi} rf={rf} len={}",
                b.len()
            );
        }
    }

    #[test]
    fn attributes_round_trip() {
        let mut s = sample();
        s.finder_info = Some([9; 32]);
        s.resource_fork = (0..5000u32).map(|i| i as u8).collect();
        let b = s.encode();
        assert_eq!(Sidecar::decode(&b), Some(s));
    }

    #[test]
    fn large_attributes_grow_the_file() {
        let mut s = Sidecar::default();
        s.attrs.insert(b"big".to_vec(), vec![7; 10_000]);
        let b = s.encode();
        assert!(b.len() > BUF_SIZE);
        assert_eq!(Sidecar::decode(&b), Some(s));
    }

    #[test]
    fn a_kernel_made_file_parses() {
        let mut b = vec![0u8; BUF_SIZE];
        put32(&mut b, 0, MAGIC);
        put32(&mut b, 4, VERSION);
        b[8..24].copy_from_slice(FILLER);
        b[24..26].copy_from_slice(&2u16.to_be_bytes());
        let rf = EMPTY_FORK_LEN;
        put32(&mut b, 26, AD_FINDERINFO);
        put32(&mut b, 30, 50);
        put32(&mut b, 34, (BUF_SIZE - 50 - rf) as u32);
        put32(&mut b, 38, AD_RESOURCE);
        put32(&mut b, 42, (BUF_SIZE - rf) as u32);
        put32(&mut b, 46, rf as u32);
        put32(&mut b, ATTR_HDR_AT, ATTR_MAGIC);
        put32(&mut b, ATTR_HDR_AT + 8, (BUF_SIZE - rf) as u32);
        put32(&mut b, ATTR_HDR_AT + 12, 140);
        put32(&mut b, ATTR_HDR_AT + 16, 4);
        b[ATTR_HDR_AT + 34..ATTR_HDR_AT + 36].copy_from_slice(&1u16.to_be_bytes());
        put32(&mut b, 120, 140);
        put32(&mut b, 124, 4);
        b[130] = 4;
        b[131..134].copy_from_slice(b"a.b");
        b[140..144].copy_from_slice(b"data");
        let s = Sidecar::decode(&b).expect("well formed");
        assert_eq!(s.attrs.get(b"a.b".as_slice()), Some(&b"data".to_vec()));
    }

    #[test]
    fn xattrs_map_both_ways() {
        let mut fi = [0u8; 32];
        fi[0] = 1;
        let xs = vec![
            (FINDER_INFO.to_vec(), fi.to_vec()),
            (RESOURCE_FORK.to_vec(), b"rsrc".to_vec()),
            (b"x.y".to_vec(), b"v".to_vec()),
            (vec![b'n'; 200], b"too long a name".to_vec()),
        ];
        let s = Sidecar::from_xattrs(xs);
        assert_eq!(s.finder_info, Some(fi));
        assert_eq!(s.resource_fork, b"rsrc");
        assert_eq!(s.attrs.len(), 1, "the 200 byte name does not fit");
        let back = s.to_xattrs();
        assert_eq!(back.len(), 3);
        assert_eq!(Sidecar::from_xattrs(back), s);
        let zero = Sidecar::from_xattrs([(FINDER_INFO.to_vec(), vec![0; 32])]);
        assert!(zero.is_empty(), "all-zero Finder info is no attribute");
    }

    #[test]
    fn every_count_and_size_of_attributes_survives_a_round_trip() {
        let sizes = [0usize, 1, 3, 4, 127, 128, 1000, 4096];
        // 255 is where the client stops, so that is where the property has to hold.
        for n in 0..=MAX_ATTRS {
            let mut s = Sidecar::default();
            for i in 0..n {
                let size = sizes[i % sizes.len()];
                s.attrs
                    .insert(format!("user.a{i:03}x").into_bytes(), vec![i as u8; size]);
            }
            if n % 5 == 0 {
                s.finder_info = Some([(n % 251 + 1) as u8; 32]);
            }
            let b = s.encode();
            assert_eq!(b.len() % BUF_SIZE, 0, "{n} attributes, {} bytes", b.len());
            assert_eq!(Sidecar::decode(&b).as_ref(), Some(&s), "{n} attributes");
        }
    }

    #[test]
    fn a_sidecar_past_the_attribute_cap_is_not_a_sidecar() {
        let mut over = Sidecar::default();
        for i in 0..MAX_ATTRS + 40 {
            over.attrs
                .insert(format!("user.a{i:03}x").into_bytes(), vec![1; 100]);
        }
        // What a client writes is not truncated, so the adapter must be able to say no to it.
        assert!(Sidecar::decode(&Sidecar::encode_all(&over)).is_none());
        // What the adapter synthesises is truncated, because the client stops there anyway. The
        // attributes themselves stay in the Vfs.
        let shown = Sidecar::decode(&over.encode()).unwrap();
        assert_eq!(shown.attrs.len(), MAX_ATTRS);
        assert!(over.attrs.len() > shown.attrs.len());
    }

    #[test]
    fn a_whole_file_write_that_is_not_a_sidecar_is_refused() {
        // What the adapter has to be able to tell: a real file named ._x must not be accepted.
        assert_eq!(Sidecar::decode(b"my real file content"), None);
        assert_eq!(Sidecar::decode(&[0u8; 20]), None);
        assert!(Sidecar::decode(&Sidecar::default().encode()).is_some());
    }

    #[test]
    fn garbage_and_partial_files_are_refused_without_panicking() {
        let good = sample().encode();
        assert_eq!(Sidecar::decode(&[]), None);
        assert_eq!(Sidecar::decode(&[0; 4096]), None);
        assert_eq!(Sidecar::decode(&good[..60]), None);
        assert_eq!(
            Sidecar::decode(&good[..good.len() - 10]),
            None,
            "resource fork cut off"
        );
        for cut in 0..good.len() {
            let _ = Sidecar::decode(&good[..cut]);
        }
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        for _ in 0..3000 {
            let mut b = good.clone();
            for _ in 0..4 {
                seed = seed
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let i = (seed >> 33) as usize % b.len();
                b[i] = (seed >> 20) as u8;
            }
            let _ = Sidecar::decode(&b);
        }
    }

    #[test]
    fn overlapping_entries_and_bad_attr_blocks_are_refused() {
        let mut b = sample().encode();
        put32(&mut b, ENTRIES_AT + 16, 60);
        assert_eq!(Sidecar::decode(&b), None);
        let mut b = sample().encode();
        put32(&mut b, 120, 1);
        assert_eq!(
            Sidecar::decode(&b),
            None,
            "attribute data inside the header"
        );
        let mut b = sample().encode();
        put32(&mut b, ATTR_HDR_AT + 34 - 2, 0xffff);
        let _ = Sidecar::decode(&b);
    }

    #[test]
    fn a_plain_finder_info_file_has_no_attrs() {
        let mut b = vec![0u8; 82];
        put32(&mut b, 0, MAGIC);
        put32(&mut b, 4, VERSION);
        b[24..26].copy_from_slice(&1u16.to_be_bytes());
        put32(&mut b, 26, AD_FINDERINFO);
        put32(&mut b, 30, 38);
        put32(&mut b, 34, 32);
        b[38] = 5;
        let s = Sidecar::decode(&b).unwrap();
        assert_eq!(s.finder_info.map(|f| f[0]), Some(5));
        assert!(s.attrs.is_empty());
    }
}
