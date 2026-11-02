import sys,collections
def load(f):
    d={}
    for l in open(f):
        l=l.rstrip('\n'); 
        if '|' not in l: continue
        k,v=l.rsplit('|',1); d[k.strip()]=v.strip()
    return d
t,p=load(sys.argv[1]),load(sys.argv[2])
bad=collections.defaultdict(list); ok=collections.Counter()
for k in t:
    kind=k.split()[0]
    if k not in p: bad[kind].append((k,t[k],'MISSING')); continue
    if t[k]==p[k]: ok[kind]+=1
    else: bad[kind].append((k,t[k],p[k]))
for kind in sorted(set(ok)|set(bad)):
    print(kind,'same',ok[kind],'diff',len(bad[kind]))
    for x in bad[kind][:4]: print('    ',x)
