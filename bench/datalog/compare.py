import sys,os,collections
dump,out=sys.argv[1],sys.argv[2]
def rd(f):
    p=os.path.join(out,f+".csv")
    d={}
    for l in open(p):
        c,j,v=l.split(); d[(int(c),int(j))]=int(v)
    return d
fc,jp,hit,rch=rd("ans_fc"),rd("ans_jump"),rd("ans_hit"),rd("ans_reach")
o=lambda d,k: str(d[k]) if k in d else "-"
n=bad=reach_n=0
for l in open(os.path.join(dump,"rust_answer.tsv")):
    c,j,f,jm,h,r=l.split(); k=(int(c),int(j)); n+=1
    mine_h = "-" if k not in hit else ("any" if hit[k]==-1 else str(hit[k]))
    ok = f==o(fc,k) and jm==o(jp,k) and h==mine_h
    if r!="-":
        reach_n+=1
        mine_r = "none" if rch.get(k)==-1 else o(rch,k)
        ok = ok and r==mine_r
    if not ok:
        bad+=1
        if bad<=5: print("MISMATCH",l.strip(),"| dl:",o(fc,k),o(jp,k),mine_h,rch.get(k))
print(f"edits {n}, reach answers {reach_n}, mismatches {bad}")
