/-
Checked examples for C4, errors and transactions: the shapes the
converter writes for T-SQL `TRY`/`CATCH`, `THROW`, `BEGIN TRAN`/`COMMIT`
and the rollback-and-rethrow idiom, confirmed or refused, and runs of the
core interpreter that show why a refused pair differs (the database a
handler starts from). Every `#guard` runs on every `lake build`.
-/
import Sqldiff.Mirror.ExamplesC3a

namespace Sqldiff.Mirror.ExamplesC4

open Sqldiff.Core (ExprSem WorldModel Obs Db)
open Sqldiff.Mirror.Examples
open Sqldiff.Mirror.ExamplesC3a
open Sqldiff.Gate (Answer)

/-! ## Building blocks -/

/-- T-SQL `BEGIN TRY b END TRY BEGIN CATCH h END CATCH`. -/
def tryT (b h : List Ast.Node) : Ast.Node := .Try b [.Handler [] h]
/-- PL/pgSQL `BEGIN b EXCEPTION WHEN OTHERS THEN h END`. -/
def blockP (b h : List Ast.Node) (conds : List String := ["others"]) : Ast.Node :=
  .Try b [.Handler conds h]
/-- `THROW;` -/
def rethrowT : Ast.Node := .Throw []
/-- `THROW n, 'm', s;` -/
def throwT (n : Nat) (m : String) (s : Nat := 1) : Ast.Node := .Throw [num n, .Str m, num s]
/-- `RAISE;` -/
def reraiseP : Ast.Node := .Raise none none [] []
/-- `RAISE EXCEPTION USING MESSAGE = 'm', ERRCODE = 'P0001'`, as the converter writes it. -/
def raiseP (m : String) (code : String := "P0001") : Ast.Node :=
  .Raise (some "EXCEPTION") none [] [.RaiseOpt "MESSAGE" (.Str m), .RaiseOpt "ERRCODE" (.Str code)]
def txn (op : String) : Ast.Node := .Txn op none
/-- `IF @@TRANCOUNT > 0 ROLLBACK` -/
def rollbackIfOpen : Ast.Node := .If (.Bin ">" (.Var "@@TRANCOUNT") (num 0)) [txn "ROLLBACK"] []

def insAcct (key qty : Ast.Expr) (t : List String := ["dbo", "Acct"]) (cols := ["Id", "Qty", "Total"]) :
    Ast.Node :=
  ins t cols [[key, qty, num 0]]
def insAcctP (key qty : Ast.Expr) : Ast.Node := ins ["acct"] ["id", "qty", "total"] [[key, qty, num 0]]
/-- `UPDATE dbo.Acct SET Total = Total + 1`. -/
def bumpAllT : Ast.Node := upd ["dbo", "Acct"] [("Total", .Bin "+" (tc "Total") (num 1))] none
def bumpAllP : Ast.Node := upd ["acct"] [("total", .Bin "+" (pv "total") (num 1))] none

def idCert : Cert := ⟨[("@id", "p_id"), (statusVar, statusVar)]⟩
def abCert : Cert := ⟨[("@a", "p_a"), ("@b", "p_b"), (statusVar, statusVar)]⟩

/-! ## The converter's shapes, confirmed

### A handled arithmetic error

```sql
CREATE PROCEDURE dbo.p1 @a int, @b int OUTPUT AS
BEGIN
  SET XACT_ABORT ON;
  SET NOCOUNT ON;
  BEGIN TRY
    SET @b = @a / 0;
  END TRY
  BEGIN CATCH
    SET @b = -1;
  END CATCH
END
```

converted (as `sqldiff convert` writes it) to

```sql
CREATE OR REPLACE PROCEDURE p1(p_a integer, INOUT p_b integer) LANGUAGE plpgsql AS $$
BEGIN
  BEGIN
    p_b := p_a / 0;
  EXCEPTION WHEN OTHERS THEN
    p_b := -1;
  END;
END; $$;
```

The `TRY` body writes nothing, so PostgreSQL's rollback of the block
changes nothing: confirmed, under the handler's residuals. -/

def p1T : Ast.Routine :=
  tsqlProc ["dbo", "p1"] [inT "@a", ioT "@b"] [xactAbort, nocount,
    tryT [set (tv "@b") (.Bin "/" (tv "@a") (num 0))] [set (tv "@b") (.Num "-1")]]
def p1P : Ast.Routine :=
  pgProc ["p1"] [inP "p_a", ioP "p_b"] [] [
    blockP [set (pv "p_b") (.Bin "/" (pv "p_a") (num 0))] [set (pv "p_b") (.Num "-1")]]

#guard isC2a (check depd Γd abCert p1T p1P)
#guard residualOf depd Γd p1T ==
  some ["ASM-TSQL-ARITH-ERRORS", "ASM-TSQL-STRING-ARGS", "ASM-CATCH-CORE-ERRORS", "ASM-NO-CALLER-TXN"]
-- The handler runs: `@b` is -1 (and the status 0).
#guard runD (lowerT depd Γd p1T) [I 5, I 7] db0 == some (.done [I (-1), I 0] none 0 db0 [])
#guard runD (lowerP Γd p1P) [I 5, I 7] db0 == runD (lowerT depd Γd p1T) [I 5, I 7] db0
-- Without a handler the same division fails the call.
#guard runD (lowerT depd Γd (tsqlProc ["dbo", "p1"] [inT "@a", ioT "@b"]
    [xactAbort, set (tv "@b") (.Bin "/" (tv "@a") (num 0))])) [I 5, I 7] db0 ==
  some (.failed .arithmetic [])

/-! ### A handled constraint violation re-raised as the routine's own error

```sql
BEGIN TRY
  INSERT INTO dbo.Acct (Id, Qty, Total) VALUES (@id, 1, 0);
END TRY
BEGIN CATCH
  THROW 50001, 'dup', 1;
END CATCH
```

against

```sql
BEGIN
  INSERT INTO Acct (Id, Qty, Total) VALUES (p_id, 1, 0);
EXCEPTION WHEN OTHERS THEN
  RAISE EXCEPTION USING MESSAGE = 'dup', ERRCODE = 'P0001';
END;
```

The `INSERT` is the body's only statement: when it fails it has written
nothing (statement atomicity), so the rollback is empty. -/

def p3T (h : List Ast.Node := [throwT 50001 "dup"]) : Ast.Routine :=
  tsqlProc ["dbo", "p3"] [inT "@id"] [xactAbort, nocount, tryT [insAcct (tv "@id") (num 1)] h]
def p3P (h : List Ast.Node := [raiseP "dup"]) : Ast.Routine :=
  pgProc ["p3"] [inP "p_id"] [] [blockP [insAcctP (pv "p_id") (num 1)] h]

#guard isC2a (check depd Γd idCert p3T p3P)
#guard residualOf depd Γd p3T ==
  some ["ASM-TSQL-STRING-ARGS", "ASM-CATCH-CORE-ERRORS", "ASM-NO-CALLER-TXN", "ASM-THROW-ERRCODE",
    "ASM-TSQL-ROWCOUNT-0"]
-- Key 1 exists: the caller sees the routine's own error, of kind `user`.
#guard runD (lowerT depd Γd p3T) [I 1] db0 == some (.failed .user [])
#guard runD (lowerP Γd p3P) [I 1] db0 == some (.failed .user [])
-- Key 4 is new: the row is inserted, the handler does not run.
#guard runD (lowerP Γd p3P) [I 4] db0 == runD (lowerT depd Γd p3T) [I 4] db0
#guard runD (lowerT depd Γd p3T) [I 4] db0 ==
  some (.done [I 0] none 0 [("acct", acctRows ++ [[I 4, I 1, I 0]]), ("pair", [])] [])
-- `THROW;` against `RAISE;`: the constraint error itself reaches the caller.
#guard isC2a (check depd Γd idCert (p3T [rethrowT]) (p3P [reraiseP]))
#guard runD (lowerT depd Γd (p3T [rethrowT])) [I 1] db0 == some (.failed .constraint [])

/-! ### `CATCH THROW;` after several writes

```sql
BEGIN TRY
  INSERT INTO dbo.Acct (Id, Qty, Total) VALUES (@id, 1, 0);
  UPDATE dbo.Acct SET Total = Total + 1;
END TRY
BEGIN CATCH
  THROW;
END CATCH
```

The `UPDATE` can fail (overflow) after the `INSERT` wrote: T-SQL keeps the
row, PostgreSQL rolls it back. But the handler only re-raises, so the
caller sees a failed call either way, with the same error kind; what the
database holds after a failed call is not observed
(`ASM-STATE-AFTER-ERROR`, named in the residual). Confirmed by
`Core.Stmt.settleTop`. -/

def p4Body : List Ast.Node := [insAcct (tv "@id") (num 1), bumpAllT]
def p4BodyP : List Ast.Node := [insAcctP (pv "p_id") (num 1), bumpAllP]
def p4T (h : List Ast.Node := [rethrowT]) : Ast.Routine :=
  tsqlProc ["dbo", "p4"] [inT "@id"] [xactAbort, nocount, tryT p4Body h]
def p4P (h : List Ast.Node := [reraiseP]) : Ast.Routine :=
  pgProc ["p4"] [inP "p_id"] [] [blockP p4BodyP h]

#guard isC2a (check depd Γd idCert p4T p4P)
#guard residualOf depd Γd p4T ==
  some ["ASM-TSQL-ARITH-ERRORS", "ASM-STATE-AFTER-ERROR", "ASM-TSQL-STRING-ARGS", "ASM-CATCH-CORE-ERRORS",
    "ASM-NO-CALLER-TXN", "ASM-TSQL-ROWCOUNT-0"]
/-- Row 9's `total` is at the top of the range. -/
def dbFull : Db Unit := [("acct", [[I 9, N, I 2147483647]]), ("pair", [])]
-- Key 4: the row is inserted, then `Total + 1` overflows on row 9. Both
-- callers see the same failed call.
#guard runD (lowerT depd Γd p4T) [I 4] dbFull == some (.failed .arithmetic [])
#guard runD (lowerP Γd p4P) [I 4] dbFull == some (.failed .arithmetic [])
-- Inside, the runs differ: T-SQL's error leaves row 4 behind, PostgreSQL's
-- does not. Only a failed call can tell them apart this way.
#guard endOf (lowerT depd Γd p4T) [I 4] dbFull ==
  some ("arithmetic", [("acct", [[I 9, N, I 2147483647], [I 4, I 1, I 0]]), ("pair", [])])
#guard endOf (lowerP Γd p4P) [I 4] dbFull == some ("arithmetic", dbFull)
-- Key 9 exists: the `INSERT` fails first, nothing was written.
#guard runD (lowerT depd Γd p4T) [I 9] dbFull == some (.failed .constraint [])
#guard runD (lowerP Γd p4P) [I 9] dbFull == some (.failed .constraint [])

/-! ### The rollback-and-rethrow idiom

```sql
SET XACT_ABORT ON;
BEGIN TRY
  BEGIN TRAN;
  INSERT INTO dbo.Acct (Id, Qty, Total) VALUES (@id, 1, 0);
  UPDATE dbo.Acct SET Total = Total + 1;
  COMMIT;
END TRY
BEGIN CATCH
  IF @@TRANCOUNT > 0 ROLLBACK;
  THROW;
END CATCH
```

The converter drops the handler (`idiom.rollback-rethrow`) and the
`BEGIN TRAN`, and keeps the `COMMIT`:

```sql
INSERT INTO Acct (Id, Qty, Total) VALUES (p_id, 1, 0);
UPDATE Acct SET Total = Total + 1;
COMMIT;
``` -/

def p2T (h : List Ast.Node := [rollbackIfOpen, rethrowT]) (pre : List Ast.Node := [xactAbort, nocount])
    (body : List Ast.Node := [txn "BEGIN"] ++ p4Body ++ [txn "COMMIT"]) : Ast.Routine :=
  tsqlProc ["dbo", "p2"] [inT "@id"] (pre ++ [tryT body h])
def p2P (body : List Ast.Node := p4BodyP ++ [txn "COMMIT"]) (sec : Ast.Security := .Caller) :
    Ast.Routine :=
  .Routine .Procedure ["p2"] [inP "p_id"] none sec none (.Block [] body)

#guard isC2a (check depd Γd idCert p2T p2P)
#guard residualOf depd Γd p2T ==
  some ["ASM-TSQL-ARITH-ERRORS", "ASM-STATE-AFTER-ERROR", "ASM-TSQL-STRING-ARGS", "ASM-NO-CALLER-TXN",
    "ASM-TSQL-ROWCOUNT-0"]
-- The unguarded `ROLLBACK; THROW;`: the body opens and commits the transaction.
#guard isC2a (check depd Γd idCert (p2T [txn "ROLLBACK", rethrowT]) p2P)
-- The same without `BEGIN TRAN ... COMMIT` in either.
#guard isC2a (check depd Γd idCert (p2T (body := p4Body)) (p2P p4BodyP))

/-! ### `BEGIN TRAN ... COMMIT` around the body -/

def p5T (body : List Ast.Node := [txn "BEGIN", insAcct (tv "@id") (num 1), txn "COMMIT"])
    (pre : List Ast.Node := [xactAbort, nocount]) : Ast.Routine :=
  tsqlProc ["dbo", "p5"] [inT "@id"] (pre ++ body)
def p5P (body : List Ast.Node := [insAcctP (pv "p_id") (num 1), txn "COMMIT"]) : Ast.Routine :=
  pgProc ["p5"] [inP "p_id"] [] body

#guard isC2a (check depd Γd idCert p5T p5P)
#guard residualOf depd Γd p5T ==
  some ["ASM-TSQL-STRING-ARGS", "ASM-NO-CALLER-TXN", "ASM-TSQL-ROWCOUNT-0"]

/-! ### A `COMMIT` after each statement T-SQL commits on its own

Under `XACT_ABORT ON` and without `BEGIN TRAN`, T-SQL commits each
statement on its own; the converter writes a `COMMIT` after each
(`tsql.txn.autocommit`). The `COMMIT`s lower to nothing, and the
confirmation names `ASM-NO-CALLER-TXN` from them. -/

def autoT : Ast.Routine :=
  tsqlProc ["dbo", "p5"] [inT "@id"] [xactAbort, nocount,
    insAcct (tv "@id") (num 1), bumpAllT]
def autoP (body : List Ast.Node := [insAcctP (pv "p_id") (num 1), txn "COMMIT", bumpAllP,
    txn "COMMIT"]) : Ast.Routine :=
  pgProc ["p5"] [inP "p_id"] [] body

#guard isC2a (check depd Γd idCert autoT autoP)
-- Without the `COMMIT`s too: they change nothing a finished call shows.
#guard isC2a (check depd Γd idCert autoT (autoP [insAcctP (pv "p_id") (num 1), bumpAllP]))
-- The T-SQL routine has no transaction statement; its conversion's `COMMIT`s name the assumption.
#guard (lowerT depd Γd autoT).map (residual autoT · (pgTxn (autoP))) ==
  some ["ASM-TSQL-ARITH-ERRORS", "ASM-STATE-AFTER-ERROR", "ASM-TSQL-STRING-ARGS", "ASM-NO-CALLER-TXN",
    "ASM-TSQL-ROWCOUNT-0"]
#guard (lowerT depd Γd autoT).map (residual autoT ·) ==
  some ["ASM-TSQL-ARITH-ERRORS", "ASM-STATE-AFTER-ERROR", "ASM-TSQL-STRING-ARGS", "ASM-TSQL-ROWCOUNT-0"]
#guard (lowerT depd Γd autoT).map (usesC4 autoT · (pgTxn (autoP))) == some true
-- A `COMMIT` inside an exception block is outside the spec (P13).
#guard reasonOf (check depd Γd idCert autoT
    (autoP [blockP [insAcctP (pv "p_id") (num 1), txn "COMMIT"] [reraiseP], bumpAllP])) ==
  "the PL/pgSQL routine is outside the C1 spec"

/-! ### Nested handlers -/

def nestT : Ast.Routine :=
  tsqlProc ["dbo", "n"] [inT "@a", ioT "@b"] [xactAbort,
    tryT [tryT [set (tv "@b") (.Bin "/" (tv "@a") (num 0))] [rethrowT]] [set (tv "@b") (.Num "-2")]]
def nestP : Ast.Routine :=
  pgProc ["n"] [inP "p_a", ioP "p_b"] [] [
    blockP [blockP [set (pv "p_b") (.Bin "/" (pv "p_a") (num 0))] [reraiseP]] [set (pv "p_b") (.Num "-2")]]

#guard isC2a (check depd Γd abCert nestT nestP)
-- The inner handler re-raises, the outer one handles: `@b` is -2.
#guard runD (lowerT depd Γd nestT) [I 5, I 7] db0 == some (.done [I (-2), I 0] none 0 db0 [])

/-! ## Refused

### The rollback difference

A `TRY` whose body writes and can then fail, with a handler that goes on:
T-SQL keeps the row, PostgreSQL's block rolls it back, and the routine
then finishes normally, so the caller sees two different databases. -/

def diffT : Ast.Routine :=
  tsqlProc ["dbo", "d"] [inT "@id", ioT "@b"] [xactAbort, nocount,
    tryT [insAcct (tv "@id") (num 1), set (tv "@b") (.Bin "/" (tv "@b") (num 0))]
      [set (tv "@b") (.Num "-1")]]
def diffP : Ast.Routine :=
  pgProc ["d"] [inP "p_id", ioP "p_b"] [] [
    blockP [insAcctP (pv "p_id") (num 1), set (pv "p_b") (.Bin "/" (pv "p_b") (num 0))]
      [set (pv "p_b") (.Num "-1")]]
def diffCert : Cert := ⟨[("@id", "p_id"), ("@b", "p_b"), (statusVar, statusVar)]⟩

#guard reasonOf (check depd Γd diffCert diffT diffP) ==
  "the lowered routines differ beyond the renaming"
-- Why: the two runs, from the same database, end with different tables.
#guard runD (lowerT depd Γd diffT) [I 4, I 7] db0 ==
  some (.done [I (-1), I 0] none 0 [("acct", acctRows ++ [[I 4, I 1, I 0]]), ("pair", [])] [])
#guard runD (lowerP Γd diffP) [I 4, I 7] db0 == some (.done [I (-1), I 0] none 0 db0 [])
-- A handler that writes does not hide the difference either.
#guard reasonOf (check depd Γd idCert
    (p4T [insAcct (num 99) (num 0), rethrowT]) (p4P [insAcctP (num 99) (num 0), reraiseP])) ==
  "the lowered routines differ beyond the renaming"
-- Nor does a re-raising handler inside another `TRY`, whose handler would see the database.
#guard reasonOf (check depd Γd idCert
    (tsqlProc ["dbo", "p4"] [inT "@id"] [xactAbort, nocount, tryT [tryT p4Body [rethrowT]] [ret none]])
    (pgProc ["p4"] [inP "p_id"] [] [blockP [blockP p4BodyP [reraiseP]] [ret none]])) ==
  "the lowered routines differ beyond the renaming"

/-! ### Outside the spec -/

def outT (a : Ast.Routine) : String := reasonOf (check depd Γd idCert a (p3P))
def outP (b : Ast.Routine) : String := reasonOf (check depd Γd idCert (p3T) b)
def tOut : String := "the T-SQL routine is outside the C1 spec"
def pOut : String := "the PL/pgSQL routine is outside the C1 spec"

-- What the error is: `ERROR_NUMBER()`, `XACT_STATE()`; `SQLSTATE`, `SQLERRM`.
#guard outT (p3T [set (tv "@id") (fn "ERROR_NUMBER" [])]) == tOut
#guard outT (p3T [.If (.Bin "=" (fn "XACT_STATE" []) (.Num "-1")) [rethrowT] []]) == tOut
#guard outP (p3P [set (pv "p_id") (pv "SQLSTATE")]) == pOut
#guard outP (p3P [set (pv "p_id") (pv "sqlerrm")]) == pOut
-- `THROW` with a number below 50000, or a variable as an argument.
#guard outT (p3T [throwT 49999 "dup"]) == tOut
#guard outT (p3T [.Throw [num 50001, tv "@id", num 1]]) == tOut
-- `RAISE` at another level, with a placeholder, or with another SQLSTATE.
#guard outP (p3P [.Raise (some "NOTICE") (some "dup") [] []]) == pOut
#guard outP (p3P [.Raise (some "EXCEPTION") (some "dup %") [] []]) == pOut
#guard outP (p3P [raiseP "dup" "22012"]) == pOut
-- `ERRCODE = 'p0001'`: an SQLSTATE is upper-case, so PostgreSQL reads a
-- condition name, finds none, and raises 42704.
#guard outP (p3P [raiseP "dup" "p0001"]) == pOut
-- `RAISE EXCEPTION 'dup'` (P0001 by default) is in the spec.
#guard isC2a (check depd Γd idCert p3T (p3P [.Raise (some "EXCEPTION") (some "dup") [] []]))
-- A handler for one condition only.
#guard outP (pgProc ["p3"] [inP "p_id"] [] [blockP [insAcctP (pv "p_id") (num 1)] [raiseP "dup"]
    ["unique_violation"]]) == pOut
-- Re-raising outside a handler, or inside a `TRY` nested in one.
#guard outT (tsqlProc ["dbo", "p3"] [inT "@id"] [xactAbort, rethrowT]) == tOut
#guard outT (p3T [tryT [rethrowT] []]) == tOut
#guard outP (pgProc ["p3"] [inP "p_id"] [] [reraiseP]) == pOut
-- A call in a handled body: a callee's errors and writes are not in the core.
#guard outT (tsqlProc ["dbo", "p3"] [inT "@id"]
    [xactAbort, tryT [.Call ["dbo", "log_value"] [] none] [rethrowT]]) == tOut
-- `RAISERROR` does not even read (the Rust reader refuses it); `THROW`
-- outside `TRY` ends the routine and is in the spec.
#guard isC2a (check depd Γd idCert
  (tsqlProc ["dbo", "p3"] [inT "@id"] [xactAbort, throwT 50001 "no"])
  (pgProc ["p3"] [inP "p_id"] [] [raiseP "no"]))

-- A T-SQL function cannot have `TRY`/`CATCH` (SQL Server refuses it).
#guard reasonOf (check depd Γd abCert
    (.Routine .Function ["dbo", "f"] [inT "@a"] (some int) .Caller none (.Block [] [
      tryT [ret (some (.Bin "/" (tv "@a") (num 0)))] [ret (some (num 0))]]))
    (.Routine .Function ["f"] [inP "p_a"] (some integer) .Caller none (.Block [] [
      blockP [ret (some (.Bin "/" (pv "p_a") (num 0)))] [ret (some (num 0))]]))) == tOut

/-! ### Transactions outside the spec -/

-- Without `SET XACT_ABORT ON`.
#guard reasonOf (check depd Γd idCert (p5T (pre := [nocount])) p5P) == tOut
-- `BEGIN TRAN` without `COMMIT`, `COMMIT` without `BEGIN TRAN`, nesting.
#guard reasonOf (check depd Γd idCert (p5T [txn "BEGIN", insAcct (tv "@id") (num 1)]) p5P) == tOut
#guard reasonOf (check depd Γd idCert (p5T [insAcct (tv "@id") (num 1), txn "COMMIT"]) p5P) == tOut
#guard reasonOf (check depd Γd idCert
  (p5T [txn "BEGIN", txn "BEGIN", insAcct (tv "@id") (num 1), txn "COMMIT", txn "COMMIT"]) p5P) == tOut
-- `RETURN` or a call inside the transaction.
#guard reasonOf (check depd Γd idCert
  (p5T [txn "BEGIN", insAcct (tv "@id") (num 1), ret none, txn "COMMIT"]) p5P) == tOut
-- `ROLLBACK` outside the idiom, a named transaction.
#guard reasonOf (check depd Γd idCert
  (p5T [txn "BEGIN", insAcct (tv "@id") (num 1), txn "ROLLBACK"]) p5P) == tOut
#guard reasonOf (check depd Γd idCert
  (p5T [.Txn "BEGIN" (some "t"), insAcct (tv "@id") (num 1), .Txn "COMMIT" (some "t")]) p5P) == tOut
-- `BEGIN TRAN` inside a `TRY` whose handler goes on: the error dooms the
-- transaction (T-SQL), so the handler could not write.
#guard reasonOf (check depd Γd idCert (p2T [set (tv "@id") (num 0)]) p2P) == tOut
-- The idiom inside another `TRY`: the outer handler would see the rollback.
#guard reasonOf (check depd Γd idCert
  (tsqlProc ["dbo", "p2"] [inT "@id"] [xactAbort, nocount,
    tryT [tryT ([txn "BEGIN"] ++ p4Body ++ [txn "COMMIT"]) [rollbackIfOpen, rethrowT]] [rethrowT]])
  p2P) == tOut
-- An unguarded `ROLLBACK` when the body does not open the transaction
-- (error 3903 when nothing is open).
#guard reasonOf (check depd Γd idCert (p2T [txn "ROLLBACK", rethrowT] (body := p4Body)) (p2P p4BodyP)) ==
  tOut
/- The unguarded form around two brackets: when the division between them
fails, no transaction is open, the `ROLLBACK` raises 3903 and `THROW;`
never runs (probed on SQL Server). The body must be exactly one bracket. -/
def twoBrackets : List Ast.Node :=
  [txn "BEGIN", insAcct (tv "@id") (num 1), txn "COMMIT",
   set (tv "@id") (.Bin "/" (num 10) (tv "@id")),
   txn "BEGIN", bumpAllT, txn "COMMIT"]
def twoBracketsP : Ast.Routine :=
  p2P [insAcctP (pv "p_id") (num 1), txn "COMMIT", set (pv "p_id") (.Bin "/" (num 10) (pv "p_id")),
    bumpAllP, txn "COMMIT"]
#guard reasonOf (check depd Γd idCert (p2T [txn "ROLLBACK", rethrowT] (body := twoBrackets))
  twoBracketsP) == tOut
#guard !oneBracket twoBrackets
#guard oneBracket ([txn "BEGIN"] ++ p4Body ++ [txn "COMMIT"])
-- `IF @@TRANCOUNT > 0 ROLLBACK` is right whatever is open.
#guard isC2a (check depd Γd idCert (p2T (body := twoBrackets)) twoBracketsP)
/- The idiom inside an `IF` (or `WHILE`, or a nested block): transaction
statements are outside the spec there, the idiom's included. -/
#guard reasonOf (check depd Γd idCert
  (tsqlProc ["dbo", "p2"] [inT "@id"] [xactAbort, nocount,
    .If (.Bin ">" (tv "@id") (num 0)) [tryT ([txn "BEGIN"] ++ p4Body ++ [txn "COMMIT"])
      [rollbackIfOpen, rethrowT]] []])
  (p2P [.If (.Bin ">" (pv "p_id") (num 0)) (p4BodyP ++ [txn "COMMIT"]) []])) == tOut
#guard reasonOf (check depd Γd idCert
  (tsqlProc ["dbo", "p2"] [inT "@id"] [xactAbort, nocount,
    .NBlock (.Block [] [tryT ([txn "BEGIN"] ++ p4Body ++ [txn "COMMIT"]) [rollbackIfOpen, rethrowT]])])
  p2P) == tOut
-- PostgreSQL `COMMIT` in a `SECURITY DEFINER` procedure, inside an
-- exception block, or `ROLLBACK`.
#guard reasonOf (check depd Γd idCert p2T (p2P (sec := .Owner))) == pOut
#guard reasonOf (check depd Γd idCert p2T (p2P [blockP (p4BodyP ++ [txn "COMMIT"]) [reraiseP]])) == pOut
#guard reasonOf (check depd Γd idCert p2T (p2P (p4BodyP ++ [txn "ROLLBACK"]))) == pOut

end Sqldiff.Mirror.ExamplesC4
