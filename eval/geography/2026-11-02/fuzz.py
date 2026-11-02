import random,struct,sys
from cases import C
from cases2 import D
random.seed(int(sys.argv[1]) if len(sys.argv)>1 else 1)
base=[v for v in list(C.values())+list(D.values()) if len(v)>=9]
special=[0.0,-0.0,1e-310,90.0,-90.0,180.0,-180.0,90.0000000001,180.0000000001,float('nan'),float('inf'),1e300,-1e300,179.99999999999997]
def mutate(b):
    b=bytearray(b); k=random.randrange(6)
    if k==0 and len(b)>10:  # flip bytes
        for _ in range(random.randint(1,3)): b[random.randrange(len(b))]=random.randrange(256)
    elif k==1: b=b[:random.randrange(len(b))]
    elif k==2: b+=bytes(random.randrange(256) for _ in range(random.randint(1,9)))
    elif k==3 and len(b)>=24: # replace a double
        i=random.randrange(9,len(b)-8); b[i:i+8]=struct.pack('<d',random.choice(special))
    elif k==4 and len(b)>=9: # tweak a u32
        i=random.choice([1,5,9]); 
        if i+4<=len(b): b[i:i+4]=struct.pack('<I',random.choice([0,1,2,3,4,5,6,7,8,1000,3,0xffffffff]))
    else: # byte order / type
        if len(b)>5: b[0]=random.choice([0,1,2]); b[1:5]=struct.pack('<I',random.choice([1,2,3,4,5,6,7,8,9,1001,1003,0x20000003]))
    return bytes(b)
def rnd_poly():
    n=random.choice([4,5,6]); 
    x0,y0=random.uniform(-179,179),random.uniform(-89,89)
    pts=[(x0+random.uniform(-1,1),y0+random.uniform(-1,1)) for _ in range(n-1)]
    pts.append(pts[0] if random.random()<.8 else (0,0))
    return b'\x01'+struct.pack('<III',3,1,len(pts))+b''.join(struct.pack('<dd',*p) for p in pts)
N=int(sys.argv[2]) if len(sys.argv)>2 else 1200
for i in range(N):
    b=rnd_poly() if random.random()<.3 else random.choice(base)
    for _ in range(random.randint(0,2)): b=mutate(b)
    print(f"f{i}",b.hex() or '-')
