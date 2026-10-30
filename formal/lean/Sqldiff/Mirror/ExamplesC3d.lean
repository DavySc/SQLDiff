/-
Checked examples for C3d part 1, query blocks: several catalog tables,
inner and cross joins, aliases, qualified names, in the C3b statement
shapes. The pairs the converter writes are confirmed; each boundary of
the rules is refused (with its reason code where one helps); runs of the
core interpreter show the block's rows, the pick through a join, and the
scalar subquery's `cardinality` error on both sides. Every `#guard` runs
on every `lake build`.
-/
import Sqldiff.Mirror.ExamplesC3b

namespace Sqldiff.Mirror.ExamplesC3d

open Sqldiff.Core (Obs Db)
open Sqldiff.Mirror.Examples
open Sqldiff.Mirror.ExamplesC3a (pairT tc runD nocount)
open Sqldiff.Mirror.ExamplesC3b (twoK asg als item countStarE one field)
open Sqldiff.Gate (Answer)

/-! ## The tables

```sql
-- T-SQL                                     -- PostgreSQL (the converted DDL)
CREATE TABLE dbo.Ord (Id int NOT NULL PRIMARY KEY,  CREATE TABLE ord (id integer NOT NULL PRIMARY KEY,
  Qty int NULL, CustId int NULL);                     qty integer NULL, custid integer NULL);
CREATE TABLE dbo.Cust (Id int NOT NULL PRIMARY KEY, CREATE TABLE cust (id integer NOT NULL PRIMARY KEY,
  Score int NULL, Name varchar(10) NULL);             score integer NULL, name varchar(10) NULL);
CREATE TABLE dbo.Ev (D date NOT NULL PRIMARY KEY,   CREATE TABLE ev (d date NOT NULL PRIMARY KEY,
  N int NULL);                                        n integer NULL);
```

and C3b's `two` (key `A`, `B`) and C3a's keyless `pair`. -/

def ordT : CatTable :=
  { name := "ord", cols := [⟨"id", "Id", true, true, .int .i32, none⟩,
      ⟨"qty", "Qty", false, false, .int .i32, none⟩, ⟨"custid", "CustId", false, false, .int .i32, none⟩] }
def custT : CatTable :=
  { name := "cust", cols := [⟨"id", "Id", true, true, .int .i32, none⟩,
      ⟨"score", "Score", false, false, .int .i32, none⟩, ⟨"name", "Name", false, false, .str 10 false, none⟩] }
def evT : CatTable :=
  { name := "ev", cols := [⟨"d", "D", true, true, .date, none⟩, ⟨"n", "N", false, false, .int .i32, none⟩] }
/-- A table keyed by a `datetime2(6)`/`timestamp(6)` column. -/
def evtT : CatTable :=
  { name := "evt", cols := [⟨"t", "T", true, true, .ts 6, none⟩, ⟨"n", "N", false, false, .int .i32, none⟩] }

def Γj : Catalog := { routines := [], tables := [ordT, custT, twoK, evT, pairT, evtT] }
def depj : Deployment :=
  { rowCounts := false, routines := [],
    tables := [("dbo.Ord", "ord"), ("dbo.Cust", "cust"), ("dbo.Two", "two"), ("dbo.Ev", "ev"),
      ("dbo.Pair", "pair"), ("dbo.Evt", "evt")] }

/-! ## Building blocks -/

def tbl (t : List String) (a : Option String := none) : Ast.From := .Table t a []
def jn (l r : Ast.From) (on : Ast.Expr) : Ast.From := .Join "INNER" l r (some on)
def cj (l r : Ast.From) : Ast.From := .Join "CROSS" l r none
/-- A qualified name `a.c`. -/
def qn (a c : String) : Ast.Expr := .Name [a, c]
def eq (a b : Ast.Expr) : Ast.Expr := .Bin "=" a b
def and_ (a b : Ast.Expr) : Ast.Expr := .Bin "AND" a b

/-- `SELECT items FROM froms [WHERE wh] [INTO into]`. -/
def selF (items : List Ast.SelItem) (froms : List Ast.From) (wh : Option Ast.Expr)
    (into : List Ast.Expr := []) : Ast.Select :=
  .Select [] false [] false none items into false froms wh [] none [] none none [] none

def selectF (items : List Ast.SelItem) (froms : List Ast.From) (wh : Option Ast.Expr) : Ast.Node :=
  .NStmt (.SSelect (selF items froms wh))

/-- The converter's `FOR` block over a query block. -/
def forF (items : List Ast.SelItem) (froms : List Ast.From) (wh : Option Ast.Expr)
    (body : List Ast.Node) (r : String := "sqldiff_r") : Ast.Node :=
  .NBlock (.Block [.Declare r (some (.Ty "RECORD" [])) none] [.ForQuery r (selF items froms wh) body])

def lookCert : Cert :=
  ⟨[("@id", "p_id"), ("@q", "p_q"), ("@s", "p_s"), ("@n", "p_n"), (statusVar, statusVar)]⟩

def checkJ (a b : Ast.Routine) (cert : Cert := lookCert) : Answer := check depj Γj cert a b

/-! ## The pair

The converter's input and output (`sqldiff convert --to postgres`, the
comments left out):

```sql
CREATE PROCEDURE dbo.look @id int, @q int OUTPUT, @s int OUTPUT, @n int OUTPUT AS
BEGIN
  SET XACT_ABORT ON;
  SET NOCOUNT ON;
  SELECT @q = o.Qty, @s = c.Score FROM dbo.Ord AS o JOIN dbo.Cust c ON c.Id = o.CustId
    WHERE o.Id = @id;
  SET @s = (SELECT c.Score FROM dbo.Ord o INNER JOIN dbo.Cust AS c ON o.CustId = c.Id
    WHERE o.Qty > @id);
  SELECT @n = COUNT(*) FROM dbo.Ord, dbo.Cust WHERE Ord.CustId = Cust.Id;
  IF EXISTS (SELECT 1 FROM dbo.Ord o CROSS JOIN dbo.Cust c WHERE o.Qty = c.Score)
    SET @n = 0;
END
```

```sql
CREATE OR REPLACE PROCEDURE look(p_id integer, INOUT p_q integer, INOUT p_s integer,
    INOUT p_n integer) LANGUAGE plpgsql AS $$
BEGIN
  DECLARE
  sqldiff_r record;
  BEGIN
  FOR sqldiff_r IN SELECT o.Qty AS sqldiff_c1, c.Score AS sqldiff_c2 FROM Ord AS o
      JOIN Cust c ON c.Id = o.CustId WHERE o.Id = p_id LOOP
    p_q := sqldiff_r.sqldiff_c1;
    p_s := sqldiff_r.sqldiff_c2;
  END LOOP;
  END;
  p_s := (SELECT c.Score FROM Ord o INNER JOIN Cust AS c ON o.CustId = c.Id WHERE o.Qty > p_id);
  SELECT COUNT(*) INTO p_n FROM Ord, Cust WHERE Ord.CustId = Cust.Id;
  IF EXISTS (SELECT 1 FROM Ord o CROSS JOIN Cust c WHERE o.Qty = c.Score) THEN
    p_n := 0;
  END IF;
END; $$;
```
-/

def lookBodyT : List Ast.Node := [
  selectF [asg "@q" (qn "o" "Qty"), asg "@s" (qn "c" "Score")]
    [jn (tbl ["dbo", "Ord"] (some "o")) (tbl ["dbo", "Cust"] (some "c")) (eq (qn "c" "Id") (qn "o" "CustId"))]
    (some (eq (qn "o" "Id") (tv "@id"))),
  set (tv "@s") (.Subquery (selF [item (qn "c" "Score")]
    [jn (tbl ["dbo", "Ord"] (some "o")) (tbl ["dbo", "Cust"] (some "c")) (eq (qn "o" "CustId") (qn "c" "Id"))]
    (some (.Bin ">" (qn "o" "Qty") (tv "@id"))))),
  selectF [asg "@n" countStarE] [tbl ["dbo", "Ord"], tbl ["dbo", "Cust"]]
    (some (eq (qn "Ord" "CustId") (qn "Cust" "Id"))),
  .If (.Exists (selF [one] [cj (tbl ["dbo", "Ord"] (some "o")) (tbl ["dbo", "Cust"] (some "c"))]
    (some (eq (qn "o" "Qty") (qn "c" "Score"))))) [set (tv "@n") (num 0)] []]

def lookT (body : List Ast.Node := lookBodyT) (lead : List Ast.Node := [xactAbort, nocount]) :
    Ast.Routine :=
  tsqlProc ["dbo", "look"] [inT "@id", ioT "@q", ioT "@s", ioT "@n"] (lead ++ body)

def lookBodyP : List Ast.Node := [
  forF [als (qn "o" "Qty") "sqldiff_c1", als (qn "c" "Score") "sqldiff_c2"]
    [jn (tbl ["Ord"] (some "o")) (tbl ["Cust"] (some "c")) (eq (qn "c" "Id") (qn "o" "CustId"))]
    (some (eq (qn "o" "Id") (pv "p_id")))
    [field "p_q" "sqldiff_r" "sqldiff_c1", field "p_s" "sqldiff_r" "sqldiff_c2"],
  set (pv "p_s") (.Subquery (selF [item (qn "c" "Score")]
    [jn (tbl ["Ord"] (some "o")) (tbl ["Cust"] (some "c")) (eq (qn "o" "CustId") (qn "c" "Id"))]
    (some (.Bin ">" (qn "o" "Qty") (pv "p_id"))))),
  .NStmt (.SSelect (selF [item countStarE] [tbl ["Ord"], tbl ["Cust"]]
    (some (eq (qn "Ord" "CustId") (qn "Cust" "Id"))) [pv "p_n"])),
  .If (.Exists (selF [one] [cj (tbl ["Ord"] (some "o")) (tbl ["Cust"] (some "c"))]
    (some (eq (qn "o" "Qty") (qn "c" "Score"))))) [set (pv "p_n") (num 0)] []]

def lookP (body : List Ast.Node := lookBodyP)
    (ps : List Ast.Param := [inP "p_id", ioP "p_q", ioP "p_s", ioP "p_n"]) : Ast.Routine :=
  pgProc ["look"] ps [] body

/-! ## Confirmed -/

#guard isC2a (checkJ lookT lookP)
#guard residualOf depj Γj lookT == some ["ASM-TSQL-ARITH-ERRORS", "ASM-TSQL-ANSI-NULLS", "ASM-TSQL-STRING-ARGS", "ASM-NO-CALLER-TXN"]

/- The lowered `SELECT @q = o.Qty, @s = c.Score FROM dbo.Ord AS o JOIN
dbo.Cust c ON c.Id = o.CustId WHERE o.Id = @id`: two sources, columns
by position (`0.qty`, `1.score`), the `ON` then the `WHERE`. -/
def ordDef : Core.TableDef := ordT.toDef
def custDef : Core.TableDef := custT.toDef
def pickBlock : Core.Block :=
  ⟨[ordDef, custDef], [.sc (.cmp .eq (.col "1.id") (.col "0.custid")), .sc (.cmp .eq (.col "0.id") (.var "@id"))]⟩
#guard (lowerT depj Γj (tsqlProc ["r"] [inT "@id", ioT "@q", ioT "@s"] (lookBodyT.take 1))).map
    (·.body) ==
  some (.seq (.assign statusVar (.lit 0))
    (.query ⟨pickBlock⟩ (.row false [("@q", .sc (.col "0.qty")), ("@s", .sc (.col "1.score"))])))
-- The key closure: `ord` by `0.id = @id`, then `cust` by `1.id = 0.custid`.
#guard pickBlock.atMostOne
#guard pickBlock.closure == [0, 1]
-- Without the `WHERE`, `ord` is not determined, and so neither is `cust`.
#guard !(Core.Block.atMostOne ⟨[ordDef, custDef], [.sc (.cmp .eq (.col "1.id") (.col "0.custid"))]⟩)

/- A self-join through two aliases (eval/queries Q11, "pick through a
self-join"): `y` is determined from `x`'s column. -/
def selfT : Ast.Routine :=
  tsqlProc ["r"] [inT "@id", ioT "@q"] [selectF [asg "@q" (qn "y" "Qty")]
    [jn (tbl ["dbo", "Ord"] (some "x")) (tbl ["dbo", "Ord"] (some "y")) (eq (qn "y" "Id") (qn "x" "CustId"))]
    (some (eq (qn "x" "Id") (tv "@id")))]
def selfP : Ast.Routine :=
  pgProc ["r"] [inP "p_id", ioP "p_q"] [] [forF [als (qn "y" "qty") "sqldiff_c1"]
    [jn (tbl ["ord"] (some "x")) (tbl ["ord"] (some "y")) (eq (qn "y" "id") (qn "x" "custid"))]
    (some (eq (qn "x" "id") (pv "p_id"))) [field "p_q" "sqldiff_r" "sqldiff_c1"]]
def rCert : Cert := ⟨[("@id", "p_id"), ("@q", "p_q"), (statusVar, statusVar)]⟩
#guard isC2a (checkJ selfT selfP rCert)

/- A two-column key fixed from another table's column, in a comma list
(eval/queries Q11): `two.A = o.CustId AND two.B = 1`. -/
def twoKeyT : Ast.Routine :=
  tsqlProc ["r"] [inT "@id", ioT "@q"] [selectF [asg "@q" (qn "Two" "V")]
    [tbl ["dbo", "Ord"] (some "o"), tbl ["dbo", "Two"]]
    (some (and_ (eq (qn "Two" "A") (qn "o" "CustId")) (and_ (eq (qn "Two" "B") (num 1))
      (eq (qn "o" "Id") (tv "@id")))))]
def twoKeyP : Ast.Routine :=
  pgProc ["r"] [inP "p_id", ioP "p_q"] [] [forF [als (qn "Two" "V") "sqldiff_c1"]
    [tbl ["Ord"] (some "o"), tbl ["Two"]]
    (some (and_ (eq (qn "Two" "A") (qn "o" "CustId")) (and_ (eq (qn "Two" "B") (num 1))
      (eq (qn "o" "Id") (pv "p_id"))))) [field "p_q" "sqldiff_r" "sqldiff_c1"]]
#guard isC2a (checkJ twoKeyT twoKeyP rCert)
-- Half of the key: `B` not fixed.
#guard (lowerT depj Γj (tsqlProc ["r"] [inT "@id", ioT "@q"] [selectF [asg "@q" (qn "Two" "V")]
    [tbl ["dbo", "Ord"] (some "o"), tbl ["dbo", "Two"]]
    (some (and_ (eq (qn "Two" "A") (qn "o" "CustId")) (eq (qn "o" "Id") (tv "@id"))))])).isNone

/- Unqualified names, each a column of one table only; an alias that
swaps the tables' names (eval/queries Q12: `FROM a AS b, b AS a`). -/
#guard isC2a (checkJ
  (tsqlProc ["r"] [inT "@id", ioT "@q"] [selectF [asg "@q" (tc "Score")]
    [jn (tbl ["dbo", "Ord"]) (tbl ["dbo", "Cust"]) (eq (tc "CustId") (qn "Cust" "Id"))]
    (some (eq (qn "Ord" "Id") (tv "@id")))])
  (pgProc ["r"] [inP "p_id", ioP "p_q"] [] [forF [als (pv "score") "sqldiff_c1"]
    [jn (tbl ["ord"]) (tbl ["cust"]) (eq (pv "custid") (qn "cust" "id"))]
    (some (eq (qn "ord" "id") (pv "p_id"))) [field "p_q" "sqldiff_r" "sqldiff_c1"]])
  rCert)
#guard isC2a (checkJ
  (tsqlProc ["r"] [inT "@id", ioT "@q"] [selectF [asg "@q" (qn "Ord" "Score")]
    [tbl ["dbo", "Cust"] (some "Ord"), tbl ["dbo", "Ord"] (some "Cust")]
    (some (and_ (eq (qn "Ord" "Id") (qn "Cust" "CustId")) (eq (qn "Cust" "Id") (tv "@id"))))])
  (pgProc ["r"] [inP "p_id", ioP "p_q"] [] [forF [als (qn "ord" "score") "sqldiff_c1"]
    [tbl ["cust"] (some "ord"), tbl ["ord"] (some "cust")]
    (some (and_ (eq (qn "ord" "id") (qn "cust" "custid")) (eq (qn "cust" "id") (pv "p_id"))))
    [field "p_q" "sqldiff_r" "sqldiff_c1"]])
  rCert)

/- A scalar subquery over a block the closure determines cannot raise
`cardinality` on a database the engines can be in: without `XACT_ABORT
ON` the routine is still a C2a confirmation. Over a block it does not
determine, it can: then only `check_sound` (the C1 theorem). -/
def scalarT (wh : Ast.Expr) (lead : List Ast.Node := []) : Ast.Routine :=
  tsqlProc ["r"] [inT "@id", ioT "@q"] (lead ++ [set (tv "@q") (.Subquery (selF [item (qn "c" "Score")]
    [jn (tbl ["dbo", "Ord"] (some "o")) (tbl ["dbo", "Cust"] (some "c")) (eq (qn "c" "Id") (qn "o" "CustId"))]
    (some wh)))])
def scalarP (wh : Ast.Expr) : Ast.Routine :=
  pgProc ["r"] [inP "p_id", ioP "p_q"] [] [set (pv "p_q") (.Subquery (selF [item (qn "c" "Score")]
    [jn (tbl ["Ord"] (some "o")) (tbl ["Cust"] (some "c")) (eq (qn "c" "Id") (qn "o" "CustId"))]
    (some wh)))]
#guard isC2a (checkJ (scalarT (eq (qn "o" "Id") (tv "@id"))) (scalarP (eq (qn "o" "Id") (pv "p_id"))) rCert)
#guard isC1 (checkJ (scalarT (.Bin ">" (qn "o" "Qty") (tv "@id")))
  (scalarP (.Bin ">" (qn "o" "Qty") (pv "p_id"))) rCert)
#guard isC2a (checkJ (scalarT (.Bin ">" (qn "o" "Qty") (tv "@id")) [xactAbort])
  (scalarP (.Bin ">" (qn "o" "Qty") (pv "p_id"))) rCert)
/- (Review of C3d.) An error that leaves a T-SQL routine under `XACT_ABORT
ON` dooms a caller's open transaction, losing the caller's own earlier
work, where a PostgreSQL caller's exception block keeps it (eval/queries
Q15): a routine that can fail names `ASM-NO-CALLER-TXN` even without a
write; one that cannot (a pick through a closed block) does not. -/
#guard residualOf depj Γj (scalarT (.Bin ">" (qn "o" "Qty") (tv "@id")) [xactAbort]) ==
  some ["ASM-TSQL-ANSI-NULLS", "ASM-TSQL-STRING-ARGS", "ASM-NO-CALLER-TXN"]
#guard residualOf depj Γj (scalarT (eq (qn "o" "Id") (tv "@id"))) ==
  some ["ASM-TSQL-ANSI-NULLS", "ASM-TSQL-STRING-ARGS"]
#guard residualOf depj Γj selfT == some ["ASM-TSQL-ANSI-NULLS", "ASM-TSQL-STRING-ARGS"]

/-! ## Refused: the key closure

`SELECT @x = ...` over a block that may keep more than one tuple: which
one T-SQL assigns is not specified (eval/queries Q2). -/

def pickT (froms : List Ast.From) (wh : Option Ast.Expr) (e : Ast.Expr := qn "c" "Score")
    (ps : List Ast.Param := [inT "@id", ioT "@q"]) : Ast.Routine :=
  tsqlProc ["r"] ps [selectF [asg "@q" e] froms wh]
def oc (on : Ast.Expr) : List Ast.From :=
  [jn (tbl ["dbo", "Ord"] (some "o")) (tbl ["dbo", "Cust"] (some "c")) on]
#guard (lowerT depj Γj (pickT (oc (eq (qn "c" "Id") (qn "o" "CustId"))) (some (eq (qn "o" "Id") (tv "@id"))))).isSome
-- A join condition on a column that is no key, picked into a variable.
#guard (lowerT depj Γj (pickT (oc (eq (qn "c" "Score") (qn "o" "Qty"))) (some (eq (qn "o" "Id") (tv "@id"))))).isNone
#guard whyT depj Γj (pickT (oc (eq (qn "c" "Score") (qn "o" "Qty"))) (some (eq (qn "o" "Id") (tv "@id")))) ==
  ["stmt:select:many-rows"]
-- A key compared with `>`, or under `OR`: not a step.
#guard (lowerT depj Γj (pickT (oc (.Bin ">=" (qn "c" "Id") (qn "o" "CustId"))) (some (eq (qn "o" "Id") (tv "@id"))))).isNone
#guard (lowerT depj Γj (pickT (oc (eq (qn "c" "Id") (qn "o" "CustId")))
  (some (.Bin "OR" (eq (qn "o" "Id") (tv "@id")) (eq (qn "o" "Id") (num 2)))))).isNone
-- A cross join of a determined table with one that is not.
#guard (lowerT depj Γj (pickT [cj (tbl ["dbo", "Ord"] (some "o")) (tbl ["dbo", "Cust"] (some "c"))]
  (some (eq (qn "o" "Id") (tv "@id"))) (qn "o" "Qty"))).isNone
-- A step whose value reads the source it determines.
#guard (lowerT depj Γj (pickT (oc (eq (qn "c" "Id") (qn "c" "Score"))) (some (eq (qn "o" "Id") (tv "@id"))))).isNone
-- A keyless table.
#guard (lowerT depj Γj (pickT [tbl ["dbo", "Ord"] (some "o"), tbl ["dbo", "Pair"] (some "p")]
  (some (and_ (eq (qn "o" "Id") (tv "@id")) (eq (qn "p" "A") (num 1)))) (qn "o" "Qty"))).isNone
/- A step through `=` across kinds: a `date` key against a `datetime2(6)`
variable is `toTs(d) = @t` in the core (C2c), no `i.k = e`; the key is
not determined. The same key against a `date` variable is a step. -/
def evT' (ty : Ast.Ty) : Ast.Routine :=
  tsqlProc ["r"] [inT "@t" ty, ioT "@q"] [selectF [asg "@q" (qn "e" "N")]
    [tbl ["dbo", "Ev"] (some "e")] (some (eq (qn "e" "D") (tv "@t")))]
#guard (lowerT depj Γj (evT' (.Ty "DATE" []))).isSome
#guard (lowerT depj Γj (evT' (.Ty "DATETIME2" ["6"]))).isNone
#guard whyT depj Γj (evT' (.Ty "DATETIME2" ["6"])) == ["stmt:select:many-rows"]
/- The other way round is a step: a timestamp key against a `date`
variable is `t = toTs(@d)`, the date widened to midnight on both engines
(review of C3d), which fixes the key. -/
def evtT' (ty : Ast.Ty) : Ast.Routine :=
  tsqlProc ["r"] [inT "@d" ty, ioT "@q"] [selectF [asg "@q" (qn "e" "N")]
    [tbl ["dbo", "Evt"] (some "e")] (some (eq (qn "e" "T") (tv "@d")))]
#guard (lowerT depj Γj (evtT' (.Ty "DATE" []))).isSome
#guard (lowerT depj Γj (evtT' (.Ty "DATETIME2" ["6"]))).isSome
#guard (lowerT depj Γj (evtT' (.Ty "DATE" []))).map (·.body) ==
  some (.seq (.assign statusVar (.lit 0)) (.query ⟨⟨[evtT.toDef],
    [.sc (.cmp .eq (.col "0.t") (.toTs (.var "@d")))]⟩⟩ (.row false [("@q", .sc (.col "0.n"))])))
/- A step through string equality: under the binary-collation fact a
`Name = @n` is `streq` in the core (T-SQL `rtrim` on both sides), never
`i.k = e`, and `Name` is no key either. -/
def Γjb : Catalog := { Γj with binaryCollation := true }
#guard (lowerT depj Γjb (pickT [tbl ["dbo", "Cust"] (some "c")] (some (eq (qn "c" "Name") (tv "@n")))
  (ps := [inT "@n" (.Ty "VARCHAR" ["10"]), ioT "@q"]))).isNone
#guard (lowerT depj Γjb (tsqlProc ["r"] [inT "@n" (.Ty "VARCHAR" ["10"]), ioT "@q"]
  [set (tv "@q") (.Subquery (selF [item (qn "c" "Score")] [tbl ["dbo", "Cust"] (some "c")]
    (some (eq (qn "c" "Name") (tv "@n")))))])).isSome
-- The same pick reads its target in the condition: refused (T-SQL reads
-- the variable per row as it assigns it).
#guard (lowerT depj Γj (pickT (oc (eq (qn "c" "Id") (qn "o" "CustId"))) (some (eq (qn "o" "Id") (tv "@q"))))).isNone

/-! ## Refused: names -/

-- T-SQL: an unqualified name in two tables (209, eval/queries Q12).
def ambT : Ast.Routine := pickT (oc (eq (qn "c" "Id") (qn "o" "CustId"))) (some (eq (tc "Id") (tv "@id")))
#guard (lowerT depj Γj ambT).isNone
#guard whyT depj Γj ambT == ["name:ambiguous"]
-- PostgreSQL: the same (42702).
def forJ (wh : Ast.Expr) (on : Ast.Expr := eq (qn "c" "id") (qn "o" "custid"))
    (ps : List Ast.Param := [inP "p_id", ioP "p_q"]) (e : Ast.Expr := qn "c" "score")
    (name : List String := ["r"]) : Ast.Routine :=
  pgProc name ps [] [forF [als e "sqldiff_c1"] [jn (tbl ["ord"] (some "o")) (tbl ["cust"] (some "c")) on]
    (some wh) [field "p_q" "sqldiff_r" "sqldiff_c1"]]
#guard (lowerP Γj (forJ (eq (qn "o" "id") (pv "p_id")))).isSome
#guard (lowerP Γj (forJ (eq (pv "id") (pv "p_id")))).isNone
#guard whyP Γj (forJ (eq (pv "id") (pv "p_id"))) == ["name:ambiguous"]
-- PostgreSQL: a column of the second table that is also a variable (a
-- parameter `score`; 42702, eval/queries Q12), unqualified.
#guard (lowerP Γj (forJ (and_ (eq (qn "o" "id") (pv "p_id")) (.IsNull (pv "score") true))
  (ps := [inP "p_id", ioP "p_q", inP "score"]))).isNone
#guard whyP Γj (forJ (and_ (eq (qn "o" "id") (pv "p_id")) (.IsNull (pv "score") true))
  (ps := [inP "p_id", ioP "p_q", inP "score"])) == ["name:column-variable"]
-- ... qualified, it is the column.
#guard (lowerP Γj (forJ (and_ (eq (qn "o" "id") (pv "p_id")) (.IsNull (qn "c" "score") true))
  (ps := [inP "p_id", ioP "p_q", inP "score"]))).isSome
-- PostgreSQL: a qualifier that is a variable (a parameter), the loop's
-- record, or the routine's name (eval/queries Q12: 42702, 55000, 42702).
#guard (lowerP Γj (pgProc ["r"] [inP "p_id", ioP "p_q"] [] [forF [als (qn "p_id" "score") "sqldiff_c1"]
  [tbl ["cust"] (some "p_id")] (some (eq (qn "p_id" "id") (num 1))) [field "p_q" "sqldiff_r" "sqldiff_c1"]])).isNone
#guard whyP Γj (pgProc ["r"] [inP "p_id", ioP "p_q"] [] [forF [als (qn "p_id" "score") "sqldiff_c1"]
  [tbl ["cust"] (some "p_id")] (some (eq (qn "p_id" "id") (num 1))) [field "p_q" "sqldiff_r" "sqldiff_c1"]]) ==
  ["stmt:select:from:qualifier"]
#guard (lowerP Γj (pgProc ["r"] [inP "p_id", ioP "p_q"] [] [forF [als (qn "sqldiff_r" "score") "sqldiff_c1"]
  [tbl ["cust"] (some "sqldiff_r")] (some (eq (qn "sqldiff_r" "id") (pv "p_id")))
  [field "p_q" "sqldiff_r" "sqldiff_c1"]])).isNone
#guard (lowerP Γj (forJ (eq (qn "o" "id") (pv "p_id")) (name := ["o"]))).isNone
#guard (lowerP Γj (forJ (eq (qn "o" "id") (pv "p_id")) (name := ["public", "c"]))).isNone
-- PostgreSQL: a reference name read as a column (a whole-row reference).
#guard (lowerP Γj (forJ (and_ (eq (qn "o" "id") (pv "p_id")) (.IsNull (pv "c") true)))).isNone
-- An aliased table's own name as qualifier (4104, 42P01).
#guard (lowerT depj Γj (pickT (oc (eq (qn "Cust" "Id") (qn "o" "CustId"))) (some (eq (qn "o" "Id") (tv "@id"))))).isNone
#guard whyT depj Γj (pickT (oc (eq (qn "Cust" "Id") (qn "o" "CustId"))) (some (eq (qn "o" "Id") (tv "@id")))) ==
  ["stmt:select:from:qualifier"]
#guard (lowerP Γj (forJ (eq (qn "o" "id") (pv "p_id")) (on := eq (qn "cust" "id") (qn "o" "custid")))).isNone
-- T-SQL: a qualifier or a column in another case than declared (4104 and
-- 207 in a case-sensitive database, eval/queries Q12).
#guard (lowerT depj Γj (pickT (oc (eq (qn "C" "Id") (qn "o" "CustId"))) (some (eq (qn "o" "Id") (tv "@id"))))).isNone
#guard (lowerT depj Γj (pickT (oc (eq (qn "c" "ID") (qn "o" "CustId"))) (some (eq (qn "o" "Id") (tv "@id"))))).isNone
-- Three-part names.
#guard (lowerT depj Γj (pickT (oc (eq (.Name ["dbo", "c", "Id"]) (qn "o" "CustId")))
  (some (eq (qn "o" "Id") (tv "@id"))))).isNone
-- Two sources with one reference name (1013, 42712), also up to case in
-- T-SQL.
#guard (lowerT depj Γj (pickT [jn (tbl ["dbo", "Ord"]) (tbl ["dbo", "Ord"]) (eq (qn "Ord" "Id") (qn "Ord" "CustId"))]
  (some (eq (qn "Ord" "Id") (tv "@id"))) (qn "Ord" "Qty"))).isNone
#guard whyT depj Γj (pickT [jn (tbl ["dbo", "Ord"]) (tbl ["dbo", "Ord"]) (eq (qn "Ord" "Id") (qn "Ord" "CustId"))]
  (some (eq (qn "Ord" "Id") (tv "@id"))) (qn "Ord" "Qty")) == ["stmt:select:from:ref"]
#guard (lowerT depj Γj (pickT [tbl ["dbo", "Ord"] (some "x"), tbl ["dbo", "Cust"] (some "X")]
  (some (and_ (eq (qn "x" "Id") (tv "@id")) (eq (qn "X" "Id") (qn "x" "CustId")))) (qn "X" "Score"))).isNone
-- An `ON` that names a table joined later, or another comma item (4104,
-- 42P01, eval/queries Q12).
#guard (lowerT depj Γj (pickT [jn (jn (tbl ["dbo", "Ord"] (some "o")) (tbl ["dbo", "Cust"] (some "c"))
    (eq (qn "t" "A") (qn "o" "Id"))) (tbl ["dbo", "Two"] (some "t")) (eq (qn "t" "B") (qn "c" "Id"))]
  (some (eq (qn "o" "Id") (tv "@id"))) (qn "o" "Qty"))).isNone
#guard (lowerT depj Γj (pickT [tbl ["dbo", "Two"] (some "t"),
    jn (tbl ["dbo", "Ord"] (some "o")) (tbl ["dbo", "Cust"] (some "c")) (eq (qn "c" "Id") (qn "t" "A"))]
  none countStarE)).isNone
-- ... while the `WHERE` sees every source.
#guard (lowerT depj Γj (pickT [tbl ["dbo", "Two"] (some "t"),
    jn (tbl ["dbo", "Ord"] (some "o")) (tbl ["dbo", "Cust"] (some "c")) (eq (qn "c" "Id") (qn "o" "CustId"))]
  (some (eq (qn "c" "Id") (qn "t" "A"))) countStarE)).isSome

/-! ## Refused: the FROM -/

-- Outer joins, a hint, a derived table.
def leftT : Ast.Routine :=
  pickT [.Join "LEFT" (tbl ["dbo", "Ord"] (some "o")) (tbl ["dbo", "Cust"] (some "c"))
    (some (eq (qn "c" "Id") (qn "o" "CustId")))] (some (eq (qn "o" "Id") (tv "@id")))
#guard (lowerT depj Γj leftT).isNone
#guard whyT depj Γj leftT == ["stmt:select:from:join:left"]
#guard whyT depj Γj (pickT [.Join "FULL" (tbl ["dbo", "Ord"] (some "o")) (tbl ["dbo", "Cust"] (some "c"))
    (some (eq (qn "c" "Id") (qn "o" "CustId")))] (some (eq (qn "o" "Id") (tv "@id")))) ==
  ["stmt:select:from:join:full"]
#guard (lowerT depj Γj (pickT [jn (tbl ["dbo", "Ord"] (some "o")) (.Table ["dbo", "Cust"] (some "c") ["NOLOCK"])
  (eq (qn "c" "Id") (qn "o" "CustId"))] (some (eq (qn "o" "Id") (tv "@id"))))).isNone
-- A table outside the catalog in a join.
#guard whyT depj Γj (pickT [jn (tbl ["dbo", "Ord"] (some "o")) (tbl ["dbo", "Nope"] (some "c"))
  (eq (qn "c" "Id") (qn "o" "CustId"))] (some (eq (qn "o" "Id") (tv "@id")))) ==
  ["stmt:select:table:unknown"]

/-! ## The core as an oracle

Rows of `ord` (`id`, `qty`, `custid`): (1, 10, 1), (2, 20, NULL), (3, 30,
2), (4, NULL, 2); of `cust` (`id`, `score`, `name`): (1, 100, NULL), (2,
200, NULL), (3, NULL, NULL); the rows of eval/queries Q11. The outputs
are `@q`, `@s`, `@n` and the status. -/

def dbj : Db Unit :=
  [("ord", [[I 1, I 10, I 1], [I 2, I 20, N], [I 3, I 30, I 2], [I 4, N, I 2]]),
   ("cust", [[I 1, I 100, N], [I 2, I 200, N], [I 3, N, N]])]

def lT := lowerT depj Γj lookT
def lP := lowerP Γj lookP

/- `look(25)`: no order has key 25 (`@q`, `@s` keep 7, 8 from the pick);
one order has `Qty > 25` (3, customer 2): `@s = 200`; three orders have a
customer (the NULL `CustId` matches nothing): `@n = 3`; no `Qty` equals a
`Score`: `EXISTS` is FALSE. -/
#guard runD lT [I 25, I 7, I 8, N] dbj == some (.done [I 7, I 200, I 3, I 0] none 0 dbj [])
#guard runD lP [I 25, I 7, I 8, N] dbj == runD lT [I 25, I 7, I 8, N] dbj
/- `look(35)`: no order has `Qty > 35`: the scalar subquery is NULL. -/
#guard runD lT [I 35, I 7, I 8, N] dbj == some (.done [I 7, N, I 3, I 0] none 0 dbj [])
#guard runD lP [I 35, I 7, I 8, N] dbj == runD lT [I 35, I 7, I 8, N] dbj
/- `look(3)`: the pick by key through the join gives order 3's `Qty`
and its customer's `Score` (eval/queries Q11: 30, 200); then two orders
with a customer have `Qty > 3` (1 and 3): the scalar subquery over the
join raises `cardinality` on both sides (T-SQL 512, PostgreSQL 21000,
eval/queries Q13), and the routine fails. -/
#guard runD lT [I 3, N, N, N] dbj == some (.failed .cardinality [])
#guard runD lP [I 3, N, N, N] dbj == runD lT [I 3, N, N, N] dbj
#guard runD (lowerT depj Γj (lookT (lookBodyT.take 1))) [I 3, N, N, N] dbj ==
  some (.done [I 30, I 200, N, I 0] none 0 dbj [])
/- `look(2)`: order 2's `CustId` is NULL, so the join keeps no tuple and
the pick keeps the variables (eval/queries Q11); the scalar subquery
sees orders 1 and 3 again. -/
#guard runD (lowerT depj Γj (lookT (lookBodyT.take 1))) [I 2, I 7, I 8, N] dbj ==
  some (.done [I 7, I 8, N, I 0] none 0 dbj [])
/- The self-join: order 4's customer id is 2, and order 2's `Qty` is
20 (eval/queries Q11, "pick through a self-join"). -/
#guard runD (lowerT depj Γj selfT) [I 4, N] dbj == some (.done [I 20, I 0] none 0 dbj [])
#guard runD (lowerP Γj selfP) [I 4, N] dbj == runD (lowerT depj Γj selfT) [I 4, N] dbj

end Sqldiff.Mirror.ExamplesC3d
