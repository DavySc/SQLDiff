# Port of perf-plan/measurements/2026-11-07/loop-chain-bench.rs body() (non-loop shapes).
import sys
L=["SET @v1 = @p + {};","SET @v2 = @v1 * {};","SET @v3 = @v2 - {};",
"INSERT INTO #t (a) VALUES (@v1 + {});","UPDATE #t SET a = a + {} WHERE a > @v2;",
"SELECT a FROM #t WHERE a = @v3 + {};","SELECT @v1 + {};","SET @w{i} = @v1 + {};","SET @v1 = @w{j} + {};"]
M=(1<<64)-1
def body(n,every,b,nvars,chain=False):
    s=[];x=12345
    if chain: nvars=n+1
    decl="DECLARE @v1 int = 0, @v2 int = 0, @v3 int = 0"+"".join(f";\nDECLARE @w{i} int = 0" for i in range(nvars))+";"
    for k in range(n):
        if chain:
            c=7 if b and k+every>=n else 3
            s.append(f"SET @w{k} = @w{k+1} + {c};"); continue
        x=(x*6364136223846793005+1442695040888963407)&M
        t=L[(x>>33)%len(L)]; i=(x>>20)%nvars; j=(x>>40)%nvars
        c=7 if b and k%every==0 else 3
        s.append(t.replace("{i}",str(i)).replace("{j}",str(j)).replace("{}",str(c)))
    return f"CREATE PROCEDURE dbo.r @p int AS\nBEGIN\n{decl}\nCREATE TABLE #t (a int);\n"+"\n".join(s)+"\nSELECT a FROM #t;\nEND\n"
n,every,nv,out=int(sys.argv[1]),int(sys.argv[2]),int(sys.argv[3]),sys.argv[4]  # [chain]
chain=len(sys.argv)>5
open(out+".a.sql","w").write(body(n,every,False,nv,chain)); open(out+".b.sql","w").write(body(n,every,True,nv,chain))
