import sys
cases=[c for c in open(sys.argv[1],encoding='utf-8').read().split('\n') if c!='']
def load(p,enc):
  d={}
  for l in open(p,encoding='utf-8',errors='replace'):
    l=l.strip('\r\n').lstrip('﻿')
    parts=l.split('|',2)
    if len(parts)!=3 or not parts[0].isdigit(): continue
    i,k,v=parts
    if v not in('NULL',) and not v.startswith('ERR'):
      b=bytes.fromhex(v); v=repr(b.decode(enc,errors='surrogatepass'))
    elif v.startswith('ERR'): v='ERR'
    d[(int(i),k)]=v
  return d
m=load(sys.argv[2],'utf-16-le'); p=load(sys.argv[3],'utf-8')
keys=sys.argv[4].split(',') if len(sys.argv)>4 else ['isjson','q','v','d']
bad=0
for i,c in enumerate(cases):
  for k in keys:
    a,b=m.get((i,k)),p.get((i,k))
    if a!=b:
      bad+=1; print(f"{i:2} {k:6} {c[:45]!r:50} MS={a} PG={b}")
print('differences',bad)
