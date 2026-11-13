#!/usr/bin/env python3
# Adversarial synthetic T-SQL inputs for scaling checks. gen_adv.py OUTDIR
import os,sys
D=sys.argv[1]; os.makedirs(D,exist_ok=True)
def w(name,txt): open(os.path.join(D,name),'w').write(txt)
schema="CREATE TABLE dbo.T (Id int NOT NULL PRIMARY KEY, Name nvarchar(50) NULL, Qty int NOT NULL, Amount decimal(18,2) NOT NULL, Created datetime2(7) NOT NULL);\nGO\nCREATE TABLE dbo.Log (Id int IDENTITY(1,1) PRIMARY KEY, Msg nvarchar(200) NULL, At datetime2(7) NOT NULL);\nGO\n"
# A. many procedures per file, each N statements (Davy: largest files CREATE many procs)
def proc(i,n):
    body="".join(f"    UPDATE dbo.T SET Qty = Qty + {k} WHERE Id = @id + {k};\n    IF @@ROWCOUNT = 0 INSERT dbo.Log (Msg, At) VALUES (N'none {i}-{k}', SYSUTCDATETIME());\n" for k in range(n))
    return f"CREATE PROCEDURE dbo.P{i} @id int\nAS\nBEGIN\n    SET NOCOUNT ON;\n{body}END\nGO\n"
for procs,n in [(50,20),(200,20),(50,200),(500,20)]:
    w(f"a_procs_{procs}x{n}.sql", schema+"".join(proc(i,n) for i in range(procs)))
# B. one procedure with many writes inside a loop (loop with W writes)
for n in [20,100,400]:
    body="".join(f"        UPDATE dbo.T SET Qty = Qty + {k} WHERE Id = @i;\n" for k in range(n))
    w(f"b_loop_writes_{n}.sql", schema+f"CREATE PROCEDURE dbo.PL @n int\nAS\nBEGIN\n    DECLARE @i int = 0;\n    WHILE @i < @n\n    BEGIN\n{body}        SET @i = @i + 1;\n    END\nEND\nGO\n")
# C. nested TRY/CATCH depth d with writes
def nest(d,k=0):
    if d==0: return f"{'    '*(k+1)}INSERT dbo.Log (Msg, At) VALUES (N'leaf', SYSUTCDATETIME());\n"
    ind='    '*(k+1)
    return f"{ind}BEGIN TRY\n{ind}    UPDATE dbo.T SET Qty = Qty + 1 WHERE Id = {d};\n{nest(d-1,k+1)}{ind}END TRY\n{ind}BEGIN CATCH\n{ind}    INSERT dbo.Log (Msg, At) VALUES (ERROR_MESSAGE(), SYSUTCDATETIME());\n{nest(d-1,k+1) if d>3 else ''}{ind}END CATCH\n"
for d in [4,8,12]:
    w(f"c_nested_try_{d}.sql", schema+f"CREATE PROCEDURE dbo.PT\nAS\nBEGIN\n{nest(d)}END\nGO\n")
# D. one long transaction script with many writes (sakila-like, PG BEGIN..COMMIT can't merge)
for n in [500,2000,8000]:
    rows="".join(f"INSERT dbo.T (Id, Name, Qty, Amount, Created) VALUES ({k}, N'n{k}', {k%7}, {k}.25, '20200101');\n" for k in range(n))
    w(f"d_txn_inserts_{n}.sql", schema+"BEGIN TRANSACTION\n"+rows+"COMMIT\nGO\n")
# E. huge VALUES list in one INSERT
for n in [2000,20000]:
    vals=",\n".join(f"({k}, N'n{k}', {k%7}, {k}.25, '20200101')" for k in range(n))
    w(f"e_values_{n}.sql", schema+"INSERT dbo.T (Id, Name, Qty, Amount, Created) VALUES\n"+vals+";\nGO\n")
# F. long IN list and OR chain in a procedure
for n in [500,5000]:
    w(f"f_in_{n}.sql", schema+f"CREATE PROCEDURE dbo.PI\nAS\nBEGIN\n    SELECT Id FROM dbo.T WHERE Id IN ({','.join(str(k) for k in range(n))});\n    DELETE dbo.T WHERE "+" OR ".join(f"Id = {k}" for k in range(n))+";\nEND\nGO\n")
# G. many tables (catalog scaling) + a proc that touches many of them
for n in [1000,5000]:
    tabs="".join(f"CREATE TABLE dbo.T{k} (Id int NOT NULL PRIMARY KEY, V{k} nvarchar(20) NULL, Q int NOT NULL);\nGO\n" for k in range(n))
    uses="".join(f"    UPDATE dbo.T{k} SET Q = Q + 1 WHERE Id = @id;\n" for k in range(0,n,max(1,n//200)))
    w(f"g_tables_{n}.sql", tabs+f"CREATE PROCEDURE dbo.PG @id int\nAS\nBEGIN\n{uses}END\nGO\n")
# H. wide table (1000 columns) and inserts into it
cols=",\n".join(f"  C{k} int NULL" for k in range(1000))
w("h_wide_1000.sql", f"CREATE TABLE dbo.W (Id int NOT NULL PRIMARY KEY,\n{cols});\nGO\nINSERT dbo.W (Id, "+", ".join(f"C{k}" for k in range(1000))+") VALUES (1, "+", ".join("1" for k in range(1000))+");\nGO\nUPDATE dbo.W SET "+", ".join(f"C{k} = C{k} + 1" for k in range(1000))+" WHERE Id = 1;\nGO\n")
# I. cursor loop with writes and dynamic SQL
w("i_cursor_dyn.sql", schema+"""CREATE PROCEDURE dbo.PC
AS
BEGIN
    DECLARE @id int, @sql nvarchar(max);
    DECLARE c CURSOR LOCAL FAST_FORWARD FOR SELECT Id FROM dbo.T;
    OPEN c; FETCH NEXT FROM c INTO @id;
    WHILE @@FETCH_STATUS = 0
    BEGIN
        UPDATE dbo.T SET Qty = Qty + 1 WHERE Id = @id;
        SET @sql = N'UPDATE dbo.T SET Qty = Qty + 1 WHERE Id = ' + CAST(@id AS nvarchar(10));
        EXEC sp_executesql @sql;
        INSERT dbo.Log (Msg, At) VALUES (@sql, SYSUTCDATETIME());
        FETCH NEXT FROM c INTO @id;
    END
    CLOSE c; DEALLOCATE c;
END
GO
""")
# J. deep IF/ELSE branching (2^k paths) with writes
def ifs(k):
    if k==0: return "    UPDATE dbo.T SET Qty = Qty + 1 WHERE Id = 1;\n"
    return f"    IF @x > {k}\n    BEGIN\n{ifs(k-1)}    END\n    ELSE\n    BEGIN\n        UPDATE dbo.T SET Qty = Qty - {k} WHERE Id = {k};\n{ifs(k-1)}    END\n"
for k in [6,10,14,16]:
    w(f"j_branches_{k}.sql", schema+f"CREATE PROCEDURE dbo.PB @x int\nAS\nBEGIN\n{ifs(k)}END\nGO\n")
# K. sequential IFs (not nested): 2^n paths if explored naively
for n in [20,60,200]:
    body="".join(f"    IF @x > {k} UPDATE dbo.T SET Qty = Qty + {k} WHERE Id = {k};\n" for k in range(n))
    w(f"k_seq_ifs_{n}.sql", schema+f"CREATE PROCEDURE dbo.PS @x int\nAS\nBEGIN\n{body}    INSERT dbo.Log (Msg, At) VALUES (N'done', SYSUTCDATETIME());\nEND\nGO\n")
# L. many savepoints / nested transactions
body="".join(f"    SAVE TRANSACTION S{k};\n    UPDATE dbo.T SET Qty = Qty + 1 WHERE Id = {k};\n    IF @@ERROR <> 0 ROLLBACK TRANSACTION S{k};\n" for k in range(100))
w("l_savepoints_100.sql", schema+f"CREATE PROCEDURE dbo.PSV\nAS\nBEGIN\n    BEGIN TRANSACTION;\n{body}    COMMIT;\nEND\nGO\n")
# M. a view chain depth 50 (binding/propagation)
vs="CREATE VIEW dbo.V0 AS SELECT Id, Qty FROM dbo.T;\nGO\n"+"".join(f"CREATE VIEW dbo.V{k} AS SELECT Id, Qty + 1 AS Qty FROM dbo.V{k-1};\nGO\n" for k in range(1,50))
w("m_view_chain_50.sql", schema+vs+"CREATE PROCEDURE dbo.PV AS BEGIN UPDATE dbo.T SET Qty = v.Qty FROM dbo.T t JOIN dbo.V49 v ON v.Id = t.Id; END\nGO\n")
print(sorted(os.listdir(D)))
# N. The audit repros (eval/audit-2026-10-26/repros, regenerated here so that
# nothing large is committed): hostile single-line and deeply nested inputs.
w("n_h01_go_line.sql", "GO x " * 200000 + "\n")
w("n_h02_select_no_from_db2i.sql", "SELECT 1\n" * 20000 + "\n")
w("n_h03_union_all_12k.sql", "INSERT INTO t (a) SELECT 0" + "".join(f" UNION ALL SELECT {k}" for k in range(1, 12000)) + ";\n")
w("n_h04_nested_while.sql", "WHILE 1=1 " * 4000 + "\n")
w("n_s01_deep_parens.sql", "SELECT " + "(" * 3000 + "1" + ")" * 3000 + ";\n")
w("n_s02_long_or_chain.sql", "SELECT * FROM t WHERE a=0" + "".join(f" OR a={k}" for k in range(1, 10000)) + ";\n")
w("n_s03_nested_begin.sql", "BEGIN " * 8000 + "SELECT 1;" + " END" * 8000 + "\n")
for name, text in [("p01_unterminated_quote", "SELECT '"), ("p02_routine_no_body", "CREATE PROCEDURE p"),
                   ("p03_insert_alone", "INSERT"), ("p04_set_alone", "SET"), ("p05_set_no_value", "SET @a ="),
                   ("p06_declare_alone", "DECLARE"), ("p07_create_alone", "CREATE"), ("p08_print_alone", "PRINT")]:
    w(f"n_{name}.sql", text)
