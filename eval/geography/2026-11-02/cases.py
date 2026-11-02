import struct
def pt(x,y): return struct.pack('<dd',x,y)
def ring(pts): return struct.pack('<I',len(pts))+b''.join(pt(*p) for p in pts)
def poly(rings,t=3): return b'\x01'+struct.pack('<I',t)+struct.pack('<I',len(rings))+b''.join(ring(r) for r in rings)
sq_ccw=[(0,0),(1,0),(1,1),(0,1),(0,0)]
sq_cw=[(0,0),(0,1),(1,1),(1,0),(0,0)]
big_ccw=[(-170,-80),(170,-80),(170,80),(-170,80),(-170,-80)]
C={}
C['ccw']=poly([sq_ccw]); C['cw_small']=poly([sq_cw]); C['cw_big']=poly([[(-170,-80),(-170,80),(170,80),(170,-80),(-170,-80)]])
C['big_ccw']=poly([big_ccw])
C['unclosed']=poly([[(0,0),(1,0),(1,1),(0,1)]])
C['three_pts']=poly([[(0,0),(1,0),(0,0)]])
C['bowtie']=poly([[(0,0),(1,1),(1,0),(0,1),(0,0)]])
C['dup_vertex']=poly([[(0,0),(1,0),(1,0),(1,1),(0,1),(0,0)]])
C['lat91']=poly([[(0,0),(1,0),(1,91),(0,1),(0,0)]])
C['lon181']=poly([[(0,0),(181,0),(181,1),(0,1),(0,0)]])
C['hole_in']=poly([[(0,0),(10,0),(10,10),(0,10),(0,0)],[(2,2),(2,4),(4,4),(4,2),(2,2)]])
C['hole_out']=poly([[(0,0),(10,0),(10,10),(0,10),(0,0)],[(20,2),(20,4),(22,4),(22,2),(20,2)]])
C['hole_ccw']=poly([[(0,0),(10,0),(10,10),(0,10),(0,0)],[(2,2),(4,2),(4,4),(2,4),(2,2)]])
C['empty_poly']=poly([])
C['trailing']=poly([sq_ccw])+b'\x00'
C['bigendian']=b'\x00'+struct.pack('>I',3)+struct.pack('>I',1)+struct.pack('>I',5)+b''.join(struct.pack('>dd',*p) for p in sq_ccw)
C['z']=b'\x01'+struct.pack('<I',1003)+struct.pack('<I',1)+struct.pack('<I',5)+b''.join(struct.pack('<ddd',*p,0) for p in sq_ccw)
C['point']=b'\x01'+struct.pack('<I',1)+pt(3,4)
C['line']=b'\x01'+struct.pack('<I',2)+ring([(0,0),(1,1)])
C['line_1pt']=b'\x01'+struct.pack('<I',2)+ring([(0,0)])
C['mpoly']=b'\x01'+struct.pack('<I',6)+struct.pack('<I',2)+poly([sq_ccw])+poly([[(5,5),(6,5),(6,6),(5,6),(5,5)]])
C['mpoly_overlap']=b'\x01'+struct.pack('<I',6)+struct.pack('<I',2)+poly([sq_ccw])+poly([[(0.5,0.5),(2,0.5),(2,2),(0.5,2),(0.5,0.5)]])
C['mpoly_empty']=b'\x01'+struct.pack('<I',6)+struct.pack('<I',0)
C['nan']=b'\x01'+struct.pack('<I',1)+struct.pack('<dd',float('nan'),1)
C['gc']=b'\x01'+struct.pack('<I',7)+struct.pack('<I',1)+b'\x01'+struct.pack('<I',1)+pt(1,2)
C['mpoint']=b'\x01'+struct.pack('<I',4)+struct.pack('<I',2)+b'\x01'+struct.pack('<I',1)+pt(1,2)+b'\x01'+struct.pack('<I',1)+pt(3,4)
C['mline']=b'\x01'+struct.pack('<I',5)+struct.pack('<I',1)+b'\x01'+struct.pack('<I',2)+ring([(0,0),(1,1)])
C['trunc']=poly([sq_ccw])[:-3]
C['touch_vertex']=poly([[(0,0),(2,0),(2,2),(1,1),(0,2),(1,1),(0,0)]])
C['pole']=poly([[(0,89),(120,89),(240,89),(0,89)]])
C['antimeridian']=poly([[(179,0),(-179,0),(-179,1),(179,1),(179,0)]])
C['lon_wrap_edge']=poly([[(170,0),(-170,0),(-170,10),(170,10),(170,0)]])
if __name__=='__main__':
    import sys
    for k,v in C.items(): print(k, v.hex())
