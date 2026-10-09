#!/usr/bin/env python3
"""Gate g5, the differential report: xfstests-generic, native ext4 vs cowfs.

Usage:
  g5_diff.py report --run DIR [--mode acceptance|diagnostic] [--json OUT] [--md OUT]

No root and no mounts here. `bench/g5_root.sh` does all the privileged work and
leaves plain text evidence per arm and case; this module reads it, applies the
validity rules in docs/g5-harness-redesign.md and decides. Design and the reasons
for each rule: that document.

Exit codes, the set bench/compare.py uses:
  0  PASS (acceptance) or DIAGNOSTIC (valid, any other id set)
  1  FAIL          native passed a case that cowfs failed or timed out
  2  UNMEASURABLE  an arm has no clean PASS, or acceptance without all-PASS
  3  INVALID       a validity rule failed
"""

import argparse
import json
import re
import sys
from collections import Counter
from pathlib import Path

import xfstests_gate as gate

ARM_FSTYP = {"native": "ext4", "cowfs": "fuse", "control": "fuse"}
TIMEOUT_RCS = (124, 137)
RESULT_LINE = r"^{case}\s+(.*)$"
FSTYP_RE = re.compile(r"^FSTYP\s+-- (\S+)", re.M)
NOT_RUN_RE = re.compile(r"\[not run\]\s*(.*)")

# (class, regex) in priority order; first match wins.
REASON_CLASSES = (
    ("harness", r"SCRATCH_DEV|SCRATCH_MNT|not built|dbench|command not found"),
    ("inherent_fuse", r"block disk|block size"),
    ("by_fstype", r"by test filesystem type|by this filesystem type|does not define maximum ACL"),
    ("missing_feature", r"xfs_io|chattr|renameat2|O_TMPFILE|creation time|not supported"),
)


def classify_reason(reason):
    for cls, rx in REASON_CLASSES:
        if re.search(rx, reason or ""):
            return cls
    return "unclassified"


def parse_console(text, rc, case):
    """One single-case `check` run -> status. Only the suite's own grammar is read.

    A not-run case also prints `Passed all 1 tests`, so the `[not run]` bracket
    decides, never the summary.
    """
    m = FSTYP_RE.search(text)
    out = {"status": "NO_RESULT", "reason": None, "fstyp": m.group(1) if m else None,
           "why": None}
    if rc in TIMEOUT_RCS:
        out["status"], out["why"] = "TIMEOUT", f"rc {rc} from timeout"
        return out
    ids, _, summaries, problem = gate.suite_witness(text)
    if problem:
        out["why"] = problem
        return out
    if ids != [case]:
        out["why"] = f"the suite named {ids}, wanted {[case]}"
        return out
    line = re.search(RESULT_LINE.format(case=re.escape(case)), text, re.M)
    body = line.group(1) if line else ""
    if not summaries:
        out["why"] = "no summary line"
        return out
    last = summaries[-1]
    if last.startswith("Failed"):
        out["status"], out["why"] = "FAIL", last
        return out
    if last != "Passed all 1 tests":
        out["why"] = f"unexpected summary {last!r}"
        return out
    nr = NOT_RUN_RE.search(body)
    if nr:
        if f"Not run: {case}" not in text:
            out["why"] = "[not run] without the suite's Not run line"
            return out
        out["status"], out["reason"] = "NOT_RUN", nr.group(1).strip()
        return out
    if rc != 0:
        out["why"] = f"the suite reported a pass but exited {rc}"
        return out
    if not re.match(r"\d+s\b", body):
        out["why"] = f"result line {body!r} is not a timed pass"
        return out
    out["status"] = "PASS"
    return out


def parse_identity(text):
    out = {}
    for ln in (text or "").splitlines():
        k, sep, v = ln.partition("=")
        if sep:
            out[k.strip()] = v.strip()
    return out


def check_identity(arm, ident):
    """Measured identity of the mount a case ran on. Empty list means valid."""
    p = []
    if not ident:
        return ["identity: none recorded"]
    parts = ident.get("target_line", "").split("|")
    if len(parts) != 3:
        return [f"identity: target_line {ident.get('target_line')!r} is not fstype|source|target"]
    fstype, source, target = parts
    dev = ident.get("test_dev", "")
    if source != dev:
        p.append(f"identity: mount source {source!r} is not TEST_DEV {dev!r}")
    if target != ident.get("test_dir"):
        p.append(f"identity: mount target {target!r} is not TEST_DIR {ident.get('test_dir')!r}")
    if ident.get("source_mounts") != "1":
        p.append(f"identity: {ident.get('source_mounts')} mounts have source {dev!r}, need exactly 1")
    if arm == "native":
        if fstype != "ext4":
            p.append(f"identity: native arm is {fstype!r}, not ext4")
        if not source.startswith("/dev/loop"):
            p.append(f"identity: native source {source!r} is not a loop device")
        if not ident.get("backing"):
            p.append("identity: native loop device has no backing file")
    else:
        if not fstype.startswith("fuse"):
            p.append(f"identity: {arm} arm is {fstype!r}, not a FUSE mount")
        if source != "cowfs":
            p.append(f"identity: {arm} source {source!r} is not cowfs")
        snap = ident.get("snapshot", "")
        if not snap or ident.get("fsroot") != "/" + snap:
            p.append(f"identity: {arm} mount root {ident.get('fsroot')!r} is not its fresh "
                     f"snapshot {snap!r}")
    return p


LOG_RE = re.compile(r"^op=(\w+) moved=(\d) caller=(.*?) args=")
CHECK_CALLER_RE = re.compile(r"(^|[\s/])check(\s|$)")


def count_cycles(text):
    """Calls the case itself made on TEST_DEV or TEST_DIR, from the shim's log.

    The shim logs every such call with its caller. The suite's own wrap-up runs in
    `check`, not in the case script, so it is not a cycle. Anything else is, and a
    line this cannot parse counts too, so a doubtful log never reads as clean.
    """
    n = 0
    for ln in (text or "").splitlines():
        if not ln.strip():
            continue
        m = LOG_RE.match(ln)
        if m and CHECK_CALLER_RE.search(m.group(3)) and "tests/" not in m.group(3):
            continue
        n += 1
    return n


def make_record(arm, case, console, rc, identity_text, cycle_text, residue_text=None):
    parsed = parse_console(console, rc, case)
    ident = parse_identity(identity_text)
    problems = check_identity(arm, ident)
    want = ARM_FSTYP[arm]
    if parsed["fstyp"] != want:
        problems.append(f"header: FSTYP {parsed['fstyp']!r}, the {arm} arm needs {want!r}")
    if residue_text is not None and residue_text.strip():
        problems.append("bare mount directory written, the case did not stay on its snapshot: "
                        + " ".join(residue_text.split())[:120])
    cycles = count_cycles(cycle_text)
    status = parsed["status"]
    if status == "PASS" and cycles and arm != "native":
        status = "PASS_EMULATED"
    reason = parsed["reason"]
    return {"arm": arm, "case": case, "status": status, "reason": reason,
            "class": classify_reason(reason) if reason else None, "cycles": cycles,
            "erofs": "Read-only file system" in console,
            "why": parsed["why"], "problems": problems, "identity": ident}


def pair_label(n, c):
    ns, cs = n["status"], c["status"]
    if "NO_RESULT" in (ns, cs):
        return "invalid"
    if ns == "PASS":
        return {"PASS": "both_pass", "PASS_EMULATED": "emulated", "FAIL": "worse",
                "TIMEOUT": "worse", "NOT_RUN": "gap"}[cs]
    if cs in ("PASS", "PASS_EMULATED"):
        return "better"
    if ns == "FAIL" and cs == "FAIL":
        return "both_fail"
    if ns == "NOT_RUN" and cs == "NOT_RUN":
        return "both_not_run"
    return "other"


def build_receipt(native, cowfs, control, meta, pin, mode, requested, expect_daemon_sha256=None):
    bad = []
    for k in ("tree_head", "check_sha256", "cowfs_profile"):
        if not meta.get(k):
            bad.append(f"meta: {k} was not recorded before the run")
    if meta.get("tree_porcelain"):
        bad.append(f"tree: not clean ({meta['tree_porcelain']!r})")
    if meta.get("tree_head") and meta["tree_head"] != pin.get("tree_sha"):
        bad.append(f"tree: head {meta['tree_head']} is not the reviewed {pin.get('tree_sha')}")
    want_check = (pin.get("runner") or {}).get("check")
    if meta.get("check_sha256") and meta["check_sha256"] != want_check:
        bad.append("tree: check executor bytes are not the reviewed ones")
    for cid, sha in (pin.get("cases") or {}).items():
        got = meta.get(f"case_sha.{cid}")
        if f"generic/{cid}" in requested and got != sha:
            bad.append(f"tree: case {cid} bytes {got} are not the reviewed {sha}")
    reviewed = {f"generic/{i}" for i in (pin.get("cases") or {})}
    if mode == "acceptance":
        if set(requested) != reviewed:
            bad.append("acceptance: the requested ids are not exactly the reviewed set")
        if not re.fullmatch(r"[0-9a-f]{40}", meta.get("cowfs_rev") or ""):
            bad.append(f"acceptance: cowfs_rev {meta.get('cowfs_rev')!r} is not a recorded git "
                       "revision of the tree that built the daemon")
        if meta.get("noshim"):
            bad.append("acceptance: the run used NOSHIM, so mount cycles were not logged")
        if not expect_daemon_sha256:
            bad.append("acceptance: no expected daemon sha256 was given (--daemon-sha256)")
        elif meta.get("cowfs_bin_sha256") != expect_daemon_sha256:
            bad.append(f"acceptance: daemon sha256 {meta.get('cowfs_bin_sha256')} is not the "
                       f"expected {expect_daemon_sha256}")
        if meta.get("cowfs_profile") != "release":
            bad.append(f"acceptance: cowfs daemon is a {meta.get('cowfs_profile')!r} build, need release")
    if (control is None or control["status"] != "FAIL" or control["problems"]
            or not control.get("erofs")):
        bad.append("control: the read-only negative control did not fail for the read-only reason "
                   f"({None if control is None else control['status']})")
    nids, cids = [r["case"] for r in native], [r["case"] for r in cowfs]
    for name, ids in (("native", nids), ("cowfs", cids)):
        if len(set(ids)) != len(ids):
            bad.append(f"coverage: {name} arm has a duplicated case")
        if sorted(ids) != sorted(requested):
            bad.append(f"coverage: {name} arm ran {len(ids)} ids, requested {len(requested)}")
    for r in native + cowfs:
        for pr in r["problems"]:
            bad.append(f"{r['arm']} {r['case']}: {pr}")
        if r["status"] == "NO_RESULT":
            bad.append(f"{r['arm']} {r['case']}: no result ({r.get('why')})")
    dirs = {r["arm"]: r["identity"].get("test_dir") for r in (native[:1] + cowfs[:1])
            if r.get("identity")}
    if len(dirs) == 2 and dirs["native"] == dirs["cowfs"]:
        bad.append("arms: native and cowfs share one TEST_DIR")
    by_case = {r["case"]: r for r in cowfs}
    by_native = {r["case"]: r for r in native}
    labels = {}
    for n in native:
        c = by_case.get(n["case"])
        if c:
            labels[n["case"]] = pair_label(n, c)
    counts = Counter(labels.values())
    gap = {}
    for case, lab in labels.items():
        if lab == "gap":
            gap.setdefault(by_case[case]["class"], []).append(case)
    n_pass = sum(1 for r in native if r["status"] == "PASS")
    c_pass = sum(1 for r in cowfs if r["status"] == "PASS")
    worse = sorted(k for k, v in labels.items() if v == "worse")
    if bad:
        verdict, code = "INVALID", 3
    elif not n_pass or not c_pass:
        verdict, code = "UNMEASURABLE", 2
    elif worse:
        verdict, code = "FAIL", 1
    elif mode == "acceptance":
        ok = counts.get("both_pass") == len(requested)
        verdict, code = ("PASS", 0) if ok else ("UNMEASURABLE", 2)
    else:
        verdict, code = "DIAGNOSTIC", 0
    return {
        "verdict": verdict, "exit": code, "mode": mode, "problems": bad,
        "counts": dict(counts), "requested": len(requested),
        "native_status": dict(Counter(r["status"] for r in native)),
        "cowfs_status": dict(Counter(r["status"] for r in cowfs)),
        "other": {k: f"native {by_native[k]['status']}, cowfs {by_case[k]['status']}"
                  for k, v in labels.items() if v == "other"},
        "worse": worse, "gap": {k: sorted(v) for k, v in gap.items()},
        "emulated": sorted(k for k, v in labels.items() if v == "emulated"),
        "better": sorted(k for k, v in labels.items() if v == "better"),
        "cowfs_not_run_by_class": dict(Counter(r["class"] for r in cowfs if r["status"] == "NOT_RUN")),
        "native_not_run_by_class": dict(Counter(r["class"] for r in native if r["status"] == "NOT_RUN")),
        "unclassified": sorted({r["reason"] for r in native + cowfs if r["class"] == "unclassified"}),
        "labels": labels, "meta": meta,
        "control": None if control is None else {"status": control["status"],
                                                  "problems": control["problems"]},
        "label": ("acceptance evidence" if verdict == "PASS" else
                  "diagnostic only, not acceptance" if verdict == "DIAGNOSTIC" else verdict),
    }


def _read(p):
    return p.read_text(errors="replace") if p.is_file() else ""


def load_arm(run, arm, ids):
    recs = []
    for case in ids:
        d = run / arm / case.split("/")[1]
        if not (d / "console.txt").is_file():
            recs.append({"arm": arm, "case": case, "status": "NO_RESULT", "reason": None,
                         "class": None, "cycles": 0, "why": "no console recorded",
                         "problems": [f"missing evidence {d}"], "identity": {}})
            continue
        rc_txt = _read(d / "rc").strip()
        rc = int(rc_txt) if rc_txt.lstrip("-").isdigit() else -1
        recs.append(make_record(arm, case, _read(d / "console.txt"), rc,
                                _read(d / "identity.txt"), _read(d / "mountcycle.log"),
                                (d / "residue.txt").read_text(errors="replace")
                                if (d / "residue.txt").is_file() else None))
    return recs


def load_run(run):
    run = Path(run)
    meta = parse_identity(_read(run / "meta.txt"))
    ids = [ln.strip() for ln in _read(run / "cases.txt").splitlines() if ln.strip()]
    ctl = load_arm(run, "control", ["generic/005"])[0] if (run / "control").is_dir() else None
    return {"meta": meta, "requested": ids, "native": load_arm(run, "native", ids),
            "cowfs": load_arm(run, "cowfs", ids), "control": ctl}


def render_md(r):
    L = [f"# g5 differential: {r['verdict']} ({r['label']})", "",
         f"Requested {r['requested']} cases, mode {r['mode']}.",
         f"Native: {r['native_status']}.", f"Cowfs: {r['cowfs_status']}.",
         f"Pairs: {r['counts']}.", ""]
    L.append(f"Worse than native ({len(r['worse'])}): {' '.join(r['worse']) or 'none'}")
    for cls, ids in sorted(r["gap"].items()):
        L.append(f"Gap, native passes and cowfs does not run, {cls} ({len(ids)}): {' '.join(ids)}")
    L.append(f"Other combinations, not pass and not worse ({len(r['other'])}): {r['other']}")
    L.append(f"Emulated mount cycle on cowfs: {' '.join(r['emulated']) or 'none'}")
    L.append(f"Cowfs not-run by class: {r['cowfs_not_run_by_class']}")
    L.append(f"Native not-run by class: {r['native_not_run_by_class']}")
    if r["unclassified"]:
        L.append(f"Unclassified reasons: {r['unclassified']}")
    L.append(f"Control: {r['control']}")
    if r["meta"].get("noshim"):
        L.append("WARNING: NOSHIM run, mount cycles were not logged, so no PASS here is clean.")
    for p in r["problems"][:40]:
        L.append(f"INVALID: {p}")
    return "\n".join(L) + "\n"


def report(args):
    pin, err = gate.reviewed_pin()
    if err:
        print(f"INVALID: {err}", file=sys.stderr)
        return 3
    run = load_run(args.run)
    r = build_receipt(run["native"], run["cowfs"], run["control"], run["meta"], pin,
                      args.mode, run["requested"], args.daemon_sha256)
    r["records"] = {"native": run["native"], "cowfs": run["cowfs"], "control": run["control"]}
    Path(args.json or Path(args.run) / "receipt.json").write_text(json.dumps(r, indent=1, default=str))
    Path(args.md or Path(args.run) / "receipt.md").write_text(render_md(r))
    print(render_md(r))
    return r["exit"]


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    rp = sub.add_parser("report")
    rp.add_argument("--run", required=True)
    rp.add_argument("--mode", choices=("acceptance", "diagnostic"), default="diagnostic")
    rp.add_argument("--daemon-sha256", help="expected sha256 of cowfs-daemon (acceptance needs it)")
    rp.add_argument("--json")
    rp.add_argument("--md")
    args = ap.parse_args(argv)
    return report(args)


if __name__ == "__main__":
    sys.exit(main())
