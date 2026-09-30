#!/usr/bin/env python3
"""compare.py <dirA> <dirB> : compare target/ trees (dirA/target vs dirB/target) by relative path, print JSON."""
import json, os, re, sys, collections

SESS = re.compile(r"/s-[a-z0-9]+-[a-z0-9]+-[a-z0-9]+")
CGU = re.compile(r"\.[a-z0-9]{7}\.rcgu\.o$")
norm = lambda r: CGU.sub(".X.rcgu.o", SESS.sub("/s-X", r))


def kind(rel):
    p = rel.split("/")
    n = p[-1]
    if ".dSYM" in rel: return "dSYM"
    if "incremental" in p: return "incremental"
    if ".fingerprint" in p: return "fingerprint"
    if "build" in p and p.index("build") <= 2:
        return "build-out" if "out" in p else "build-script"
    if n.endswith(".rlib"): return "rlib"
    if n.endswith(".rmeta"): return "rmeta"
    if n.endswith(".o"): return "o"
    if n.endswith(".d"): return "dep-info"
    if n.endswith((".dylib", ".so")): return "dylib"
    if n.startswith(".") or n in ("CACHEDIR.TAG",): return "misc"
    return "binary-or-other"


def walk(root):
    files = {}
    for dp, dn, fn in os.walk(root):
        dn.sort()
        for f in sorted(fn):
            p = os.path.join(dp, f)
            st = os.lstat(p)
            if not os.path.isfile(p) or os.path.islink(p): continue
            files[norm(os.path.relpath(p, root))] = (p, st.st_size)
    return files


def cmpfile(pa, pb, sa, sb):
    """returns ('same'|'path'|'other'|'size', n residual differing bytes after mapping slot path A->B)"""
    a, b = open(pa, "rb").read(), open(pb, "rb").read()
    if a == b: return "same", 0
    if len(sa) == len(sb) and a.replace(sa, sb) == b: return "path", 0
    a = a.replace(sa, sb) if len(sa) == len(sb) else a
    if len(a) != len(b): return "size", len(b)
    return "other", sum(x != y for x, y in zip(a, b))


def main(da, db):
    fa, fb = walk(f"{da}/target"), walk(f"{db}/target")
    sa, sb = da.encode(), db.encode()
    S = lambda: collections.defaultdict(lambda: {"files": 0, "bytes": 0})
    tot, same, path, other, onlyA, onlyB = S(), S(), S(), S(), S(), S()
    ex = collections.defaultdict(list)
    resid = collections.Counter()
    for rel, (pa, sz) in fa.items():
        k = kind(rel)
        tot[k]["files"] += 1; tot[k]["bytes"] += sz
        if rel not in fb:
            onlyA[k]["files"] += 1; onlyA[k]["bytes"] += sz
            continue
        r, frac = cmpfile(pa, fb[rel][0], sa, sb)
        d = {"same": same, "path": path}.get(r, other)
        d[k]["files"] += 1; d[k]["bytes"] += sz
        if r in ("other", "size"):
            resid[k] += frac
            if len(ex[k]) < 3: ex[k].append((rel, r, frac, sz))
    for rel, (pb, sz) in fb.items():
        if rel not in fa:
            k = kind(rel); onlyB[k]["files"] += 1; onlyB[k]["bytes"] += sz
    T = lambda d: {"files": sum(v["files"] for v in d.values()), "bytes": sum(v["bytes"] for v in d.values())}
    out = {"A": da, "B": db, "A_total": T(tot), "same": T(same), "path_only_diff": T(path), "content_diff": T(other),
           "only_in_A": T(onlyA), "only_in_B": T(onlyB),
           "by_kind": {k: {"total": tot[k], "same": same[k], "path_only": path[k], "diff": other[k], "onlyA": onlyA[k], "onlyB": onlyB[k]}
                       for k in sorted(set(tot) | set(onlyB))},
           "residual_diff_bytes_by_kind": dict(resid), "residual_diff_bytes": sum(resid.values()), "diff_examples": ex}
    return out


if __name__ == "__main__":
    print(json.dumps(main(os.path.realpath(sys.argv[1]), os.path.realpath(sys.argv[2])), indent=1))
