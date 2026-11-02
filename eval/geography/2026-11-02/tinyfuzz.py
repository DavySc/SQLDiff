import math,random,struct,sys
seed=int(sys.argv[1]); N=int(sys.argv[2]); rnd=random.Random(seed)
def enc(rings):
    b=b'\x01'+struct.pack('<II',3,len(rings))
    for r in rings: b+=struct.pack('<I',len(r))+b''.join(struct.pack('<dd',*p) for p in r)
    return b
for i in range(N):
    cx=rnd.uniform(-120,120); cy=rnd.uniform(-60,60); r=rnd.choice([0.01,0.5,5])
    n=rnd.choice([4,6,12]); angs=sorted(rnd.uniform(0,2*math.pi) for _ in range(n))
    pts=[(cx+r*math.cos(a)/math.cos(math.radians(cy)),cy+r*math.sin(a)) for a in angs]
    out=[]
    for k,p in enumerate(pts):
        out.append(p)
        if rnd.random()<.5:
            d=math.degrees(10**rnd.uniform(-10,-5)) # radians -> degrees
            mode=rnd.random()
            if mode<.5: # nearby extra vertex along outward/inward direction
                a=rnd.uniform(0,2*math.pi)
                out.append((p[0]+d*math.cos(a),p[1]+d*math.sin(a)))
            else: # spike back to near p
                a=rnd.uniform(0,2*math.pi)
                out.append((p[0]+d*math.cos(a),p[1]+d*math.sin(a))); out.append((p[0]+d*0.5*math.cos(a)+d*0.1,p[1]+d*0.5*math.sin(a)))
    out.append(out[0])
    print(f"t{seed}_{i}",enc([out]).hex())
