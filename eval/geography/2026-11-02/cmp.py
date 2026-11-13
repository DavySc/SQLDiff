import sys,collections
pre=sys.argv[1]
rs=dict(l.split()[:2] for l in open(pre+'.rs'))
ms={}
for l in open(pre+'.ms'):
    p=l.split(); ms[p[0]]=(p[1]=='ok' and p[3]=='1')
pg={}
for l in open(pre+'.pg'):
    p=l.strip().split(','); pg[p[0]]=(p[1]=='t' and p[2]=='t')
c=collections.Counter(); bad=[]
for k in ms:
    r=rs[k]=='accept'; c[(r,ms[k],pg.get(k))]+=1
    if r and not (ms[k] and pg.get(k)): bad.append(k)
for k,v in sorted(c.items()): print('rust,ms,pg',k,v)
print('FALSE EQUAL:',bad)
