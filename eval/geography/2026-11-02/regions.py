"""Region probe: for polygon WKB cases, the area and whether sample points
intersect the stored value, on SQL Server geography and PostGIS geography.
usage: regions.py cases.txt out.txt   (cases: name hex) -> out: name status
status: same | differs | n/a (not polygonal / an engine rejected it)."""
import struct,subprocess,sys,os,random,re
MS=os.environ.get('MS','/tmp/claude-0/w/ms.sh'); PG=os.environ.get('PG','/tmp/claude-0/w/pg.sh')
def parse(b):
    """polygon rings as lists of (lon,lat) for types 3,6 (little endian), else None"""
    try:
        if b[0]!=1: return None
        t=struct.unpack_from('<I',b,1)[0]; at=5; rings=[]
        def poly(at):
            n=struct.unpack_from('<I',b,at)[0]; at+=4; out=[]
            for _ in range(n):
                m=struct.unpack_from('<I',b,at)[0]; at+=4
                out.append([struct.unpack_from('<dd',b,at+16*i) for i in range(m)]); at+=16*m
            return out,at
        if t==3:
            r,at=poly(at); return r
        if t==6:
            k=struct.unpack_from('<I',b,at)[0]; at+=4; out=[]
            for _ in range(k):
                assert b[at]==1 and struct.unpack_from('<I',b,at+1)[0]==3; r,at=poly(at+5); out+=r
            return out
    except Exception: return None
def samples(rings):
    pts=[p for r in rings for p in r]
    xs=[p[0] for p in pts]; ys=[p[1] for p in pts]
    x0,x1,y0,y1=min(xs),max(xs),min(ys),max(ys)
    rnd=random.Random(len(pts))
    s=[(x0+(x1-x0)*rnd.random(),y0+(y1-y0)*rnd.random()) for _ in range(40)]
    s+=[(x0-1,y0-1),(x1+1,y1+1),((x0+x1)/2,(y0+y1)/2),(x0+(x1-x0)*0.5,y1+0.5)]
    s+=[(max(-179,min(179,x0+90)),max(-80,min(80,y0-60))),(max(-179,min(179,x0-90)),max(-80,min(80,y1+30)))]
    return [(max(-179.9,min(179.9,x)),max(-89.9,min(89.9,y))) for x,y in s]
cases=[l.split() for l in open(sys.argv[1]) if len(l.split())>=2]
work=[]
for k,h in cases:
    r=parse(bytes.fromhex(h)) if h!='-' else None
    if r: work.append((k,h,samples(r)))
q=[];p=["create temp table r(k text, area float8, bits text);"]
for k,h,s in work:
    cols=",".join(f"CAST(@g.STIntersects(geography::Point({y!r},{x!r},4326)) AS int)" for x,y in s)
    q.append(f"BEGIN TRY DECLARE @g geography=geography::STGeomFromWKB(0x{h},4326); SELECT '{k}', @g.STArea(), CONCAT({','.join(f'CAST(@g.STIntersects(geography::Point({y!r},{x!r},4326)) AS int)' for x,y in s)}); END TRY BEGIN CATCH SELECT '{k}', -1, 'ERR'; END CATCH\nGO")
    bits="||".join(f"(case when ST_Intersects(g, ST_SetSRID(ST_MakePoint({x!r},{y!r}),4326)::geography) then '1' else '0' end)" for x,y in s)
    p.append(f"do $$ declare g geography; begin g := ST_GeomFromWKB('\\x{h}'::bytea,4326)::geography; insert into r values ('{k}', ST_Area(g), {bits}); exception when others then insert into r values ('{k}', -1, 'ERR'); end $$;")
p.append("copy r to stdout with (format csv);")
ms={}
out=subprocess.run([MS,'-i','/dev/stdin'],input="\n".join(q),capture_output=True,text=True).stdout
for l in out.splitlines():
    t=l.split()
    if len(t)==3 and t[0] in {w[0] for w in work}: ms[t[0]]=(float(t[1]),t[2])
pg={}
out=subprocess.run([PG,'-q'],input="\n".join(p),capture_output=True,text=True).stdout
for l in out.splitlines():
    t=l.split(',')
    if len(t)==3: pg[t[0]]=(float(t[1]),t[2])
res=[]
for k,h in cases:
    if k not in ms or k not in pg or ms[k][1]=='ERR' or pg[k][1]=='ERR': res.append(f"{k} n/a"); continue
    a,b=ms[k][0],pg[k][0]
    ok=ms[k][1]==pg[k][1] and (abs(a-b)<=0.01*max(abs(a),abs(b),1.0))
    res.append(f"{k} {'same' if ok else 'differs'}")
open(sys.argv[2],'w').write("\n".join(res)+"\n")
