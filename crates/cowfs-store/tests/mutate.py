#!/usr/bin/env python3
"""Mutation check for cowfs-store. Usage: [MUT_PKG="cowfs-store"] [MUT_ARGS="--test crash --test power_discard"] python3 tests/mutate.py [TAG ...]

MUT_PKG names the packages whose tests run (default cowfs-store); a mutation whose file name has a
slash is relative to crates/ (the cowfs-gc ones), otherwise it is a cowfs-store source file.

Copies the repo to target/mut/work, applies one textual mutation at a time to the store
sources, runs the store test suite with its own CARGO_TARGET_DIR, and records whether any test
failed. A mutant that leaves the suite green is a SURVIVOR: either a test gap or an equivalent
mutation. Results go to target/mut/results.txt. The list is documented in docs/v1-store.md.
"""
import os, re, shutil, subprocess, sys

root = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..", ".."))
mut = f"{root}/target/mut"
work = f"{mut}/work"
src = f"{root}/crates/cowfs-store/src"
dst = f"{work}/crates/cowfs-store/src"
pkgs = [x for p in os.environ.get("MUT_PKG", "cowfs-store").split() for x in ("-p", p)]
extra = os.environ.get("MUT_ARGS", "").split()


def move_wm_after_unlink(t):
    """Mutation 2 of docs/crash-injection-173.md: the watermark raise runs after the unlink."""
    a = t.index("        {\n            let mut wm =\n                g.wm.lock()")
    b = t.index("        fsio::mark(MARK_UNLINK);")
    block = t[a:b]
    t = t[:a] + t[b:]
    anchor = "        self.forget_pack(id);\n        let mut out = Discarded {"
    if anchor not in t:
        raise ValueError(anchor)
    return t.replace(anchor, "        self.forget_pack(id);\n" + block + "        let mut out = Discarded {", 1)

M = [
    ("G1 get skips BLAKE3 check", "store.rs", "if BlockId::of(&data) != id {\n            return Err(Error::HashMismatch(id));", "if false {\n            return Err(Error::HashMismatch(id));"),
    ("G2 get skips record CRC", "store.rs", "if Header::expected_crc(raw, payload) != header.crc {\n            return Err(corrupt(\"checksum mismatch\"));", "if false {\n            return Err(corrupt(\"checksum mismatch\"));"),
    ("G3 open never cuts a torn tail", "store.rs", "if is_last && offset >= dur && !exhausted && torn_from.is_none() {", "if false && is_last && offset >= dur && !exhausted && torn_from.is_none() {"),
    ("G4 torn detection ignores the watermark", "store.rs", "if is_last && offset >= dur && !exhausted && torn_from.is_none() {", "if is_last && !exhausted && torn_from.is_none() {"),
    ("G5 torn tail not saved to sidecar", "store.rs", "save_torn(&dir, id, &file, t, len);", ""),
    ("G6 records after a torn region not moved down", "store.rs", "file.write_all_at(&buf, w)?;", "let _ = &buf;"),
    ("W1 watermark written before data fsync", "store.rs", "            self.io.sync_file(&file, &pack::pack_path(&self.dir, id))?;\n            self.wm\n                .lock()\n                .unwrap_or_else(PoisonError::into_inner)\n                .advance(Mark { pack: id, len })?;", "            self.wm\n                .lock()\n                .unwrap_or_else(PoisonError::into_inner)\n                .advance(Mark { pack: id, len })?;\n            self.io.sync_file(&file, &pack::pack_path(&self.dir, id))?;"),
    ("W2 watermark write not fsynced", "wm.rs", "        io.sync_file(&self.file, &self.path)?;\n        self.seq = seq;", "        self.seq = seq;"),
    ("W3 watermark slot CRC ignored", "wm.rs", "if crc32c::crc32c(&b[..24]).to_le_bytes() != b[24..28] {", "if false {"),
    ("W4 pick the OLDER watermark slot", "wm.rs", ".max_by_key(|(seq, _, _)| *seq);", ".min_by_key(|(seq, _, _)| *seq);"),
    ("W5 both watermark slots at one offset", "wm.rs", "(seq % 2) * SLOT as u64", "0u64"),
    ("W6 sync never fsyncs data or watermark", "store.rs", "w.synced < (w.id, w.len),", "false,"),
    ("W7 sealed packs not treated as durable", "store.rs", "            if Some(id) != last {\n                return u64::MAX;\n            }", "            if Some(id) != last {\n                return 0;\n            }"),
    ("W8 advance may regress the mark", "wm.rs", "if self.mark.is_some_and(|m| m >= mark) {", "if false {"),
    ("W9 first watermark written once, not twice", "wm.rs", "        self.write(io, mark, self.base)?;\n        self.write(io, mark, self.base)\n", "        self.write(io, mark, self.base)\n"),
    ("R1 open resets the watermark instead of advancing it", "store.rs", "            wm.advance(&io, here)?;\n        }", "            wm.reset(&io, here, 0)?;\n        }"),
    ("R2 open skips the fsync after a scan or cut", "store.rs", "if scanned > 0 || torn_from.is_some() {", "if false {"),
    ("R3 roll skips fsync of the sealed pack", "store.rs", "        self.io\n            .sync_file(&w.file, &pack::pack_path(&self.dir, w.id))?;\n        let id =", "        let id ="),
    ("R4 checkpoint skips its sync", "store.rs", "let lens = self.sync_capture()?;", "let lens = self.writer().pack_lens();"),
    ("R5 index save skips the directory fsync", "index.rs", "    io.sync_dir(store)", "    Ok(())"),
    ("R6 index tmp file not fsynced", "index.rs", "    io.sync_file(&f, &tmp)?;\n", ""),
    ("R7 create_pack skips the packs/ directory fsync", "store.rs", "        io.sync_dir(&pack::pack_dir(store))\n    };", "        Ok(())\n    };"),
    ("R8 create_pack skips the file fsync", "store.rs", "        io.sync_file(&file, &path)?;\n        io.sync_dir(&pack::pack_dir(store))", "        io.sync_dir(&pack::pack_dir(store))"),
    ("R9 LOCK directory entry not fsynced", "store.rs", "            io.created(&lock_path);\n            io.sync_dir(&dir)?;", "            io.created(&lock_path);"),
    ("P1 missing packs not detected", "store.rs", ".filter(|p| !lens.contains_key(p))", ".filter(|_| false)"),
    ("P2 writer trusts a watermark ahead of the packs", "store.rs", ".filter(|m| *m <= here)", ".filter(|_| true)"),
    ("P3 short pack below the watermark not reported", "store.rs", "if dur != u64::MAX && len < dur {", "if false && dur != u64::MAX && len < dur {"),
    ("P4 append into a damaged last pack", "store.rs", "if !last_damaged && lens.get", "if lens.get"),
    ("P5 corrupt_synced never populated", "store.rs", "            recovery.corrupt_synced.push(r);", ""),
    ("P6 superseded regions not recognised", "store.rs", "if r.id.is_some_and(elsewhere) {", "if false {"),
    ("P7 acknowledgements ignored", "store.rs", "if ack::covers(&acked, r.pack, r.offset, r.len) {", "if false {"),
    ("C1 create_pack leaves a half-made file", "store.rs", "        let _ = fs::remove_file(&path);\n", ""),
    ("C2 create_pack overwrites a leftover pack that holds data", "store.rs", "if f.metadata()?.len() > PACK_HEADER_LEN {", "if false {"),
    ("V1 put dedups without confirming the stored copy", "store.rs", "if loc.verified || self.confirm(id, loc, data) {", "if true {"),
    ("V2 confirm compares nothing", "store.rs", "Codec::Raw => payload == data,", "Codec::Raw => true,"),
    ("V3 rebuild does not hash-verify records", "store.rs", "if record::verify(header, payload) {", "if true {"),
    ("V4 a verified record does not replace an unverified entry", "index.rs", "Entry::Occupied(mut o) if !o.get().verified => {", "Entry::Occupied(mut o) if false && !o.get().verified => {"),
    ("S1 stats not decremented on replace", "index.rs", "        self.blocks.fetch_sub(1, Relaxed);\n        self.ulen.fetch_sub(u64::from(loc.ulen), Relaxed);\n        self.stored.fetch_sub(stored_len(loc), Relaxed);", "        let _ = loc;"),
    ("S2 stats not added when replacing nothing", "index.rs", "            self.sub(&old);\n        }\n        self.add(&loc);", "            self.sub(&old);\n        } else {\n            self.add(&loc);\n        }"),
    ("S3 pack_bytes not updated on put", "store.rs", "        self.counters\n            .pack_bytes\n            .fetch_add(rec.len() as u64, Relaxed);", ""),
    ("H1 record header CRC not checked", "record.rs", "if crc32c::crc32c(&buf[..HCRC_AT]) != u32_at(buf, HCRC_AT) {", "if false {"),
    ("H2 record padding check removed", "record.rs", "if buf[5..8] != [0, 0, 0] {", "if false {"),
    ("H3 decoded length check removed", "record.rs", "Ok(v) if v.len() == header.ulen as usize => Ok(v),", "Ok(v) => Ok(v),"),
    ("H4 get skips the header-vs-index match", "store.rs", "if header.id != id || header.slen != loc.slen || header.ulen != loc.ulen {", "if false {"),
    ("H5 get skips the index length range check", "store.rs", "if !locate_ok(&loc) {\n            return Err(corrupt(\"index entry out of range\"));", "if false {\n            return Err(corrupt(\"index entry out of range\"));"),
    ("H6 index entries not validated at open", "store.rs", "if packs_ok && entries_ok {", "if packs_ok {"),
    ("B1 resync work bound removed", "pack.rs", "if *budget < u64::from(h.slen) {", "if false {"),
    ("F1 fd cache never evicts", "fdcache.rs", "while g.len() > self.cap {", "while false && g.len() > self.cap {"),
    ("F2 fd cache evicts the id just inserted", "fdcache.rs", ".filter(|(k, _)| **k != id)", ".filter(|(k, _)| **k == id)"),
    ("F3 no EMFILE retry", "fdcache.rs", "Err(e) if matches!(e.raw_os_error(), Some(23 | 24)) => {", "Err(e) if false && matches!(e.raw_os_error(), Some(23 | 24)) => {"),
    ("M2 discard unlinks the pack before the watermark is raised", "compact.rs", move_wm_after_unlink, None),
    ("N1 finish_compaction skips the new pack's fsync", "compact.rs", "            g.io.sync_file(target, &path)?;\n            g.io.sync_dir(&pack::pack_dir(g.dir))?;", "            g.io.sync_dir(&pack::pack_dir(g.dir))?;"),
    ("D1 discard skips the packs directory fsync after the unlink", "compact.rs", "        if let Err(e) = g.io.sync_dir(&pack::pack_dir(g.dir)) {\n            out.durability_error = Some(e.into());\n            return Ok(out);\n        }\n", ""),
    ("E7 discard skips its leading sync", "compact.rs", "-> Result<Discarded> {\n        self.sync()?;\n", "-> Result<Discarded> {\n"),
    ("E8 acknowledge_corruption skips the directory fsync after dropping index.cix", "store.rs", "            self.io.remove_file(&self.index_path())?;\n            self.io.sync_dir(&self.dir)?;\n", "            self.io.remove_file(&self.index_path())?;\n"),
    # Power loss across a whole collect (tests/power_collect.rs, MUT_PKG=cowfs-gc MUT_ARGS="--test power_collect").
    ("GM1 collect discards the new pack instead of the source", "cowfs-gc/src/lib.rs", "self.store.discard(rw.from, &rw.condemned)", "self.store.discard(rw.to, &rw.condemned)"),
    # GF1/GF2 are EQUIVALENT: live_blocks_with_root runs inner.sync() itself (db.rs:2141). Only GF1+GF2+walk-sync together
    # is observable (8 of 992 images, and only without the mid hook): defence in depth no test pins.
    # GA1/GA2 survive because power_collect already explores every state their fsyncs change; mark.bin bit rot is a separate known bug.
    ("GF1 freeze skips the metadata sync", "cowfs-gc/src/lib.rs", "        self.meta.sync()?;\n        // Listed after the sync", "        // Listed after the sync"),
    ("GF2 fresh roots skip the metadata sync", "cowfs-gc/src/lib.rs", "cowfs_meta::SnapshotId)>> {\n        self.meta.sync()?;\n", "cowfs_meta::SnapshotId)>> {\n"),
    ("GA1 mark cache written without fsync", "cowfs-gc/src/state.rs", "            f.write_all(&buf)?;\n            f.sync_data()?;", "            f.write_all(&buf)?;"),
    ("GA2 mark cache rename without directory fsync", "cowfs-gc/src/state.rs", "        fs::rename(&tmp, &self.path)?;\n        if let Some(parent) = self.path.parent() {\n            File::open(parent)?.sync_all()?;\n        }\n        Ok(())", "        fs::rename(&tmp, &self.path)?;\n        Ok(())"),
]

only = sys.argv[1:]
os.makedirs(mut, exist_ok=True)
subprocess.run(["rsync", "-a", "--delete", "--exclude", "target", "--exclude", ".git", f"{root}/", work + "/"], check=True)
base = f"{mut}/tm_base"
if not os.path.exists(base):
    env = dict(os.environ, CARGO_TARGET_DIR=base)
    subprocess.run(["cargo", "test", "-j4", "--no-run", *pkgs, *extra], cwd=work, env=env, check=True, capture_output=True)
out = open(f"{mut}/results.txt", "a")
for name, f, a, b in M:
    tag = name.split()[0]
    if only and tag not in only:
        continue
    for fn in os.listdir(src):
        shutil.copy(f"{src}/{fn}", f"{dst}/{fn}")
    shutil.copytree(f"{root}/crates/cowfs-gc/src", f"{work}/crates/cowfs-gc/src", dirs_exist_ok=True)
    target = f"{work}/crates/{f}" if "/" in f else f"{dst}/{f}"
    t = open(target).read()
    if callable(a):
        try:
            mutated = a(t)
        except ValueError:
            out.write(f"{name}: PATTERN NOT FOUND\n"); out.flush(); continue
    elif a not in t:
        out.write(f"{name}: PATTERN NOT FOUND\n"); out.flush(); continue
    else:
        mutated = t.replace(a, b, 1)
    open(target, "w").write(mutated)
    tgt = f"{mut}/tm_{tag}"
    shutil.rmtree(tgt, ignore_errors=True)
    subprocess.run(["cp", "-cR" if sys.platform == "darwin" else "-a", base, tgt], check=True)
    env = dict(os.environ, CARGO_TARGET_DIR=tgt)
    try:
        r = subprocess.run(["cargo", "test", "-j4", "--no-fail-fast", *pkgs, *extra], cwd=work, env=env, capture_output=True, text=True, timeout=900)
        txt = r.stdout + r.stderr
        failed = sorted(set(re.findall(r"^test (\S+) \.\.\. FAILED", txt, re.M)))
        if "could not compile" in txt:
            v = "COMPILE ERROR"
        elif failed:
            v = "KILLED by " + ", ".join(failed[:4]) + (f" (+{len(failed) - 4})" if len(failed) > 4 else "")
        else:
            v = "SURVIVED"
    except subprocess.TimeoutExpired:
        v = "KILLED (suite timed out after 900 s)"
    out.write(f"{name}: {v}\n"); out.flush()
    shutil.rmtree(tgt, ignore_errors=True)
out.write("DONE\n"); out.close()
