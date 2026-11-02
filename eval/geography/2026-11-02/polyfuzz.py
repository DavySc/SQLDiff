import math,random,struct,sys
seed=int(sys.argv[1]); N=int(sys.argv[2]); rnd=random.Random(seed)
def star(cx,cy,r,n,ccw=True,jag=0.5):
    angs=sorted(rnd.uniform(0,2*math.pi) for _ in range(n))
    pts=[(cx+ (r*(1-jag*rnd.random()))*math.cos(a)/max(0.2,math.cos(math.radians(cy))), cy+(r*(1-jag*rnd.random()))*math.sin(a)) for a in angs]
    if not ccw: pts.reverse()
    return pts+[pts[0]]
def enc_poly(rings):
    b=b'\x01'+struct.pack('<II',3,len(rings))
    for r in rings: b+=struct.pack('<I',len(r))+b''.join(struct.pack('<dd',*p) for p in r)
    return b
def mk():
    cy=rnd.choice([rnd.uniform(-75,75),rnd.uniform(-89,89),rnd.uniform(-20,20)])
    cx=rnd.choice([rnd.uniform(-179,179),rnd.uniform(-60,60),rnd.choice([179.5,-179.5,0])])
    r=rnd.choice([0.05,0.5,2,8,20,45,80])
    ccw=rnd.random()<.6
    ext=star(cx,cy,r,rnd.choice([4,5,8,20,60]),ccw)
    rings=[ext]
    if rnd.random()<.4:
        hole=star(cx,cy,r*0.2,rnd.choice([4,6,12]),rnd.random()<.2 if ccw else rnd.random()<.8,jag=0.2)
        rings.append(hole)
    if rnd.random()<.15 and len(ext)>5:  # mutate: swap/move a vertex
        i=rnd.randrange(1,len(ext)-2); ext[i]=(ext[i][0]+rnd.uniform(-r,r),ext[i][1]+rnd.uniform(-r,r))
    kind=rnd.random()
    if kind<.2:
        c2=star(cx+rnd.choice([3*r,0.3*r]),cy,r*0.5,6,ccw)
        return b'\x01'+struct.pack('<II',6,2)+enc_poly(rings)+enc_poly([c2])
    return enc_poly([[(max(-180,min(180,x)),max(-90,min(90,y))) for x,y in r_] for r_ in rings])
for i in range(N): print(f"p{seed}_{i}",mk().hex())
