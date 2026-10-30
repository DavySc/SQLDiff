/-
Checked examples for C3b, single-table queries: the shapes the converter
writes (`SELECT @x = col` against the PL/pgSQL `FOR` block, `IF [NOT]
EXISTS`, a scalar subquery, `COUNT(*)`), confirmed; the shapes outside
the spec, refused, with the runs that show why where a run can; and runs
of the core interpreter on table states. Every `#guard` runs on every
`lake build`.
-/
import Sqldiff.Mirror.ExamplesC3a

namespace Sqldiff.Mirror.ExamplesC3b

open Sqldiff.Core (Obs Db)
open Sqldiff.Mirror.Examples
open Sqldiff.Mirror.ExamplesC3a (acct pairT Γd depd tc del acctRows db0 runD nocount acctDef)
open Sqldiff.Gate (Answer)

/-! ## Building blocks

The table is C3a's `acct` (`Id` key, `Qty` nullable, `Total NOT NULL`),
`dbo.Acct` in T-SQL and `acct` in PostgreSQL. `two` has a key of two
columns. -/

def twoK : CatTable :=
  { name := "two", cols := [⟨"a", "A", true, true, .int .i32, none⟩, ⟨"b", "B", true, true, .int .i32, none⟩, ⟨"v", "V", false, false, .int .i32, none⟩] }

def Γq : Catalog := { Γd with tables := Γd.tables ++ [twoK] }
def depq : Deployment := { depd with tables := depd.tables ++ [("dbo.Two", "two")] }

/-- A plain `SELECT items FROM tgt [WHERE wh] [INTO into]`. -/
def sel (items : List Ast.SelItem) (tgt : List String) (wh : Option Ast.Expr)
    (into : List Ast.Expr := []) : Ast.Select :=
  .Select [] false [] false none items into false [.Table tgt none []] wh [] none [] none none [] none

/-- T-SQL `@x = e` in a select list. -/
def asg (x : String) (e : Ast.Expr) : Ast.SelItem := .SelItem e none (some x)
/-- `e AS a`. -/
def als (e : Ast.Expr) (a : String) : Ast.SelItem := .SelItem e (some a) none
/-- `e`. -/
def item (e : Ast.Expr) : Ast.SelItem := .SelItem e none none
def countStarE : Ast.Expr := .CallE (.CallF ["COUNT"] [] false [] false false true)
def one : Ast.SelItem := item (num 1)

def selectT (items : List Ast.SelItem) (tgt : List String) (wh : Option Ast.Expr) : Ast.Node :=
  .NStmt (.SSelect (sel items tgt wh))

/-- The converter's PL/pgSQL block for `SELECT @x = e, ...`. -/
def forBlock (items : List Ast.SelItem) (tgt : List String) (wh : Option Ast.Expr)
    (body : List Ast.Node) (r : String := "sqldiff_r") (ty : Ast.Ty := .Ty "RECORD" [])
    (r' : String := r) : Ast.Node :=
  .NBlock (.Block [.Declare r (some ty) none] [.ForQuery r' (sel items tgt wh) body])

def field (x r a : String) : Ast.Node := set (pv x) (.Name [r, a])

def existsT (tgt : List String) (wh : Option Ast.Expr) : Ast.Expr := .Exists (sel [one] tgt wh)

def keyT : Ast.Expr := .Bin "=" (tc "Id") (tv "@id")
def keyP : Ast.Expr := .Bin "=" (pv "Id") (pv "p_id")

def lookCert : Cert :=
  ⟨[("@id", "p_id"), ("@q", "p_q"), ("@t", "p_t"), ("@e", "p_e"), (statusVar, statusVar)]⟩

def checkQ (a b : Ast.Routine) (cert : Cert := lookCert) : Answer := check depq Γq cert a b

/-! ## The pair

The converter's input and output (as `sqldiff convert --to postgres`
writes them, the comments left out):

```sql
CREATE PROCEDURE dbo.look @id int, @q int OUTPUT, @t int OUTPUT, @e int OUTPUT AS
BEGIN
  SET XACT_ABORT ON;
  SET NOCOUNT ON;
  SELECT @q = Qty, @t = Total FROM dbo.Acct WHERE Id = @id;
  IF EXISTS (SELECT 1 FROM dbo.Acct WHERE Qty > @id)
    SET @e = 1;
  IF NOT EXISTS (SELECT 1 FROM dbo.Acct WHERE Qty > @id)
    SET @e = 2;
  ELSE
    SET @e = 3;
  SET @t = (SELECT Total FROM dbo.Acct WHERE Id = @id);
  SELECT @e = COUNT(*) FROM dbo.Acct WHERE Qty > 0;
END
```

```sql
CREATE OR REPLACE PROCEDURE look(p_id integer, INOUT p_q integer,
    INOUT p_t integer, INOUT p_e integer) LANGUAGE plpgsql AS $$
BEGIN
  DECLARE
  sqldiff_r record;
  BEGIN
  FOR sqldiff_r IN SELECT Qty AS sqldiff_c1, Total AS sqldiff_c2 FROM Acct WHERE Id = p_id LOOP
    p_q := sqldiff_r.sqldiff_c1;
    p_t := sqldiff_r.sqldiff_c2;
  END LOOP;
  END;
  IF EXISTS (SELECT 1 FROM Acct WHERE Qty > p_id) THEN
    p_e := 1;
  END IF;
  IF NOT EXISTS (SELECT 1 FROM Acct WHERE Qty > p_id) THEN
    p_e := 2;
  ELSE
    p_e := 3;
  END IF;
  p_t := (SELECT Total FROM Acct WHERE Id = p_id);
  SELECT COUNT(*) INTO p_e FROM Acct WHERE Qty > 0;
END; $$;
```
-/

def lookBodyT : List Ast.Node := [
  selectT [asg "@q" (tc "Qty"), asg "@t" (tc "Total")] ["dbo", "Acct"] (some keyT),
  .If (existsT ["dbo", "Acct"] (some (.Bin ">" (tc "Qty") (tv "@id")))) [set (tv "@e") (num 1)] [],
  .If (.Unary "NOT" (existsT ["dbo", "Acct"] (some (.Bin ">" (tc "Qty") (tv "@id")))))
    [set (tv "@e") (num 2)] [set (tv "@e") (num 3)],
  set (tv "@t") (.Subquery (sel [item (tc "Total")] ["dbo", "Acct"] (some keyT))),
  selectT [asg "@e" countStarE] ["dbo", "Acct"] (some (.Bin ">" (tc "Qty") (num 0)))]

def lookT (body : List Ast.Node := lookBodyT) (lead : List Ast.Node := [xactAbort, nocount]) :
    Ast.Routine :=
  tsqlProc ["dbo", "look"] [inT "@id", ioT "@q", ioT "@t", ioT "@e"] (lead ++ body)

def lookBodyP : List Ast.Node := [
  forBlock [als (pv "Qty") "sqldiff_c1", als (pv "Total") "sqldiff_c2"] ["Acct"] (some keyP)
    [field "p_q" "sqldiff_r" "sqldiff_c1", field "p_t" "sqldiff_r" "sqldiff_c2"],
  .If (existsT ["Acct"] (some (.Bin ">" (pv "Qty") (pv "p_id")))) [set (pv "p_e") (num 1)] [],
  .If (.Unary "NOT" (existsT ["Acct"] (some (.Bin ">" (pv "Qty") (pv "p_id")))))
    [set (pv "p_e") (num 2)] [set (pv "p_e") (num 3)],
  set (pv "p_t") (.Subquery (sel [item (pv "Total")] ["Acct"] (some keyP))),
  .NStmt (.SSelect (sel [item countStarE] ["Acct"] (some (.Bin ">" (pv "Qty") (num 0)))
    [pv "p_e"]))]

def lookP (body : List Ast.Node := lookBodyP) (ps : List Ast.Param :=
    [inP "p_id", ioP "p_q", ioP "p_t", ioP "p_e"]) : Ast.Routine :=
  pgProc ["look"] ps [] body

/-! ## Confirmed -/

#guard isC2a (checkQ lookT lookP)
-- `COUNT(*)` can overflow (2^31 rows); `Id = @id` compares with `=`.
#guard residualOf depq Γq lookT == some ["ASM-TSQL-ARITH-ERRORS", "ASM-TSQL-ANSI-NULLS", "ASM-TSQL-STRING-ARGS"]
#guard (lowerT depq Γq lookT).map Core.Routine.hasQuery == some true
-- The `EXISTS` variable is a local of both lowered routines.
#guard (lowerT depq Γq lookT).map (·.locals) == some [existsVar]
#guard (lowerP Γq lookP).map (·.locals) == some [existsVar]

/- The lowered T-SQL `SELECT @q = Qty, @t = Total ... WHERE Id = @id`, as
the core sees it. -/
#guard (lowerT depq Γq (tsqlProc ["r"] [inT "@id", ioT "@q", ioT "@t"]
    [selectT [asg "@q" (tc "Qty"), asg "@t" (tc "Total")] ["dbo", "Acct"] (some keyT)])).map
    (·.body) ==
  some (.seq (.assign statusVar (.lit 0))
    (.query acctDef (some (.sc (.cmp .eq (.col "id") (.var "@id"))))
      (.row false [("@q", .sc (.col "qty")), ("@t", .sc (.col "total"))])))

/- Nothing can fail without `COUNT` (a `WHERE` and values that cannot
raise): in the slice without `XACT_ABORT ON`. -/
def readT : Ast.Routine :=
  tsqlProc ["r"] [inT "@id", ioT "@q"] [
    selectT [asg "@q" (tc "Qty")] ["dbo", "Acct"] (some keyT),
    .If (existsT ["dbo", "Acct"] none) [] [set (tv "@q") .Null]]
def readP : Ast.Routine :=
  pgProc ["r"] [inP "p_id", ioP "p_q"] [] [
    forBlock [als (pv "qty") "sqldiff_c1"] ["acct"] (some keyP) [field "p_q" "sqldiff_r" "sqldiff_c1"],
    .If (existsT ["acct"] none) [] [set (pv "p_q") .Null]]
def readCert : Cert := ⟨[("@id", "p_id"), ("@q", "p_q"), (statusVar, statusVar)]⟩
#guard isC2a (checkQ readT readP readCert)
#guard residualOf depq Γq readT == some ["ASM-TSQL-ANSI-NULLS", "ASM-TSQL-STRING-ARGS"]
-- `SELECT *` in `EXISTS`, `e = k` the other way round, and `@x` in the
-- `WHERE` of a read with the key fixed by a literal.
#guard isC2a (checkQ
  (tsqlProc ["r"] [inT "@id", ioT "@q"] [
    selectT [asg "@q" (tc "Total")] ["dbo", "Acct"] (some (.Bin "=" (num 7) (tc "Id"))),
    .If (.Exists (sel [item (.Star none)] ["dbo", "Acct"] (some (.Bin "=" (tc "Qty") (tv "@id")))))
      [set (tv "@q") (num 0)] []])
  (pgProc ["r"] [inP "p_id", ioP "p_q"] [] [
    forBlock [als (pv "total") "c"] ["acct"] (some (.Bin "=" (num 7) (pv "id"))) [field "p_q" "r" "c"]
      (r := "r"),
    .If (.Exists (sel [item (.Star none)] ["acct"] (some (.Bin "=" (pv "qty") (pv "p_id")))))
      [set (pv "p_q") (num 0)] []])
  readCert)
-- A key of two columns, both fixed (with a further condition).
#guard isC2a (checkQ
  (tsqlProc ["r"] [inT "@id", ioT "@q"] [selectT [asg "@q" (tc "V")] ["dbo", "Two"]
    (some (.Bin "AND" (.Bin "=" (tc "B") (num 2)) (.Bin "AND" (.Bin "=" (tc "A") (tv "@id"))
      (.IsNull (tc "V") true))))])
  (pgProc ["r"] [inP "p_id", ioP "p_q"] [] [forBlock [als (pv "v") "sqldiff_c1"] ["two"]
    (some (.Bin "AND" (.Bin "=" (pv "b") (num 2)) (.Bin "AND" (.Bin "=" (pv "a") (pv "p_id"))
      (.IsNull (pv "v") true)))) [field "p_q" "sqldiff_r" "sqldiff_c1"]])
  readCert)
-- `SET @q = (SELECT COUNT(*) ...)` against `p_q := (SELECT count(*) ...)`
-- and against `SELECT count(*) INTO p_q`: the same core statement.
def cntT : Ast.Routine :=
  tsqlProc ["r"] [inT "@id", ioT "@q"] [xactAbort,
    set (tv "@q") (.Subquery (sel [item countStarE] ["dbo", "Acct"] none))]
#guard isC2a (checkQ cntT
  (pgProc ["r"] [inP "p_id", ioP "p_q"] [] [
    set (pv "p_q") (.Subquery (sel [item (.CallE (.CallF ["count"] [] false [] false false true))]
      ["acct"] none))]) readCert)
#guard isC2a (checkQ cntT
  (pgProc ["r"] [inP "p_id", ioP "p_q"] [] [
    .NStmt (.SSelect (sel [item countStarE] ["acct"] none [pv "p_q"]))]) readCert)

/-! ## Refused: the at-most-one-row rule

`SELECT @x = col` keeps the value of one row out of several, which one
neither vendor says (eval/queries Q2). Without a `WHERE` that fixes the
whole primary key with `=`, it is outside the spec. -/

def selAcct (wh : Option Ast.Expr) (items : List Ast.SelItem := [asg "@q" (tc "Qty")]) :
    Ast.Routine :=
  tsqlProc ["r"] [inT "@id", ioT "@q"] [selectT items ["dbo", "Acct"] wh]
-- The key fixed: in. Each variation below: out.
#guard (lowerT depq Γq (selAcct (some keyT))).isSome
#guard (lowerT depq Γq (selAcct none)).isNone
#guard (lowerT depq Γq (selAcct (some (.Bin "=" (tc "Qty") (tv "@id"))))).isNone
#guard (lowerT depq Γq (selAcct (some (.Bin ">=" (tc "Id") (tv "@id"))))).isNone
#guard (lowerT depq Γq (selAcct (some (.Bin "OR" keyT (.Bin "=" (tc "Id") (num 2)))))).isNone
#guard (lowerT depq Γq (selAcct (some (.Bin "=" (tc "Id") (tc "Total"))))).isNone
-- Half of a two-column key.
#guard (lowerT depq Γq (tsqlProc ["r"] [inT "@id", ioT "@q"]
  [selectT [asg "@q" (tc "V")] ["dbo", "Two"] (some (.Bin "=" (tc "A") (tv "@id")))])).isNone
-- A table without a key.
#guard (lowerT depq Γq (tsqlProc ["r"] [ioT "@q"]
  [selectT [asg "@q" (tc "A")] ["dbo", "Pair"] (some (.Bin "=" (tc "A") (num 1)))])).isNone
-- (C2c) A `bigint` variable compares with the `int` key in the slice.
#guard (lowerT depq Γq (tsqlProc ["r"] [inT "@id" bigint, ioT "@q"]
  [selectT [asg "@q" (tc "Qty")] ["dbo", "Acct"] (some keyT)])).isSome
-- The key on the C1 path (a `decimal` variable): not seen as fixed.
#guard (lowerT depq Γq (tsqlProc ["r"] [inT "@id" (.Ty "DECIMAL" ["10", "0"]), ioT "@q"]
  [selectT [asg "@q" (tc "Qty")] ["dbo", "Acct"] (some keyT)])).isNone

/-! ## Refused: what the values and targets may be -/

-- A target the `WHERE` reads: T-SQL reads the variable per row as it is
-- assigned, PostgreSQL's query parameters are fixed.
#guard (lowerT depq Γq (selAcct (some (.Bin "=" (tc "Id") (tv "@q"))))).isNone
-- A value that reads a variable (`SELECT @q = @q + Qty` is a running total
-- in T-SQL), or can raise (computed on rows the `WHERE` drops).
#guard (lowerT depq Γq (selAcct (some keyT) [asg "@q" (.Bin "+" (tv "@q") (tc "Qty"))])).isNone
#guard (lowerT depq Γq (selAcct (some keyT) [asg "@q" (.Bin "+" (tc "Qty") (num 1))])).isNone
#guard (lowerT depq Γq (selAcct (some keyT) [asg "@q" (tc "Qty"), asg "@q" (tc "Total")])).isNone
-- (C2c) A `bigint` target takes an `int` column; a `smallint` one, or a
-- `decimal`, would convert.
#guard (lowerT depq Γq (tsqlProc ["r"] [inT "@id", ioT "@q" bigint]
  [selectT [asg "@q" (tc "Qty")] ["dbo", "Acct"] (some keyT)])).isSome
#guard (lowerT depq Γq (tsqlProc ["r"] [inT "@id", ioT "@q" (.Ty "SMALLINT" [])]
  [selectT [asg "@q" (tc "Qty")] ["dbo", "Acct"] (some keyT)])).isNone
#guard (lowerT depq Γq (tsqlProc ["r"] [inT "@id", ioT "@q" (.Ty "DECIMAL" ["10", "0"])]
  [selectT [asg "@q" (tc "Qty")] ["dbo", "Acct"] (some keyT)])).isNone
-- A select list without assignments (a result set), or mixed.
#guard (lowerT depq Γq (selAcct (some keyT) [item (tc "Qty")])).isNone
#guard (lowerT depq Γq (selAcct (some keyT) [asg "@q" (tc "Qty"), item (tc "Total")])).isNone
-- `TOP`, `ORDER BY`, an alias, a hint, a join: outside the shape.
#guard (lowerT depq Γq (tsqlProc ["r"] [inT "@id", ioT "@q"] [.NStmt (.SSelect
  (.Select [] false [] false (some (num 1)) [asg "@q" (tc "Qty")] [] false
    [.Table ["dbo", "Acct"] none []] (some keyT) [] none [] none none [] none))])).isNone
#guard (lowerT depq Γq (tsqlProc ["r"] [inT "@id", ioT "@q"] [.NStmt (.SSelect
  (.Select [] false [] false none [asg "@q" (tc "Qty")] [] false
    [.Table ["dbo", "Acct"] none ["NOLOCK"]] (some keyT) [] none [] none none [] none))])).isNone
#guard (lowerT depq Γq (tsqlProc ["r"] [inT "@id", ioT "@q"] [.NStmt (.SSelect
  (.Select [] false [] false none [asg "@q" (tc "Qty")] [] false
    [.Table ["dbo", "Acct"] (some "a") []] (some keyT) [] none [] none none [] none))])).isNone
-- In a function.
#guard (lowerT depq Γq (.Routine .Function ["f"] [inT "@id"] (some int) .Caller none
  (.Block [] [declT "@q" none, selectT [asg "@q" (tc "Qty")] ["dbo", "Acct"] (some keyT),
    ret (some (tv "@q"))]))).isNone

/-! ## Refused: the PL/pgSQL `FOR` block

Only the converter's exact shape: one `record` declaration, the loop over
it, the body the assignments of its fields in order. -/

def forP (b : Ast.Node) (ps : List Ast.Param := [inP "p_id", ioP "p_q"]) : Ast.Routine :=
  pgProc ["r"] ps [] [b]
def okFor : Ast.Node :=
  forBlock [als (pv "qty") "sqldiff_c1"] ["acct"] (some keyP) [field "p_q" "sqldiff_r" "sqldiff_c1"]
#guard (lowerP Γq (forP okFor)).isSome
-- The record named like a parameter (it would shadow it).
#guard (lowerP Γq (forP okFor [inP "p_id", ioP "p_q", inP "sqldiff_r"])).isNone
-- Not a record, a default, another loop variable.
#guard (lowerP Γq (forP (forBlock [als (pv "qty") "sqldiff_c1"] ["acct"] (some keyP)
  [field "p_q" "sqldiff_r" "sqldiff_c1"] (ty := integer)))).isNone
#guard (lowerP Γq (forP (.NBlock (.Block [.Declare "sqldiff_r" (some (.Ty "RECORD" [])) (some .Null)]
  [.ForQuery "sqldiff_r" (sel [als (pv "qty") "sqldiff_c1"] ["acct"] (some keyP))
    [field "p_q" "sqldiff_r" "sqldiff_c1"]])))).isNone
#guard (lowerP Γq (forP (forBlock [als (pv "qty") "sqldiff_c1"] ["acct"] (some keyP)
  [field "p_q" "sqldiff_r" "sqldiff_c1"] (r' := "other")))).isNone
-- The body reads another field, reads them out of order, does more, or
-- writes the record.
#guard (lowerP Γq (forP (forBlock [als (pv "qty") "sqldiff_c1"] ["acct"] (some keyP)
  [field "p_q" "sqldiff_r" "sqldiff_c2"]))).isNone
#guard (lowerP Γq (forP (forBlock [als (pv "qty") "sqldiff_c1", als (pv "total") "sqldiff_c2"]
  ["acct"] (some keyP) [field "p_q" "sqldiff_r" "sqldiff_c2", field "p_id" "sqldiff_r" "sqldiff_c1"])
  [ioP "p_id", ioP "p_q"])).isNone
#guard (lowerP Γq (forP (forBlock [als (pv "qty") "sqldiff_c1"] ["acct"] (some keyP)
  [field "p_q" "sqldiff_r" "sqldiff_c1", set (pv "p_q") (num 1)]))).isNone
#guard (lowerP Γq (forP (forBlock [als (pv "qty") "sqldiff_c1"] ["acct"] (some keyP)
  [field "sqldiff_r" "sqldiff_r" "sqldiff_c1"]))).isNone
-- A `FOR` loop outside the block, or the block with another statement.
#guard (lowerP Γq (pgProc ["r"] [inP "p_id", ioP "p_q"] [.Declare "sqldiff_r" (some (.Ty "RECORD" [])) none]
  [.ForQuery "sqldiff_r" (sel [als (pv "qty") "sqldiff_c1"] ["acct"] (some keyP))
    [field "p_q" "sqldiff_r" "sqldiff_c1"]])).isNone
#guard (lowerP Γq (forP (.NBlock (.Block [.Declare "sqldiff_r" (some (.Ty "RECORD" [])) none]
  [.ForQuery "sqldiff_r" (sel [als (pv "qty") "sqldiff_c1"] ["acct"] (some keyP))
    [field "p_q" "sqldiff_r" "sqldiff_c1"], set (pv "p_q") (num 1)])))).isNone
-- A name of the query that is a column and a variable: PostgreSQL raises
-- 42702 (eval/queries Q9). Here the parameter `qty`; and the record itself.
#guard (lowerP Γq (forP okFor [inP "p_id", ioP "p_q", inP "qty"])).isNone
#guard (lowerP Γq (forP (forBlock [als (pv "qty") "sqldiff_c1"] ["acct"]
  (some (.Bin "AND" keyP (.IsNull (pv "sqldiff_r") false))) [field "p_q" "sqldiff_r" "sqldiff_c1"]))).isNone
-- `SELECT qty INTO p_q ... WHERE id = p_id`: PL/pgSQL sets NULL on no
-- row (eval/queries Q10); not a shape of the spec.
#guard (lowerP Γq (forP (.NStmt (.SSelect (sel [item (pv "qty")] ["acct"] (some keyP) [pv "p_q"]))))).isNone

/-! ## Refused: `EXISTS` and scalar subqueries -/

-- `EXISTS` with a select list other than `1` or `*`; `WHILE EXISTS`;
-- `EXISTS` inside a larger condition.
#guard (lowerT depq Γq (tsqlProc ["r"] [inT "@id"]
  [.If (.Exists (sel [item (tc "Qty")] ["dbo", "Acct"] none)) [] []])).isNone
#guard (lowerT depq Γq (tsqlProc ["r"] [inT "@id"]
  [.While (existsT ["dbo", "Acct"] none) [del ["dbo", "Acct"] none]])).isNone
#guard (lowerT depq Γq (tsqlProc ["r"] [inT "@id"]
  [.If (.Bin "AND" (existsT ["dbo", "Acct"] none) (.Bin ">" (tv "@id") (num 0))) [] []])).isNone
-- A scalar subquery that is part of a value, or without the key fixed.
#guard (lowerT depq Γq (tsqlProc ["r"] [inT "@id", ioT "@q"]
  [set (tv "@q") (.Bin "+" (.Subquery (sel [item (tc "Qty")] ["dbo", "Acct"] (some keyT))) (num 1))])).isNone
#guard (lowerT depq Γq (tsqlProc ["r"] [inT "@id", ioT "@q"]
  [set (tv "@q") (.Subquery (sel [item (tc "Qty")] ["dbo", "Acct"] (some (.Bin "=" (tc "Qty") (tv "@id")))))])).isNone

/-! ## Refused: the shapes differ

`SELECT @q = Qty ... WHERE Id = @id` keeps `@q` when no row matches; the
scalar subquery `p_q := (SELECT ...)` sets NULL (eval/queries Q1 and Q5).
The two lower to different picks, and the interpreter shows the
difference. -/

def keepT : Ast.Routine := selAcct (some keyT)
def scalarP : Ast.Routine :=
  pgProc ["r"] [inP "p_id", ioP "p_q"] [] [
    set (pv "p_q") (.Subquery (sel [item (pv "qty")] ["acct"] (some keyP)))]
#guard reasonOf (checkQ keepT scalarP readCert) == "the lowered routines differ beyond the renaming"
-- `look(9, @q = 5)`: no row has key 9.
#guard runD (lowerT depq Γq keepT) [I 9, I 5] db0 == some (.done [I 5, I 0] none 0 db0 [])
#guard runD (lowerP Γq scalarP) [I 9, I 5] db0 == some (.done [N, I 0] none 0 db0 [])
-- And `EXISTS` against `NOT EXISTS` with the branches kept.
#guard reasonOf (checkQ
  (tsqlProc ["r"] [inT "@id"] [.If (existsT ["dbo", "Acct"] none) [set (tv "@id") (num 1)] []])
  (pgProc ["r"] [inP "p_id"] [] [.If (.Unary "NOT" (existsT ["acct"] none)) [set (pv "p_id") (num 1)] []])
  ⟨[("@id", "p_id"), (statusVar, statusVar)]⟩) ==
  "the lowered routines differ beyond the renaming"

/-! ## The core as an oracle

Runs of the lowered `look` on C3a's rows (`id`, `qty`, `total`): (1, 10,
0), (2, NULL, 5), (3, 7, 1). The outputs are `@q`, `@t`, `@e` and the
status. -/

def lT := lowerT depq Γq lookT
def lP := lowerP Γq lookP

/- `look(1)`: row 1 gives `@q = 10`, `@t = 0`; `Qty > 1` holds for rows
1 and 3, so `@e` is 1, then 3; `@t` is 0 again; two rows have `Qty > 0`,
so `@e = 2`. -/
#guard runD lT [I 1, N, N, N] db0 == some (.done [I 10, I 0, I 2, I 0] none 0 db0 [])
#guard runD lP [I 1, N, N, N] db0 == runD lT [I 1, N, N, N] db0
/- `look(2)`: row 2's `Qty` is NULL, so `@q` becomes NULL; `@t = 5`. -/
#guard runD lT [I 2, I 7, N, N] db0 == some (.done [N, I 5, I 2, I 0] none 0 db0 [])
#guard runD lP [I 2, I 7, N, N] db0 == runD lT [I 2, I 7, N, N] db0
/- `look(9)`: no row has key 9: `@q` keeps 4; the scalar subquery makes
`@t` NULL; `Qty > 9` holds for row 1 (`@e = 1`, then 3). -/
#guard runD lT [I 9, I 4, I 4, N] db0 == some (.done [I 4, N, I 2, I 0] none 0 db0 [])
#guard runD lP [I 9, I 4, I 4, N] db0 == runD lT [I 9, I 4, I 4, N] db0
/- `look(NULL)`: `Id = NULL` matches nothing, `Qty > NULL` nothing:
`EXISTS` is FALSE, not UNKNOWN, so `NOT EXISTS` takes its branch
(`@e = 2`), before the count sets it to 2 anyway. -/
#guard runD lT [N, I 4, I 4, I 4] db0 == some (.done [I 4, N, I 2, I 0] none 0 db0 [])
#guard runD lP [N, I 4, I 4, I 4] db0 == runD lT [N, I 4, I 4, I 4] db0
/- An empty table: the count is 0, `EXISTS` FALSE. -/
#guard runD lT [I 1, I 4, I 4, I 4] [("acct", [])] ==
  some (.done [I 4, N, I 0, I 0] none 0 [("acct", [])] [])
/- `EXISTS` alone, on the `NOT EXISTS` branch values: with no row `@e`
would be 2; the count then sets 0. Seen without the count: -/
#guard runD (lowerT depq Γq (lookT (lookBodyT.take 3))) [I 1, I 4, I 4, I 4] [("acct", [])] ==
  some (.done [I 4, I 4, I 2, I 0] none 0 [("acct", [])] [])
/- A database that breaks the key (two rows with key 1): no engine can
be in this state (the primary key, `ASM-TABLES-AS-DECLARED`); the core
takes the last row in list order, the same on both sides, which is no
claim about either engine (eval/queries Q2). -/
#guard runD lT [I 1, N, N, N] [("acct", [[I 1, I 10, I 0], [I 1, I 11, I 1]])] ==
  some (.done [I 11, I 1, I 2, I 0] none 0 [("acct", [[I 1, I 10, I 0], [I 1, I 11, I 1]])] [])

end Sqldiff.Mirror.ExamplesC3b
