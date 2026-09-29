"""Independent check of the spike tool: file count, raw bytes, whole-file dedup bytes.

Usage: validate.py <root> [<root> ...]   (each root is one slot, walked like the tool does)
"""
import hashlib, os, sys

seen_inodes = set()
seen_hashes = set()
files = raw = uniq = links = syms = 0
for root in sys.argv[1:]:
    for dp, dn, fn in os.walk(root, followlinks=False):
        for n in fn:
            p = os.path.join(dp, n)
            st = os.lstat(p)
            if os.path.islink(p):
                syms += 1
                continue
            if not os.path.isfile(p):
                continue
            if st.st_nlink > 1:
                k = (st.st_dev, st.st_ino)
                if k in seen_inodes:
                    links += 1
                    continue
                seen_inodes.add(k)
            data = open(p, "rb").read()
            files += 1
            raw += len(data)
            h = hashlib.sha256(data).digest()
            if h not in seen_hashes:
                seen_hashes.add(h)
                uniq += len(data)
print(f"files={files} raw={raw / 2**30:.4f} GiB whole_file_unique={uniq / 2**30:.4f} GiB symlinks={syms} hardlink_dupes={links}")
