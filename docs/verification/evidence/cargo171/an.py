import sys,glob,collections,re
d=sys.argv[1]; fs=sorted(glob.glob(d+'/*.sha'))
h=collections.defaultdict(list); 
for f in fs:
    seen={}
    for l in open(f):
        a,p=l.rstrip('\n').split('  ',1); seen[p]=a
    for p,a in seen.items(): h[p].append(a)
n=len(fs); cls=collections.defaultdict(lambda:[0,0,[]])
def kind(p):
    p=re.sub(r'-[0-9a-f]{16}','-H',p)
    m=re.search(r'\.(\w+)$',p); ext=m.group(1) if m else 'noext'
    top='/'.join(p.split('/')[:3]) if '.fingerprint' in p else ''
    return ('fingerprint ' if '.fingerprint' in p else 'incremental ' if '/incremental/' in p else '')+ext
for p,v in h.items():
    ident=len(v)==n and len(set(v))==1
    k=kind(p); cls[k][0]+=1; cls[k][1]+=ident
    if not ident: cls[k][2].append(p)
tot=len(h); idn=sum(c[1] for c in cls.values())
print(f"builds={n} files={tot} identical={idn} ({100*idn/tot:.1f}%)")
for k,(t,i,diff) in sorted(cls.items()): print(f"  {k:28s} {i}/{t} identical", ('e.g. '+diff[0]) if diff else '')
