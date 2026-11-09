# gen_tree.py N OUT: N scalar functions in a random DAG (each calls up to 3 later ones), 1 in 25 leaves unproven (NEWID), plus a caller procedure per 5 functions.
import sys,random,os
n,out=int(sys.argv[1]),sys.argv[2]; random.seed(7); os.makedirs(out,exist_ok=True)
open(f"{out}/0000_tables.sql","w").write("CREATE TABLE App.Codes (Id int NOT NULL PRIMARY KEY, Code nvarchar(5) NOT NULL);\nGO\nCREATE TABLE App.Items (Id int NOT NULL PRIMARY KEY, CodeId int NULL);\nGO\n")
for i in range(n):
    later=[j for j in random.sample(range(i+1,n),min(3,n-i-1))] if i<n-1 and random.random()<0.8 else []
    expr=" + ".join([f"App.F{j}(@Code)" for j in later]) or "@Id"
    bad=" AND CAST(NEWID() AS varchar(36)) <> ''" if not later and random.random()<0.25 else ""
    name=f"{random.randrange(10**6):06d}_f{i}.sql"   # file order unrelated to call order
    open(f"{out}/{name}","w").write(f"CREATE FUNCTION App.F{i}(@Code nvarchar(5))\nRETURNS int\nAS\nBEGIN\n    DECLARE @Id int;\n    SELECT @Id = Id FROM App.Codes WHERE Code = @Code{bad};\n    RETURN {expr};\nEND;\n")
    if i%5==0:
        open(f"{out}/{random.randrange(10**6):06d}_p{i}.sql","w").write(f"CREATE PROCEDURE App.P{i} @Id int, @Code nvarchar(5)\nAS\nBEGIN\n    SET NOCOUNT ON;\n    UPDATE App.Items SET CodeId = App.F{i}(@Code) WHERE Id = @Id;\nEND;\n")
