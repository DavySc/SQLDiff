import random,struct,subprocess,sys
random.seed(7)
def lit():
    d=random.choice([1,3,6,8,10,12,15,17,20,25])
    v=random.uniform(-1,1)*random.choice([1,90])
    s=f"{v:.{d}f}"
    return s
cases=[(lit(),lit()) for _ in range(600)]
cases=[(a,b) for a,b in cases if abs(float(a))<=90 and abs(float(b))<=180]
cases+= [('90','180'),('-90','-180'),('0','0'),('0.1','0.2'),('37.765786','-122.504086'),('1e0','2e0') ]
ms=[];pg=[]
q=[]
for i,(la,lo) in enumerate(cases):
    q.append(f"SELECT {i}, convert(varchar(64), geography::Point({la},{lo},4326).STAsBinary(),2);\nGO")
r=subprocess.run(['/tmp/claude-0/w/ms.sh','-i','/dev/stdin'],input="\n".join(q),capture_output=True,text=True).stdout
got={}
for l in r.splitlines():
    p=l.split()
    if len(p)==2 and p[0].isdigit(): got[int(p[0])]=p[1].upper()
sql="select "+" ,".join([])
out=[]
for i,(la,lo) in enumerate(cases):
    out.append(f"select {i}, upper(encode(ST_AsBinary(ST_SetSRID(ST_MakePoint({lo},{la}),4326)::geography),'hex'));")
r=subprocess.run(['/tmp/claude-0/w/pg.sh'],input="\n".join(out),capture_output=True,text=True).stdout
pgg={}
for l in r.splitlines():
    a,b=l.split('|'); pgg[int(a)]=b
bad=0
for i,(la,lo) in enumerate(cases):
    py=('0101000000'+struct.pack('<d',float(lo)).hex()+struct.pack('<d',float(la)).hex()).upper()
    if not(got.get(i)==py==pgg.get(i)):
        bad+=1; print('DIFF',la,lo,got.get(i),py,pgg.get(i))
print(len(cases),'cases',bad,'differ')
