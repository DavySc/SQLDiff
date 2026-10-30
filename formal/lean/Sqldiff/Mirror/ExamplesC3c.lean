/-
Checked examples for C3c, wider plain tables: string columns written and
read under the C2b bounds, constant defaults on `INSERT`, string
comparisons on columns under the collation fact, and what stays outside
(a value that may not fit, a `wide` value into a `varchar` column, a read
into a variable it does not fit, a string key, a default of the wrong
kind), with runs of the core interpreter on table states. Every `#guard`
runs on every `lake build`.
-/
import Sqldiff.Mirror.ExamplesC3b
import Sqldiff.Mirror.ExamplesC2b

namespace Sqldiff.Mirror.ExamplesC3c

open Sqldiff.Core (Obs Db SExpr)
open Sqldiff.Mirror.Examples
open Sqldiff.Mirror.ExamplesC2b (vc nvc text S str)
open Sqldiff.Mirror.ExamplesC3a (tc upd ins del nocount runD)
open Sqldiff.Mirror.ExamplesC3b (sel asg als item selectT forBlock field)
open Sqldiff.Gate (Answer)

/-! ## The table

```sql
-- T-SQL                                       -- PostgreSQL (the converted DDL)
CREATE TABLE dbo.Cust (                        CREATE TABLE Cust (
  Id int NOT NULL PRIMARY KEY,                   Id integer NOT NULL PRIMARY KEY,
  Name varchar(20) NOT NULL,                     Name varchar(20) NOT NULL,
  City nvarchar(30) NULL DEFAULT 'Paris',        City varchar(30) NULL DEFAULT 'Paris',
  Qty int NOT NULL DEFAULT 0);                   Qty integer NOT NULL DEFAULT 0);
CREATE INDEX IX_Cust_Name ON dbo.Cust (Name);  CREATE INDEX IX_Cust_Name ON Cust (Name);
```

The index is the Rust side's business (it sends the table because the
index is non-unique and its key is at most 20 bytes, 88 in PostgreSQL's
count); the catalog below is what the checker is told. -/

def cust : CatTable :=
  { name := "cust"
    cols := [⟨"id", "Id", true, true, .int .i32, none⟩,
      ⟨"name", "Name", true, false, .str 20 false, none⟩,
      ⟨"city", "City", false, false, .str 30 true, some (.str "Paris")⟩,
      ⟨"qty", "Qty", true, false, .int .i32, some (.int 0)⟩] }

def depc : Deployment := { rowCounts := false, routines := [], tables := [("dbo.Cust", "cust")] }
def Γc : Catalog := { routines := [], tables := [cust] }
/-- With the collation fact (C2b): string `=` is in the slice. -/
def Γcb : Catalog := { Γc with binaryCollation := true }

def custDef : Core.TableDef :=
  ⟨"cust", [⟨"id", true, true⟩, ⟨"name", true, false⟩, ⟨"city", false, false⟩, ⟨"qty", true, false⟩]⟩

#guard cust.wellFormed

def keyT : Ast.Expr := .Bin "=" (tc "Id") (tv "@id")
def keyP : Ast.Expr := .Bin "=" (pv "id") (pv "p_id")
def cCert : Cert := ⟨[("@id", "p_id"), ("@c", "p_c"), ("@n", "v_n"), (statusVar, statusVar)]⟩

def procT (body : List Ast.Node) (ps : List Ast.Param := [inT "@id", ioT "@c" (nvc 30)]) :
    Ast.Routine :=
  tsqlProc ["dbo", "addc"] ps ([xactAbort, nocount] ++ body)
def procP (body : List Ast.Node) (decls : List Ast.Stmt := [])
    (ps : List Ast.Param := [inP "p_id", ioP "p_c" (vc 30)]) : Ast.Routine :=
  pgProc ["addc"] ps decls body

def checkC (a b : Ast.Routine) (g : Catalog := Γc) (cert : Cert := cCert) : Answer :=
  check depc g cert a b

/-! ## The pair

```sql
CREATE PROCEDURE dbo.addc @id int, @c nvarchar(30) OUTPUT AS
BEGIN
  SET XACT_ABORT ON;
  SET NOCOUNT ON;
  DECLARE @n varchar(20) = 'abc';
  INSERT INTO dbo.Cust (Id, Name) VALUES (@id, @n);
  UPDATE dbo.Cust SET Name = RTRIM(Name) + 'x', City = 'Lyon' WHERE Id = @id;
  SELECT @c = City FROM dbo.Cust WHERE Id = @id;
END
```

```sql
CREATE OR REPLACE PROCEDURE addc(p_id integer, INOUT p_c varchar(30))
LANGUAGE plpgsql AS $$
DECLARE v_n varchar(20);
BEGIN
  v_n := 'abc';
  INSERT INTO Cust (Id, Name) VALUES (p_id, v_n);
  UPDATE Cust SET Name = rtrim(Name) || 'x', City = 'Lyon' WHERE Id = p_id;
  DECLARE sqldiff_r record;
  BEGIN
  FOR sqldiff_r IN SELECT City AS sqldiff_c1 FROM Cust WHERE Id = p_id LOOP
    p_c := sqldiff_r.sqldiff_c1;
  END LOOP;
  END;
END; $$;
```

`RTRIM(Name) + 'x'` has at most 21 characters, too many for `Name`: the
pair uses `RTRIM(Name)` alone (refused with the `+ 'x'` below). -/

def addBodyT (setName : Ast.Expr := fn "RTRIM" [tc "Name"]) : List Ast.Node := [
  .NStmt (.Declare "@n" (some (vc 20)) (some (str "abc"))),
  ins ["dbo", "Cust"] ["Id", "Name"] [[tv "@id", tv "@n"]],
  upd ["dbo", "Cust"] [("Name", setName), ("City", str "Lyon")] (some keyT),
  selectT [asg "@c" (tc "City")] ["dbo", "Cust"] (some keyT)]

def addBodyP (setName : Ast.Expr := fn "rtrim" [pv "Name"]) : List Ast.Node := [
  set (pv "v_n") (str "abc"),
  ins ["Cust"] ["Id", "Name"] [[pv "p_id", pv "v_n"]],
  upd ["Cust"] [("Name", setName), ("City", str "Lyon")] (some keyP),
  forBlock [als (pv "City") "sqldiff_c1"] ["Cust"] (some keyP) [field "p_c" "sqldiff_r" "sqldiff_c1"]]

def addDeclsP : List Ast.Stmt := [.Declare "v_n" (some (vc 20)) none]

def addT : Ast.Routine := procT addBodyT
def addP : Ast.Routine := procP addBodyP addDeclsP

/-! ## Confirmed -/

#guard isC2a (checkC addT addP)
-- `Id = @id` compares integers with `=`; an `INSERT` into a table with a
-- `NOT NULL` column can fail (`XACT_ABORT ON`), after a write; the T-SQL
-- parameter `@c` is a string. No string comparison: no collation fact.
#guard residualOf depc Γc addT ==
  some ["ASM-TSQL-ANSI-NULLS", "ASM-STATE-AFTER-ERROR", "ASM-TSQL-STRING-ARGS", "ASM-NO-CALLER-TXN", "ASM-TSQL-ROWCOUNT-0"]
#guard (lowerT depc Γc addT).map Core.Routine.hasQuery == some true

/- The lowered `INSERT`: the left-out columns with a default, `City` and
`Qty`, are added with their literals, on both sides. -/
#guard (lowerT depc Γc (procT [ins ["dbo", "Cust"] ["Id", "Name"] [[tv "@id", str "a"]]])).map
    (·.body) ==
  some (.seq (.assign statusVar (.lit 0))
    (.insert custDef ["id", "name", "city", "qty"]
      [[.sc (.var "@id"), .sc (.str "a"), .sc (.str "Paris"), .sc (.int 0)]]))
#guard (lowerP Γc (procP [ins ["Cust"] ["Id", "Name"] [[pv "p_id", str "a"]]])).map (·.body) ==
  some (.seq (.assign statusVar (.lit 0))
    (.insert custDef ["id", "name", "city", "qty"]
      [[.sc (.var "p_id"), .sc (.str "a"), .sc (.str "Paris"), .sc (.int 0)]]))
-- A listed column keeps its value (also NULL): nothing is added for it.
#guard (lowerT depc Γc (procT [ins ["dbo", "Cust"] ["Id", "Name", "City", "Qty"]
    [[tv "@id", str "a", .Null, num 5]]])).map (·.body) ==
  some (.seq (.assign statusVar (.lit 0))
    (.insert custDef ["id", "name", "city", "qty"]
      [[.sc (.var "@id"), .sc (.str "a"), .sc .null, .sc (.int 5)]]))

/-! ## The core as an oracle

Runs of the lowered `addc` from an empty table and from a table with
row 1. The outputs are `@c` and the status. -/

def aT := lowerT depc Γc addT
def aP := lowerP Γc addP

/- `addc(1)` on an empty table: the row gets `Name 'abc'` and the
defaults, then `City` becomes `'Lyon'`, which `@c` reads. -/
#guard runD aT [I 1, N] [("cust", [])] ==
  some (.done [S "Lyon", I 0] none 0 [("cust", [[I 1, S "abc", S "Lyon", I 0]])] [])
#guard runD aP [I 1, N] [("cust", [])] == runD aT [I 1, N] [("cust", [])]
/- `addc(1)` when row 1 exists: a duplicate key, a `constraint` error,
nothing is written (statement atomicity). -/
#guard runD aT [I 1, N] [("cust", [[I 1, S "x", N, I 3]])] == some (.failed .constraint [])
#guard runD aP [I 1, N] [("cust", [[I 1, S "x", N, I 3]])] ==
  runD aT [I 1, N] [("cust", [[I 1, S "x", N, I 3]])]
/- `INSERT` of a NULL `Name` (`NOT NULL`, no default): 515 against
23502, a `constraint` error; leaving `Qty` out is no error (its default). -/
#guard runD (lowerT depc Γc (procT [ins ["dbo", "Cust"] ["Id", "Name"] [[tv "@id", .Null]]]))
  [I 1, N] [("cust", [])] == some (.failed .constraint [])
/- Trailing spaces are kept on the write and `RTRIM` takes them off on
the update (eval/tables T1). -/
#guard runD (lowerT depc Γc (procT [ins ["dbo", "Cust"] ["Id", "Name"] [[tv "@id", str "ab  "]],
    upd ["dbo", "Cust"] [("City", fn "RTRIM" [tc "Name"])] (some keyT)]))
  [I 2, N] [("cust", [])] ==
  some (.done [N, I 0] none 0 [("cust", [[I 2, S "ab  ", S "ab", I 0]])] [])

/-! ## Refused: what a string column takes -/

def insName (v : Ast.Expr) (ps : List Ast.Param := [inT "@id", ioT "@c" (nvc 30)]) : Ast.Routine :=
  procT [ins ["dbo", "Cust"] ["Id", "Name"] [[tv "@id", v]]] ps
#guard (lowerT depc Γc (insName (str "abcdefghijklmnopqrst"))).isSome
-- 21 characters into `varchar(20)`: 2628 against 22001 (eval/tables T2).
#guard (lowerT depc Γc (insName (str "abcdefghijklmnopqrstu"))).isNone
-- An `nvarchar` value into a `varchar` column (`wide`).
#guard (lowerT depc Γc (insName (tv "@w") [inT "@id", inT "@w" (nvc 5)])).isNone
-- An `nvarchar` column takes a `varchar` value and an `nvarchar` one.
#guard (lowerT depc Γc (procT [ins ["dbo", "Cust"] ["Id", "Name", "City"]
  [[tv "@id", str "a", tv "@w"]]] [inT "@id", inT "@w" (nvc 30)])).isSome
#guard (lowerT depc Γc (procT [ins ["dbo", "Cust"] ["Id", "Name", "City"]
  [[tv "@id", str "a", tv "@w"]]] [inT "@id", inT "@w" (vc 30)])).isSome
#guard (lowerT depc Γc (procT [ins ["dbo", "Cust"] ["Id", "Name", "City"]
  [[tv "@id", str "a", tv "@w"]]] [inT "@id", inT "@w" (nvc 31)])).isNone
-- An integer into a string column, a string into an integer column.
#guard (lowerT depc Γc (insName (num 5))).isNone
#guard (lowerT depc Γc (procT [ins ["dbo", "Cust"] ["Id", "Name", "Qty"]
  [[tv "@id", str "a", str "5"]]])).isNone
-- A value outside the slice (a function the slice does not have).
#guard (lowerT depc Γc (insName (fn "UPPER" [str "a"]))).isNone
-- `UPDATE`: `RTRIM(Name) + 'x'` may have 21 characters.
#guard (lowerT depc Γc (procT (addBodyT (.Bin "+" (fn "RTRIM" [tc "Name"]) (str "x"))))).isNone
#guard (lowerP Γc (procP (addBodyP (.Bin "||" (fn "rtrim" [pv "Name"]) (str "x"))) addDeclsP)).isNone
-- PostgreSQL: a parameter has no length (C2b), so its value may not fit a
-- `varchar(20)` column; a local `varchar(20)` does.
#guard (lowerP Γc (procP [ins ["Cust"] ["Id", "Name"] [[pv "p_id", pv "p_n"]]] []
  [inP "p_id", inP "p_n" (vc 20)])).isNone
#guard (lowerP Γc (procP [ins ["Cust"] ["Id", "Name"] [[pv "p_id", pv "v_n"]]] addDeclsP)).isSome
#guard (lowerP Γc (procP [ins ["Cust"] ["Id", "Name"] [[pv "p_id", pv "v_n"]]]
  [.Declare "v_n" (some (vc 21)) none])).isNone

/-! ## Refused: reads into a variable they do not fit

`SELECT @c = City` stores the column's value: SQL Server truncates one
that does not fit, PostgreSQL raises 22001 (eval/tables T9). -/

-- `@c nvarchar(30)` takes `City`; `@c varchar(30)` does not (`wide`), nor
-- `@c nvarchar(29)`.
#guard (lowerT depc Γc (procT [selectT [asg "@c" (tc "City")] ["dbo", "Cust"] (some keyT)])).isSome
#guard (lowerT depc Γc (procT [selectT [asg "@c" (tc "City")] ["dbo", "Cust"] (some keyT)]
  [inT "@id", ioT "@c" (vc 30)])).isNone
#guard (lowerT depc Γc (procT [selectT [asg "@c" (tc "City")] ["dbo", "Cust"] (some keyT)]
  [inT "@id", ioT "@c" (nvc 29)])).isNone
-- `Name` (`varchar(20)`) into `varchar(30)`.
#guard (lowerT depc Γc (procT [selectT [asg "@c" (tc "Name")] ["dbo", "Cust"] (some keyT)]
  [inT "@id", ioT "@c" (vc 30)])).isSome
-- A string into an `int` variable, an integer into a string variable.
#guard (lowerT depc Γc (procT [selectT [asg "@c" (tc "Name")] ["dbo", "Cust"] (some keyT)]
  [inT "@id", ioT "@c"])).isNone
#guard (lowerT depc Γc (procT [selectT [asg "@c" (tc "Qty")] ["dbo", "Cust"] (some keyT)])).isNone
-- The scalar form: `SET @c = (SELECT City ...)`, the same rules.
#guard (lowerT depc Γc (procT [set (tv "@c") (.Subquery (sel [item (tc "City")] ["dbo", "Cust"]
  (some keyT)))])).isSome
#guard (lowerT depc Γc (procT [set (tv "@c") (.Subquery (sel [item (tc "City")] ["dbo", "Cust"]
  (some keyT)))] [inT "@id", ioT "@c" (nvc 29)])).isNone
-- PL/pgSQL: a local `varchar(10)` does not take `City` (`varchar(30)`),
-- in the `FOR` block or the scalar form; a parameter (no length) does.
#guard (lowerP Γc (procP [forBlock [als (pv "City") "c1"] ["Cust"] (some keyP)
  [field "v_n" "sqldiff_r" "c1"]] [.Declare "v_n" (some (vc 10)) none])).isNone
#guard (lowerP Γc (procP [forBlock [als (pv "City") "c1"] ["Cust"] (some keyP)
  [field "v_n" "sqldiff_r" "c1"]] [.Declare "v_n" (some (vc 30)) none])).isSome
#guard (lowerP Γc (procP [set (pv "v_n") (.Subquery (sel [item (pv "City")] ["Cust"] (some keyP)))]
  [.Declare "v_n" (some (vc 10)) none])).isNone
#guard (lowerP Γc (procP [set (pv "p_c") (.Subquery (sel [item (pv "City")] ["Cust"] (some keyP)))]
  [])).isSome

/-! ## String comparisons on columns

T-SQL pads trailing spaces in `=` under every collation, PostgreSQL does
not (eval/tables T5): with the collation fact, T-SQL `Name = @n` is
`rtrim(Name) = rtrim(@n)`, which the converter must write as such; a
plain `=` on the PostgreSQL side is refused, and without the fact both
stay uninterpreted operators (the C1 theorem). -/

def delT : Ast.Routine :=
  procT [del ["dbo", "Cust"] (some (.Bin "=" (tc "Name") (tv "@n")))] [inT "@id", inT "@n" (vc 20)]
def delP (wh : Ast.Expr) : Ast.Routine :=
  procP [del ["Cust"] (some wh)] [] [inP "p_id", inP "v_n" (vc 20)]
def plainEq : Ast.Expr := .Bin "=" (pv "Name") (pv "v_n")
def trimEq : Ast.Expr := .Bin "=" (fn "rtrim" [pv "Name"]) (fn "rtrim" [pv "v_n"])
#guard isC2a (checkC delT (delP trimEq) Γcb)
#guard residualOf depc Γcb delT ==
  some ["ASM-TSQL-ANSI-NULLS", "ASM-BINARY-COLLATION", "ASM-TSQL-STRING-ARGS", "ASM-TSQL-ROWCOUNT-0"]
#guard reasonOf (checkC delT (delP plainEq) Γcb) == "the lowered routines differ beyond the renaming"
#guard isC1 (checkC delT (delP plainEq))
-- Why: `'ab '` is deleted by T-SQL `Name = 'ab'`, not by PostgreSQL's `=`.
#guard runD (lowerT depc Γcb delT) [I 0, S "ab"] [("cust", [[I 1, S "ab ", N, I 0]])] ==
  some (.done [I 0] none 0 [("cust", [])] [])
#guard runD (lowerP Γcb (delP plainEq)) [I 0, S "ab"] [("cust", [[I 1, S "ab ", N, I 0]])] ==
  some (.done [I 0] none 0 [("cust", [[I 1, S "ab ", N, I 0]])] [])

/-! ## Refused: tables the slice does not take -/

/-- `cust` with one column changed. -/
def custWith (i : Nat) (c : CatCol) : Catalog :=
  { Γc with tables := [{ cust with cols := cust.cols.set i c }] }
def anyIns : Ast.Routine := procT [ins ["dbo", "Cust"] ["Id", "Name"] [[tv "@id", str "a"]]]
#guard (lowerT depc Γc anyIns).isSome
-- A string key (eval/tables T6: `'a'` and `'a '` are one key in SQL Server).
#guard !(custWith 1 ⟨"name", "Name", true, true, .str 20 false, none⟩).tables.all (·.wellFormed)
#guard (lowerT depc (custWith 1 ⟨"name", "Name", true, true, .str 20 false, none⟩) anyIns).isNone
-- A length of 0 or over 4000.
#guard (lowerT depc (custWith 1 ⟨"name", "Name", true, false, .str 0 false, none⟩) anyIns).isNone
#guard (lowerT depc (custWith 1 ⟨"name", "Name", true, false, .str 4001 false, none⟩) anyIns).isNone
-- Defaults: too long for the column (eval/tables T4: 2628 against 22001),
-- a string for an integer column, an integer for a string column, one
-- outside the 32-bit range, a non-ASCII or backslash string, an expression.
#guard (lowerT depc (custWith 2 ⟨"city", "City", false, false, .str 3 true, some (.str "Paris")⟩)
  anyIns).isNone
#guard (lowerT depc (custWith 3 ⟨"qty", "Qty", true, false, .int .i32, some (.str "0")⟩) anyIns).isNone
#guard (lowerT depc (custWith 2 ⟨"city", "City", false, false, .str 30 true, some (.int 0)⟩)
  anyIns).isNone
#guard (lowerT depc (custWith 3 ⟨"qty", "Qty", true, false, .int .i32, some (.int 2147483648)⟩)
  anyIns).isNone
#guard (lowerT depc (custWith 2 ⟨"city", "City", false, false, .str 30 true, some (.str "é")⟩)
  anyIns).isNone
#guard (lowerT depc (custWith 2 ⟨"city", "City", false, false, .str 30 true, some (.str "a\\b")⟩)
  anyIns).isNone
#guard (lowerT depc (custWith 3 ⟨"qty", "Qty", true, false, .int .i32,
  some (.arith .i32 .add (.int 1) (.int 1))⟩) anyIns).isNone
-- A default on a key column is a value like any other.
#guard (lowerT depc (custWith 0 ⟨"id", "Id", true, true, .int .i32, some (.int 1)⟩)
  (procT [ins ["dbo", "Cust"] ["Name"] [[str "a"]]])).isSome

/-! ## A default on the key: the second `INSERT` is a duplicate -/

def Γk : Catalog := custWith 0 ⟨"id", "Id", true, true, .int .i32, some (.int 1)⟩
def twiceT : Ast.Routine :=
  procT [ins ["dbo", "Cust"] ["Name"] [[str "a"]], ins ["dbo", "Cust"] ["Name"] [[str "b"]]]
def twiceP : Ast.Routine :=
  procP [ins ["Cust"] ["Name"] [[str "a"]], ins ["Cust"] ["Name"] [[str "b"]]]
#guard isC2a (check depc Γk ⟨[("@id", "p_id"), ("@c", "p_c"), (statusVar, statusVar)]⟩ twiceT twiceP)
#guard runD (lowerT depc Γk twiceT) [I 0, N] [("cust", [])] == some (.failed .constraint [])

end Sqldiff.Mirror.ExamplesC3c
