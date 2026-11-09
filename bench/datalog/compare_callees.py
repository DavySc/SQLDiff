import sys,os,collections
d,o=sys.argv[1],sys.argv[2]
st={}
for l in open(os.path.join(o,"st.csv")):
    i,u,s=l.split(); st[(i,u)]=s
notes=collections.defaultdict(set)
for l in open(os.path.join(o,"note.csv")):
    i,u,r=l.rstrip("\n").split("\t"); notes[(i,u)].add(r)
n=bad=nb=0
for l in open(os.path.join(d,"rust_unit.tsv")):
    i,u,s,rules=(l.rstrip("\n").split("\t")+[""])[:4]; n+=1
    if st.get((i,u))!=s:
        bad+=1
        if bad<=5: print("STATUS MISMATCH",i,u,"rust",s,"dl",st.get((i,u)))
    if set(filter(None,rules.split(",")))!=notes[(i,u)]: nb+=1
cm=open(os.path.join(o,"cyc_mismatch.csv")).read().split()
print(f"units {n}, status mismatches {bad}, assumption-note-set differences {nb}, cycle mismatches {len(cm)}")
