/-
Checked examples for C3a, the data statements: confirmed and refused
pairs, and runs of the core interpreter showing the table state, three
valued `WHERE`, `UPDATE`'s old values, columns an `INSERT` leaves out,
and statement atomicity on a constraint violation and on an arithmetic
error part-way through an `UPDATE`. Every `#guard` runs on every
`lake build`.
-/
import Sqldiff.Mirror.Examples

namespace Sqldiff.Mirror.ExamplesC3a

open Sqldiff.Core (ExprSem WorldModel Obs Db)
open Sqldiff.Mirror.Examples
open Sqldiff.Gate (Answer)

/-! ## The table

```sql
-- T-SQL                                   -- PostgreSQL (the converted DDL)
CREATE TABLE dbo.Acct (                    CREATE TABLE acct (
  Id int NOT NULL PRIMARY KEY,               id integer NOT NULL PRIMARY KEY,
  Qty int NULL,                              qty integer NULL,
  Total int NOT NULL);                       total integer NOT NULL);
```

The catalog names the target table `acct` and its columns as PostgreSQL
folds them, with the T-SQL spelling of each; the deployment maps
`dbo.Acct` to `acct`. -/

def acct : CatTable :=
  { name := "acct"
    cols := [⟨"id", "Id", true, true, .int .i32, none⟩, ⟨"qty", "Qty", false, false, .int .i32, none⟩,
      ⟨"total", "Total", true, false, .int .i32, none⟩] }

/-- A table without a key and two nullable columns, for `UPDATE`'s old
values. -/
def pairT : CatTable :=
  { name := "pair", cols := [⟨"a", "A", false, false, .int .i32, none⟩, ⟨"b", "B", false, false, .int .i32, none⟩] }

def Γd : Catalog := { routines := [], tables := [acct, pairT] }
def depd : Deployment :=
  { rowCounts := false, routines := [], tables := [("dbo.Acct", "acct"), ("dbo.Pair", "pair")] }

/-! ## Building blocks -/

/-- A T-SQL column and a PostgreSQL name. -/
def tc (c : String) : Ast.Expr := .Name [c]
def upd (t : List String) (sets : List (String × Ast.Expr)) (w : Option Ast.Expr) : Ast.Node :=
  .NStmt (.Update t none (sets.map fun (c, e) => .Set [c] e) [] w)
def ins (t : List String) (cs : List String) (rows : List (List Ast.Expr)) : Ast.Node :=
  .NStmt (.Insert t cs (.SrcValues rows) [] [] none)
def del (t : List String) (w : Option Ast.Expr) : Ast.Node := .NStmt (.Delete t none [] w)
def nocount : Ast.Node := .Setting "NOCOUNT" true
def bumpCert : Cert := ⟨[("@id", "p_id"), ("@n", "p_n"), (statusVar, statusVar)]⟩

def checkD (a b : Ast.Routine) (cert : Cert := bumpCert) : Answer :=
  check depd Γd cert a b

/-! ## The pair

A converted procedure body: an `UPDATE` on the key, an `INSERT ...
VALUES` inside an `IF`, a `DELETE`, and an `UPDATE` in a `WHILE` loop with
integer arithmetic.

```sql
CREATE PROCEDURE dbo.bump @id int, @n int AS
BEGIN
  SET XACT_ABORT ON;
  SET NOCOUNT ON;
  UPDATE dbo.Acct SET Qty = Qty + @n WHERE Id = @id;
  IF @n > 0
    INSERT INTO dbo.Acct (Id, Qty, Total) VALUES (@id, @n, 0);
  DELETE FROM dbo.Acct WHERE Qty IS NULL;
  WHILE @n > 0
  BEGIN
    UPDATE dbo.Acct SET Total = Total + 1 WHERE Id = @id;
    SET @n = @n - 1;
  END
END
```

```sql
CREATE PROCEDURE bump(p_id integer, p_n integer) LANGUAGE plpgsql AS $$
BEGIN
  UPDATE acct SET qty = qty + p_n WHERE id = p_id;
  IF p_n > 0 THEN
    INSERT INTO acct (id, qty, total) VALUES (p_id, p_n, 0);
  END IF;
  DELETE FROM acct WHERE qty IS NULL;
  WHILE p_n > 0 LOOP
    UPDATE acct SET Total = total + 1 WHERE ID = p_id;
    p_n := p_n - 1;
  END LOOP;
END; $$;
```

(`Total` and `ID` fold to `total` and `id`.) -/

def bumpBodyT (whereFirst : Ast.Expr := .Bin "=" (tc "Id") (tv "@id")) : List Ast.Node := [
  upd ["dbo", "Acct"] [("Qty", .Bin "+" (tc "Qty") (tv "@n"))] (some whereFirst),
  .If (.Bin ">" (tv "@n") (num 0))
    [ins ["dbo", "Acct"] ["Id", "Qty", "Total"] [[tv "@id", tv "@n", num 0]]] [],
  del ["dbo", "Acct"] (some (.IsNull (tc "Qty") false)),
  .While (.Bin ">" (tv "@n") (num 0)) [
    upd ["dbo", "Acct"] [("Total", .Bin "+" (tc "Total") (num 1))]
      (some (.Bin "=" (tc "Id") (tv "@id"))),
    set (tv "@n") (.Bin "-" (tv "@n") (num 1))]]

def bumpT (body : List Ast.Node := bumpBodyT) : Ast.Routine :=
  tsqlProc ["dbo", "bump"] [inT "@id", inT "@n"] ([xactAbort, nocount] ++ body)

def bumpBodyP (whereFirst : Ast.Expr := .Bin "=" (pv "id") (pv "p_id"))
    (insCols : List String := ["id", "qty", "total"])
    (insVals : List Ast.Expr := [pv "p_id", pv "p_n", num 0]) : List Ast.Node := [
  upd ["acct"] [("qty", .Bin "+" (pv "qty") (pv "p_n"))] (some whereFirst),
  .If (.Bin ">" (pv "p_n") (num 0)) [ins ["acct"] insCols [insVals]] [],
  del ["acct"] (some (.IsNull (pv "qty") false)),
  .While (.Bin ">" (pv "p_n") (num 0)) [
    upd ["acct"] [("Total", .Bin "+" (pv "total") (num 1))]
      (some (.Bin "=" (pv "ID") (pv "p_id"))),
    set (pv "p_n") (.Bin "-" (pv "p_n") (num 1))]]

def bumpP (body : List Ast.Node := bumpBodyP) (params : List Ast.Param := [inP "p_id", inP "p_n"]) :
    Ast.Routine :=
  pgProc ["bump"] params [] body

/-! ## Confirmed -/

#guard isC2a (checkD (bumpT) (bumpP))
-- What the answer carries: the session facts, and that the routine can
-- fail (the `INSERT` on a duplicate key, an overflow) after writing.
#guard (residualOf depd Γd bumpT) ==
  some ["ASM-TSQL-ARITH-ERRORS", "ASM-TSQL-ANSI-NULLS", "ASM-STATE-AFTER-ERROR", "ASM-TSQL-STRING-ARGS",
    "ASM-TSQL-ROWCOUNT-0"]
#guard ((lowerT depd Γd bumpT).map Core.Routine.hasData) == some true

/-- The lowered T-SQL procedure, as the core sees it: column names are the
catalog's (`Qty` is `qty`), variables keep their T-SQL names. -/
def acctDef : Core.TableDef :=
  ⟨"acct", [⟨"id", true, true⟩, ⟨"qty", false, false⟩, ⟨"total", true, false⟩]⟩
#guard (lowerT depd Γd (tsqlProc ["r"] [inT "@id"]
    [upd ["dbo", "Acct"] [("Qty", .Null)] (some (.Bin "=" (tc "Id") (tv "@id")))])).map
    (·.body) ==
  some (.seq (.assign statusVar (.lit 0))
    (.update acctDef [("qty", .null)] (some (.sc (.cmp .eq (.col "id") (.var "@id"))))))

/- Nothing can fail and nothing raises: no `XACT_ABORT` needed, no
residual. `UPDATE ... SET Qty = NULL` writes a nullable column; `DELETE`
with a `WHERE` that cannot raise. -/
def safeT : Ast.Routine :=
  tsqlProc ["r"] [inT "@id"] [
    upd ["dbo", "Acct"] [("Qty", .Null)] (some (.IsNull (tv "@id") true)),
    del ["dbo", "Acct"] (some (.IsNull (tc "Qty") false))]
def safeP : Ast.Routine :=
  pgProc ["r"] [inP "p_id"] [] [
    upd ["acct"] [("qty", .Null)] (some (.IsNull (pv "p_id") true)),
    del ["acct"] (some (.IsNull (pv "qty") false))]
#guard isC2a (checkD safeT safeP ⟨[("@id", "p_id"), (statusVar, statusVar)]⟩)
#guard (residualOf depd Γd safeT) == some ["ASM-TSQL-STRING-ARGS", "ASM-TSQL-ROWCOUNT-0"]

-- `DELETE` without `WHERE` and `UPDATE` without `WHERE`.
#guard isC2a (checkD
  (tsqlProc ["r"] [] [del ["dbo", "Acct"] none, upd ["dbo", "Pair"] [("A", tc "B")] none])
  (pgProc ["r"] [] [] [del ["acct"] none, upd ["pair"] [("a", pv "b")] none])
  ⟨[(statusVar, statusVar)]⟩)

-- In PL/pgSQL `VALUES` sees no columns, so `qty` there is the variable
-- even though the table has a column `qty`.
#guard isC2a (checkD
  (tsqlProc ["r"] [inT "@q"] [ins ["dbo", "Pair"] ["A"] [[tv "@q"]]])
  (pgProc ["r"] [inP "qty"] [] [ins ["pair"] ["a"] [[pv "qty"]]])
  ⟨[("@q", "qty"), (statusVar, statusVar)]⟩)

-- Several rows in one `INSERT`.
#guard isC2a (checkD
  (tsqlProc ["r"] [] [ins ["dbo", "Pair"] ["A", "B"] [[num 1, num 2], [num 3, .Null]]])
  (pgProc ["r"] [] [] [ins ["pair"] ["a", "b"] [[num 1, num 2], [num 3, .Null]]])
  ⟨[(statusVar, statusVar)]⟩)

/-! ## Refused -/

-- A different `WHERE`.
#guard reasonOf (checkD bumpT (bumpP (bumpBodyP (.Bin ">=" (pv "id") (pv "p_id"))))) ==
  "the lowered routines differ beyond the renaming"
-- A dropped column: `qty` is not inserted, so it would be NULL.
#guard reasonOf (checkD bumpT
    (bumpP (bumpBodyP (insCols := ["id", "total"]) (insVals := [pv "p_id", num 0])))) ==
  "the lowered routines differ beyond the renaming"
-- The columns in another order: the same rows, but not a mirror.
#guard reasonOf (checkD bumpT
    (bumpP (bumpBodyP (insCols := ["qty", "id", "total"]) (insVals := [pv "p_n", pv "p_id", num 0])))) ==
  "the lowered routines differ beyond the renaming"
/- An ambiguous PL/pgSQL name: the parameter is called `id`, like the
column, so `WHERE id = id` is an error in PostgreSQL (with the default
`variable_conflict = error`) and a different statement under either other
setting. -/
#guard reasonOf (checkD bumpT
    (bumpP (bumpBodyP (.Bin "=" (pv "id") (pv "id"))) [inP "id", inP "p_n"])
    ⟨[("@id", "id"), ("@n", "p_n"), (statusVar, statusVar)]⟩) ==
  "the PL/pgSQL routine is outside the C1 spec"
-- `found` is a variable of every PL/pgSQL routine: a column of that name
-- would be ambiguous too.
#guard resolveName (.pg Γd ["found"]) (scopeOf (.pg Γd []) { acct with cols := [⟨"found", "found", false, false, .int .i32, none⟩] } ["acct"]) "found" == none
-- The table's own name is a whole-row reference in PostgreSQL.
#guard reasonOf (checkD safeT
    (pgProc ["r"] [inP "p_id"] [] [
      upd ["acct"] [("qty", .Null)] (some (.IsNull (pv "acct") true)),
      del ["acct"] (some (.IsNull (pv "qty") false))])
    ⟨[("@id", "p_id"), (statusVar, statusVar)]⟩) ==
  "the PL/pgSQL routine is outside the C1 spec"
/- T-SQL `UPDATE ... FROM`: the source row it takes is unspecified when
several match; outside C3a. -/
#guard reasonOf (checkD
    (tsqlProc ["r"] [inT "@id"] [.NStmt (.Update ["a"] none [.Set ["Qty"] .Null]
      [.Table ["dbo", "Acct"] (some "a") []] (some (.Bin "=" (tc "Id") (tv "@id"))))])
    (pgProc ["r"] [inP "p_id"] [] [upd ["acct"] [("qty", .Null)] (some (.Bin "=" (pv "id") (pv "p_id")))])
    ⟨[("@id", "p_id"), (statusVar, statusVar)]⟩) ==
  "the T-SQL routine is outside the C1 spec"
-- An alias on `UPDATE`, a qualified column: outside the spec.
#guard (lowerT depd Γd (tsqlProc ["r"] [] [.NStmt (.Update ["dbo", "Acct"] (some "a")
  [.Set ["Qty"] .Null] [] none)])).isNone
#guard (lowerT depd Γd (tsqlProc ["r"] [] [upd ["dbo", "Acct"] [("Qty", .Name ["Acct", "Qty"])] none])).isNone
-- Row counts read by the observer: T-SQL sends one per statement, PostgreSQL none.
#guard reasonOf (check { depd with rowCounts := true } Γd bumpCert
    (tsqlProc ["r"] [inT "@id", inT "@n"] [del ["dbo", "Acct"] none])
    (pgProc ["r"] [inP "p_id", inP "p_n"] [] [del ["acct"] none])) ==
  "the T-SQL routine is outside the C1 spec"
-- With them not read, the same pair is confirmed.
#guard isC2a (check depd Γd bumpCert
    (tsqlProc ["r"] [inT "@id", inT "@n"] [del ["dbo", "Acct"] none])
    (pgProc ["r"] [inP "p_id", inP "p_n"] [] [del ["acct"] none]))
-- Setting a key column: PostgreSQL checks the key row by row.
#guard (lowerT depd Γd (tsqlProc ["r"] [] [upd ["dbo", "Acct"] [("Id", .Bin "+" (tc "Id") (num 1))] none])).isNone
#guard (lowerP Γd (pgProc ["r"] [] [] [upd ["acct"] [("id", .Bin "+" (pv "id") (num 1))] none])).isNone
-- A column spelt in another case than the catalog's T-SQL spelling: which
-- column it is depends on the collation.
#guard (lowerT depd Γd (tsqlProc ["r"] [] [upd ["dbo", "Acct"] [("QTY", .Null)] none])).isNone
-- A table the deployment does not map, or the catalog does not have.
#guard (lowerT depd Γd (tsqlProc ["r"] [] [del ["dbo", "Other"] none])).isNone
#guard (lowerT { depd with tables := [("dbo.Acct", "nope")] } Γd
  (tsqlProc ["r"] [] [del ["dbo", "Acct"] none])).isNone
#guard (lowerP Γd (pgProc ["r"] [] [] [del ["public", "acct"] none])).isNone
-- Duplicate columns, a row of the wrong width, no column list.
#guard (lowerT depd Γd (tsqlProc ["r"] [] [upd ["dbo", "Pair"] [("A", num 1), ("A", num 2)] none])).isNone
#guard (lowerT depd Γd (tsqlProc ["r"] [] [ins ["dbo", "Pair"] ["A", "B"] [[num 1]]])).isNone
#guard (lowerT depd Γd (tsqlProc ["r"] [] [ins ["dbo", "Pair"] [] [[num 1, num 2]]])).isNone
/- A value of another type than `int`, written to an `int` column: T-SQL
truncates a `decimal`, PostgreSQL rounds it (eval/casts/assign), which an
uninterpreted operator would not show. Outside the spec, even on the C1
path; a `WHERE` outside the slice is fine on the C1 path. -/
#guard (lowerT depd Γd (tsqlProc ["r"] [inT "@d" (.Ty "DECIMAL" ["5", "1"])]
  [upd ["dbo", "Pair"] [("A", tv "@d")] none])).isNone
#guard (lowerT depd Γd (tsqlProc ["r"] [] [ins ["dbo", "Pair"] ["A"] [[.Str "5"]]])).isNone
#guard isC1 (checkD
  (tsqlProc ["r"] [] [del ["dbo", "Acct"] (some (.InList (tc "Qty") [num 1, num 2] false))])
  (pgProc ["r"] [] [] [del ["acct"] (some (.InList (pv "qty") [num 1, num 2] false))])
  ⟨[(statusVar, statusVar)]⟩)
-- A data statement in a function.
#guard (lowerT depd Γd (.Routine .Function ["f"] [] (some int) .Caller none
  (.Block [] [del ["dbo", "Pair"] none, ret (some (num 1))]))).isNone
-- A T-SQL name that is not a column of the target is not a variable.
#guard (lowerT depd Γd (tsqlProc ["r"] [] [del ["dbo", "Pair"] (some (.IsNull (tc "C") false))])).isNone

/-! ## In the spec, but on the C1 theorem

These confirm under `check_sound` (and the C1 caveats on the Rust side),
not `check_sound_c2a`. -/

-- An `INSERT` that can raise (`@id + 1`) into a table that can violate a
-- constraint: which error comes first depends on the row order.
#guard isC1 (checkD
  (tsqlProc ["r"] [inT "@id"] [xactAbort, ins ["dbo", "Acct"] ["Id", "Total"] [[.Bin "+" (tv "@id") (num 1), num 0]]])
  (pgProc ["r"] [inP "p_id"] [] [ins ["acct"] ["id", "total"] [[.Bin "+" (pv "p_id") (num 1), num 0]]])
  ⟨[("@id", "p_id"), (statusVar, statusVar)]⟩)
-- The same into the table without constraints: in the slice.
#guard isC2a (checkD
  (tsqlProc ["r"] [inT "@id"] [xactAbort, ins ["dbo", "Pair"] ["A"] [[.Bin "+" (tv "@id") (num 1)]]])
  (pgProc ["r"] [inP "p_id"] [] [ins ["pair"] ["a"] [[.Bin "+" (pv "p_id") (num 1)]]])
  ⟨[("@id", "p_id"), (statusVar, statusVar)]⟩)
-- An `UPDATE` value with a part that reads no column and can raise
-- (`@id / 2`... here `@id + 1`): PostgreSQL may fold it while planning.
#guard isC1 (checkD
  (tsqlProc ["r"] [inT "@id"] [xactAbort, upd ["dbo", "Pair"] [("A", .Bin "*" (tc "A") (.Bin "+" (tv "@id") (num 1)))] none])
  (pgProc ["r"] [inP "p_id"] [] [upd ["pair"] [("a", .Bin "*" (pv "a") (.Bin "+" (pv "p_id") (num 1)))] none])
  ⟨[("@id", "p_id"), (statusVar, statusVar)]⟩)
-- A `WHERE` that can raise: which rows are tested depends on the access path.
#guard isC1 (checkD
  (tsqlProc ["r"] [inT "@id"] [xactAbort, del ["dbo", "Pair"] (some (.Bin ">" (.Bin "/" (tv "@id") (tc "A")) (num 0)))])
  (pgProc ["r"] [inP "p_id"] [] [del ["pair"] (some (.Bin ">" (.Bin "/" (pv "p_id") (pv "a")) (num 0)))])
  ⟨[("@id", "p_id"), (statusVar, statusVar)]⟩)
/- `SET Total = Total + @n` on a `NOT NULL` column: overflow on one row
and NULL (when `@n` is NULL) on another are both possible as far as the
check can see. `Total + 1` cannot be NULL, so `bump` above is in the
slice. -/
#guard isC1 (checkD
  (tsqlProc ["r"] [inT "@id", inT "@n"] [xactAbort, upd ["dbo", "Acct"] [("Total", .Bin "+" (tc "Total") (tv "@n"))] none])
  (pgProc ["r"] [inP "p_id", inP "p_n"] [] [upd ["acct"] [("total", .Bin "+" (pv "total") (pv "p_n"))] none]))
-- Without `SET XACT_ABORT ON`, a failing T-SQL statement is rolled back on
-- its own and the procedure goes on.
#guard isC1 (checkD (tsqlProc ["dbo", "bump"] [inT "@id", inT "@n"] (nocount :: bumpBodyT)) bumpP)

/-! ## The core as an oracle: table state

Runs from a database with three rows in `acct` (`id`, `qty`, `total`):
(1, 10, 0), (2, NULL, 5), (3, 7, 1). -/

def acctRows : List (List V) := [[I 1, I 10, I 0], [I 2, N, I 5], [I 3, I 7, I 1]]
def db0 : Db Unit := [("acct", acctRows), ("pair", [])]

def runD (r : Option Core.Routine) (args : List V) (db : Db Unit) : Option (Obs Unit Nat) :=
  r.map fun r => r.obs es wm args 0 db 100

def bT := lowerT depd Γd bumpT
def bP := lowerP Γd bumpP

/- `bump(1, 2)`: row 1's `qty` becomes 12; `(1, 2, 0)` is not inserted
because key 1 exists: a constraint violation, which ends both routines.
The failed observation has no database (`Core.Obs.failed`). -/
#guard runD bT [I 1, I 2] db0 == some (.failed .constraint [])
#guard runD bP [I 1, I 2] db0 == runD bT [I 1, I 2] db0
/- `bump(4, 2)`: no row has key 4; `(4, 2, 0)` is inserted; row 2 (NULL
`qty`) is deleted; the loop adds 1 to row 4's `total` twice. -/
#guard runD bT [I 4, I 2] db0 ==
  some (.done [I 0] none 0
    [("acct", [[I 1, I 10, I 0], [I 3, I 7, I 1], [I 4, I 2, I 2]]), ("pair", [])] [])
#guard runD bP [I 4, I 2] db0 == runD bT [I 4, I 2] db0
/- `bump(3, NULL)`: `qty + NULL` is NULL, so row 3's `qty` becomes NULL;
`NULL > 0` is UNKNOWN, so no `INSERT` and no loop; rows 2 and 3 now have
NULL `qty` and are deleted. -/
#guard runD bT [I 3, N] db0 ==
  some (.done [I 0] none 0 [("acct", [[I 1, I 10, I 0]]), ("pair", [])] [])
#guard runD bP [I 3, N] db0 == runD bT [I 3, N] db0
-- `bump(1, 2147483647)`: `10 + 2147483647` overflows in the first `UPDATE`.
#guard runD bT [I 1, I 2147483647] db0 == some (.failed .arithmetic [])
#guard runD bP [I 1, I 2147483647] db0 == runD bT [I 1, I 2147483647] db0

/-! ### Statement atomicity

The interpreter's state after a failed statement is the state before it
(`Core.exec_error_atomic`); the observation does not show it, so these
runs look at the outcome of the body directly. -/

/-- The end of a run: how it ended and the database in its state. -/
def endOf (r : Option Core.Routine) (args : List V) (db : Db Unit) : Option (String × Db Unit) :=
  r.bind fun r => match Core.initStore es r.params args with
    | .error _ => none
    | .ok σ => some (match Core.exec es wm 100 r.body ⟨σ, 0, db, []⟩ with
      | .normal st => ("normal", st.db)
      | .returned _ st => ("returned", st.db)
      | .error k st => (match k.cls with
        | .arithmetic => "arithmetic"
        | .constraint => "constraint"
        | .other _ => "other"
        | .user => "user", st.db)
      | .outOfFuel st => ("out of fuel", st.db))

/- An arithmetic error part-way through an `UPDATE`: `total + @n` on
every row; the second row (5 + 2147483643) overflows after the first was
computed. The statement has no effect: the table is as before it. -/
def addAllT : Ast.Routine :=
  tsqlProc ["r"] [inT "@n"] [xactAbort, upd ["dbo", "Acct"] [("Total", .Bin "+" (tc "Total") (tv "@n"))] none]
#guard endOf (lowerT depd Γd addAllT) [I 2147483643] db0 == some ("arithmetic", db0)
#guard endOf (lowerT depd Γd addAllT) [I 1] db0 ==
  some ("normal", [("acct", [[I 1, I 10, I 1], [I 2, N, I 6], [I 3, I 7, I 2]]), ("pair", [])])
/- A constraint violation in the second row of a multi-row `INSERT`: the
first row is not kept. -/
def insTwoT (k : Int) : Ast.Routine :=
  tsqlProc ["r"] [] [xactAbort,
    ins ["dbo", "Acct"] ["Id", "Total"] [[num 5, num 0], [.Num (toString k), num 0]]]
#guard endOf (lowerT depd Γd (insTwoT 3)) [] db0 == some ("constraint", db0)
-- Two new rows with one key.
#guard endOf (lowerT depd Γd (insTwoT 5)) [] db0 == some ("constraint", db0)
#guard endOf (lowerT depd Γd (insTwoT 6)) [] db0 ==
  some ("normal", [("acct", acctRows ++ [[I 5, N, I 0], [I 6, N, I 0]]), ("pair", [])])
/- A `NOT NULL` violation by `UPDATE`: `Total = Qty` on row 2, whose
`Qty` is NULL; row 1 was already done. Nothing changes. -/
#guard endOf (lowerT depd Γd (tsqlProc ["r"] [] [xactAbort,
    upd ["dbo", "Acct"] [("Total", tc "Qty")] none])) [] db0 == some ("constraint", db0)
-- Statements before the failing one keep their effect in the state (what
-- the caller sees of it is C4).
#guard endOf (lowerT depd Γd (tsqlProc ["r"] [] [xactAbort,
    del ["dbo", "Acct"] (some (.Bin "=" (tc "Id") (num 3))),
    upd ["dbo", "Acct"] [("Total", tc "Qty")] none])) [] db0 ==
  some ("constraint", [("acct", [[I 1, I 10, I 0], [I 2, N, I 5]]), ("pair", [])])

/-! ### `UPDATE` reads old values; `INSERT` leaves other columns NULL -/

-- `SET A = B, B = A` swaps.
#guard runD (lowerT depd Γd (tsqlProc ["r"] [] [upd ["dbo", "Pair"] [("A", tc "B"), ("B", tc "A")] none]))
    [] [("pair", [[I 1, I 2], [N, I 3]])] ==
  some (.done [I 0] none 0 [("pair", [[I 2, I 1], [I 3, N]])] [])
-- `INSERT INTO dbo.Pair (B) VALUES (4)`: `A` is NULL. A table the database
-- does not list has no rows.
#guard runD (lowerT depd Γd (tsqlProc ["r"] [] [ins ["dbo", "Pair"] ["B"] [[num 4]]])) [] [] ==
  some (.done [I 0] none 0 [("pair", [[N, I 4]])] [])
-- `WHERE` keeps TRUE rows only: neither `Qty > 8` nor `NOT (Qty > 8)` matches row 2.
#guard runD (lowerT depd Γd (tsqlProc ["r"] [] [
    del ["dbo", "Acct"] (some (.Bin ">" (tc "Qty") (num 8))),
    del ["dbo", "Acct"] (some (.Unary "NOT" (.Bin ">" (tc "Qty") (num 8))))])) [] db0 ==
  some (.done [I 0] none 0 [("acct", [[I 2, N, I 5]]), ("pair", [])] [])

end Sqldiff.Mirror.ExamplesC3a
