/-
The C1 mirror check (docs/verified-conversion.md, "certifying converter"):
a T-SQL routine `a` (the reference) and its PL/pgSQL conversion `b` are
confirmed when `lowerP b` is `lowerT a` settled (C4,
`Sqldiff.Core.Stmt.settleTop`: `undo` on every handler whose body cannot
fail after a write, and on every handler outside other handled bodies
that always fails without touching the database) with its variables
renamed by the certificate, and the renaming is injective on `a`'s
variables. Lean does not search: the certificate says the renaming, the
checker verifies it; settling is a function of the lowered routine,
proven to change no observation (`Sqldiff.Core.Routine.obs_settleTop`:
outcomes agree except in the database of a failed run, which a failed
observation does not carry).

The certificate is untrusted: a wrong one makes the answer unconfirmed,
never wrong. What is trusted: the lowering (it is the spec), the
deployment facts `dep` and the target catalog `Γ`, and the parsers that
built `a` and `b`. With data statements (C3a), `Γ` also describes the
tables they write and `dep` maps T-SQL table names to them; the
refinement then covers the database a finished call leaves behind.

Two theorems, two answers. Every confirmation holds for every meaning of
the uninterpreted operators, *provided each operator name means the same
in both dialects* (`Sqldiff.Mirror.lowerExpr`): `check_sound`. When both
routines are in the C2a slice (`inC2a`: every expression is a slice
expression, so there is no uninterpreted operator, every data statement
is in the C3a slice, and the T-SQL routine either aborts on errors or
cannot fail), the answer names
`check_sound_c2a` instead, which adds that the confirmation does not
depend on that proviso: both lowered routines mean the same whatever the
operators mean, and their meaning is the core's scalar semantics, which
the lowering rules tie to each vendor's documentation.
-/
import Sqldiff.Core.Rename
import Sqldiff.Core.OpFree
import Sqldiff.Core.Handlers
import Sqldiff.Mirror.Lower
import Sqldiff.Mirror.Diagnose
import Sqldiff.Gate

namespace Sqldiff.Mirror

open Sqldiff.Core
open Sqldiff.Gate (Answer)

/-- Untrusted hints from the converter: each T-SQL variable name (with its
`@`; the return status as `Sqldiff.Mirror.statusVar`) and the PL/pgSQL
name it became. -/
structure Cert where
  vars : List (String × String)
deriving Repr

/-- The renaming the certificate describes; a name it does not list stays
as it is (and `check` refuses such a certificate anyway). -/
def Cert.ren (c : Cert) (x : Var) : Var :=
  (lookup c.vars x).getD x

/-- No T-SQL name is given two PL/pgSQL names. -/
def Cert.consistent (c : Cert) : Bool :=
  c.vars.all fun (x, y) => c.vars.all fun (x', y') => x != x' || y == y'

/-- No two T-SQL names are given one PL/pgSQL name. -/
def Cert.injective (c : Cert) : Bool :=
  c.vars.all fun (x, y) => c.vars.all fun (x', y') => y != y' || x == x'

/-- Every name in `xs` has an entry. -/
def Cert.covers (c : Cert) (xs : List Var) : Bool :=
  xs.all fun x => (lookup c.vars x).isSome

/-- Both routines are in the C2a slice (with the C3a data statements):
every expression is a slice expression of the right type on both sides,
neither lowering has an uninterpreted operator, every data statement is
in the C3a slice (`Core.Stmt.dmlSafe`), and the T-SQL routine aborts on
errors (`abortsT`) or has nothing that can fail (no expression that can
raise, no data statement that can violate a constraint, no call: a
callee's errors are not in the core, `Core.Stmt.failsHere`), so that the
core's "an error ends the routine" is what T-SQL does
(`Sqldiff.Mirror.lowerT`, file header). -/
def inC2a (dep : Deployment) (Γ : Catalog) (a b : Ast.Routine) (ca cb : Core.Routine) : Bool :=
  sliceT dep Γ a && sliceP Γ b && ca.opFree && cb.opFree && ca.dmlSafe && cb.dmlSafe &&
    (abortsT a || !ca.mayFail)

/-- The facts about the T-SQL session a C2a confirmation still rests on,
for the Rust side to record in place of `ASM-C1-OPERATORS`. Both are
settings the routine's text does not fix:

* `ASM-TSQL-ARITH-ERRORS`, when an expression can raise, or (C3b) the
  routine counts rows (`COUNT(*)` beyond 2^31-1): integer overflow
  and division by zero raise an error. With `ANSI_WARNINGS` and
  `ARITHABORT` both `OFF` they give NULL and a warning instead ("SET
  ARITHABORT (Transact-SQL)", "SET ANSI_WARNINGS (Transact-SQL)");
  "Setting ANSI_WARNINGS to ON implicitly sets ARITHABORT to ON when the
  database compatibility level is set to 90 or higher", and the ODBC and
  OLE DB drivers turn `ANSI_WARNINGS` on when they connect ("SET
  ANSI_WARNINGS"). Not probed.
* `ASM-TSQL-ANSI-NULLS`, when an expression uses `=` or `<>`: the routine
  was created with `ANSI_NULLS ON` ("SET ANSI_NULLS (Transact-SQL)"). In
  a `WHERE` it decides which rows a NULL matches.

And one fact about the caller:

* `ASM-STATE-AFTER-ERROR`, when something can fail after a write (a data
  statement or a call) may have happened, or the routine calls a
  procedure, which may write and then fail (`Core.Routine.errAfterWrite`):
  the caller does not rely on the database, or on what callees did, after
  a call that failed. A failed observation does not include them
  (`Core.Obs.failed`) because the dialects differ there: T-SQL under
  `XACT_ABORT ON` rolls back the open transaction, and with none open the
  statements before the failing one stay committed ("SET XACT_ABORT
  (Transact-SQL)"; autocommit, "Transactions (Transact-SQL)"), while
  PostgreSQL rolls back the whole `CALL` with its transaction. C4 models
  the state a *handled* error leaves (`Core.Stmt.tryCatch`); the state
  after an error that leaves the routine stays unobserved.

And, for strings (C2b, `Sqldiff.Mirror`, "The C2b slice: strings"):

* `ASM-CONCAT-NULL`, when an expression concatenates strings and the
  T-SQL routine does not start with `SET CONCAT_NULL_YIELDS_NULL ON`
  (`concatNullOn`): T-SQL sessions run with `CONCAT_NULL_YIELDS_NULL ON`,
  so `+` with a NULL string is NULL, as `||` is.
* `ASM-BINARY-COLLATION`, when an expression compares strings: the
  catalog fact `binaryCollation` the request asserted (the T-SQL
  database's default collation is `_BIN`/`_BIN2`, every collation a
  PostgreSQL comparison can take is deterministic).
* `ASM-TSQL-LEN-BMP`, when an expression uses `LEN`: no character outside
  the Basic Multilingual Plane reaches it, or the T-SQL collation is
  supplementary-character aware (`_SC`, `_140_`, `_UTF8`), so that `LEN`
  counts characters as `length` does.
* `ASM-TSQL-STRING-ARGS`, when the T-SQL routine has a string parameter:
  every string argument a caller passes fits the parameter's declared
  type (at most `n` bytes for `varchar(n)`, `n` byte-pairs for
  `nvarchar(n)`; for `varchar` only characters of the database's code
  page), so T-SQL does not truncate or replace it on entry and the
  PostgreSQL routine sees the same value.

And, for dates (C2c, `Sqldiff.Mirror`, "The C2c slice: typed values"):

* `ASM-DATEADD-RANGE`, when an expression adds to a date or timestamp
  (`DATEADD`, the converter's `+ n` and `+ make_interval(...)`): the
  result stays within 0001-01-01 and 9999-12-31, where T-SQL raises 517
  and PostgreSQL returns the value (eval/values V6); the core holds every
  day number.
* `ASM-DATEADD-YEAR-END`, when an expression adds years: February 29
  plus a year is February 28, which both engines do (eval/values V6) but
  Microsoft's page states for months only.

And, for errors and transactions (C4, `Sqldiff.Mirror`, "The C4 rules"):

* `ASM-CATCH-CORE-ERRORS`, when the routine has a handler: inside a
  handled block no error is raised that the core does not model
  (deadlocks, timeouts, cancellation, resource limits, name resolution),
  or such an error is caught, or not, alike by T-SQL `CATCH` and
  PL/pgSQL `WHEN OTHERS`.
* `ASM-NO-CALLER-TXN`, when an error can reach a handler, or the routine
  has transaction statements (also those of the rollback-and-rethrow
  idiom, which lowers to its body), or its conversion commits (`bTxn`:
  the PL/pgSQL routine has a `COMMIT`, which lowers to nothing): the caller holds no open transaction
  (T-SQL: `@@TRANCOUNT` 0 on entry and `IMPLICIT_TRANSACTIONS OFF`;
  PostgreSQL: a top-level `CALL` outside a transaction block). Otherwise
  a handled error dooms the caller's T-SQL transaction under `XACT_ABORT
  ON`, and PostgreSQL's `COMMIT` fails.
* `ASM-THROW-ERRCODE`, when the routine raises an error of its own:
  callers do not tell such errors apart by the T-SQL error number (the
  PostgreSQL caller sees SQLSTATE P0001).

And, for data statements and queries (C3a, C3b), one more fact about the
caller's session:

* `ASM-TSQL-ROWCOUNT-0`, when the routine has a data statement or a query
  (`Core.Routine.hasData`, `Core.Routine.hasQuery`): callers run the
  routine with `SET ROWCOUNT 0` (the default). A caller's `SET ROWCOUNT n`
  is inherited by the routine and limits the rows its `INSERT`, `UPDATE`,
  `DELETE` and `SELECT` statements touch or read ("SET ROWCOUNT
  (Transact-SQL)", quoted from memory: it applies to those four
  statements, and a `SET` inside a procedure lasts until it returns);
  PostgreSQL has no such setting (`SET rowcount` raises 42704). Probed on
  SQL Server 2022 and PostgreSQL 18.6 (eval/session R1 to R4): under a
  caller's `SET ROWCOUNT 1` the routine's `UPDATE` and `DELETE` of three
  rows touch one, a three-row `INSERT ... VALUES` inserts one, while the
  converted procedure touches all three. The C3b shapes read at most one
  row each and showed no change under `ROWCOUNT 1` (R3); a query names
  the id all the same, since the setting applies to every `SELECT` and
  the core does not model it. Not discharged by the routine's own text:
  the reader keeps `SET ROWCOUNT 0` as no `Setting` (only the `ON`/`OFF`
  options), so a routine that starts with it is outside the spec.

`ASM-STATE-AFTER-ERROR` is decided on the settled routine
(`Core.Routine.settle`): a handled body that cannot fail after a write
leaves nothing behind when it fails, which is what `undo` says. -/
def residual (a : Ast.Routine) (ca : Core.Routine) (bTxn : Bool := false) : List String :=
  (if ca.mayRaise || ca.hasCount then ["ASM-TSQL-ARITH-ERRORS"] else []) ++
    (if ca.hasEqNe then ["ASM-TSQL-ANSI-NULLS"] else []) ++
    (if ca.settle.errAfterWrite then ["ASM-STATE-AFTER-ERROR"] else []) ++
    (if ca.hasConcat && !concatNullOn a then ["ASM-CONCAT-NULL"] else []) ++
    (if ca.hasStrEq then ["ASM-BINARY-COLLATION"] else []) ++
    (if ca.hasLen then ["ASM-TSQL-LEN-BMP"] else []) ++
    (if tsqlStringParams a then ["ASM-TSQL-STRING-ARGS"] else []) ++
    (if ca.hasDateAdd then ["ASM-DATEADD-RANGE"] else []) ++
    (if ca.hasYearAdd then ["ASM-DATEADD-YEAR-END"] else []) ++
    (if ca.hasCatch then ["ASM-CATCH-CORE-ERRORS"] else []) ++
    (if ca.catches || ca.hasCommit || tsqlTxn a || bTxn then ["ASM-NO-CALLER-TXN"] else []) ++
    (if ca.raisesUser then ["ASM-THROW-ERRCODE"] else []) ++
    (if ca.hasData || ca.hasQuery then ["ASM-TSQL-ROWCOUNT-0"] else [])

/-- The routines use C4: a handler, a raised error or transaction
control (the `"errors"` field of an answer). -/
def usesC4 (a : Ast.Routine) (ca : Core.Routine) (bTxn : Bool := false) : Bool :=
  ca.hasCatch || ca.raisesUser || ca.hasCommit || tsqlTxn a || bTxn

/-- The residual of a T-SQL routine, when it lowers. -/
def residualOf (dep : Deployment) (Γ : Catalog) (a : Ast.Routine) : Option (List String) :=
  (lowerT dep Γ a).map (residual a)

/-- (C2c) A T-SQL variable's declared type and that of the PL/pgSQL
variable the certificate gives it correspond: the same slice type (a
string type of the slice on both, any lengths, as `sigCorr`). Both
engines convert a value stored into a variable to its declared type, so
two variables of different types keep different values: T-SQL `DECLARE
@x datetime` and PL/pgSQL `v_x timestamp` given one `datetime2(6)` value
hold `.123333` and `.123456` (review of C2c, on the C1 path). A type
outside the slice corresponds to nothing, not even to the type of the
same name: the same name is not the same type (T-SQL `time` is
`time(7)`, PostgreSQL's `time(6)`; at one precision they round
differently: `time(6)` `.1234565` against `.1234566` compares 0 on SQL
Server, 1 on PostgreSQL; T-SQL `numeric` is `numeric(18,0)` and rounds
1.5 to 2, PostgreSQL's keeps 1.5; `money` keeps four decimals in T-SQL;
re-review of C2c). A pair would join only with a probe showing every
store the lowering admits to be identical, and none has one. -/
def localTyCorr (ta tp : Ast.Ty) : Bool :=
  let dt : Dialect := .tsql { rowCounts := false, routines := [] } ∅
  let dp : Dialect := .pg ∅ []
  match valTypeOf dt ta, valTypeOf dp tp with
  | some (.str ..), some (.str ..) => true
  | some x, some y => x == y
  | _, _ => false

/-- Every variable the certificate pairs, declared on both sides (the
T-SQL routine's parameters and locals, `envT`; the PL/pgSQL routine's as
declared, `envPDecl`), is declared with corresponding types; a name the
T-SQL side does not declare (the status) is not compared. -/
def varsCorr (cert : Cert) (a b : Ast.Routine) : Bool :=
  let ea := envT a
  let eb := envPDecl b
  cert.vars.all fun (x, y) => match lookup ea x, lookup eb (pgName y) with
    | some ta, some tp => localTyCorr ta tp
    -- An undeclared PL/pgSQL name is refused by the scope check and the
    -- renaming test; the status is not a T-SQL declaration.
    | _, _ => true

def theoremC1 : String := "Sqldiff.Mirror.check_sound"
def theoremC2a : String := "Sqldiff.Mirror.check_sound_c2a"

/-- The check. The first three tests on the certificate only make a bad
certificate fail with a precise reason; soundness rests on the last test
alone: the renaming is injective on the source's variables, and the
lowered target is exactly the renamed lowered source. Which theorem the
answer names says whether the pair is in the C2a slice. -/
def check (dep : Deployment) (Γ : Catalog) (cert : Cert) (a b : Ast.Routine) : Answer :=
  match lowerT dep Γ a, lowerP Γ b with
  | none, _ => .unconfirmed "the T-SQL routine is outside the C1 spec"
  | _, none => .unconfirmed "the PL/pgSQL routine is outside the C1 spec"
  | some ca, some cb =>
    if (header a).1 != (header b).1 then
      .unconfirmed "a procedure and a function"
    else if (header a).2 != (header b).2 then
      .unconfirmed "the routines run with different privileges"
    else if !sigCorr a b then
      .unconfirmed "the parameter or return types do not correspond"
    else if !varsCorr cert a b then
      .unconfirmed "the declared types of the variables do not correspond"
    else if !cert.consistent then
      .unconfirmed "certificate: a T-SQL variable is given two PL/pgSQL names"
    else if !cert.injective then
      .unconfirmed "certificate: two T-SQL variables are given one PL/pgSQL name"
    -- `existsVar` (C3b) is no name of either dialect: it stays as it is.
    else if !cert.covers (ca.vars.filter (· != existsVar)) then
      .unconfirmed "certificate: a T-SQL variable has no PL/pgSQL name"
    else if InjOn cert.ren ca.vars ∧ cb = ca.settleTop.rename cert.ren then
      .confirmed (if inC2a dep Γ a b ca cb then theoremC2a else theoremC1)
    else
      .unconfirmed "the lowered routines differ beyond the renaming"

/-- The reasons `check` gives when a routine does not lower; the answer
then carries `why` (checker 0.13.0, `Sqldiff.Mirror.Request.extra`). -/
def reasonOutsideT : String := "the T-SQL routine is outside the C1 spec"
def reasonOutsideP : String := "the PL/pgSQL routine is outside the C1 spec"

/-- Why the pair is outside the spec: the untrusted reason codes
(`Sqldiff.Mirror.whyT`, `whyP`, `Sqldiff/Mirror/Diagnose.lean`) for the
routine `check` refuses, the T-SQL one first as `check` tests them;
nothing when both lower. It is computed only for an answer whose reason
is `reasonOutsideT` or `reasonOutsideP`, never changes an answer, and
no theorem mentions it. -/
def why (dep : Deployment) (Γ : Catalog) (a b : Ast.Routine) : List String :=
  match lowerT dep Γ a, lowerP Γ b with
  | none, _ => whyT dep Γ a
  | some _, none => whyP Γ b
  | some _, some _ => []

/-- A confirmed answer means: both routines are inside the spec, and the
PL/pgSQL routine refines the T-SQL one, for every meaning of the
uninterpreted operators (the same on both sides) and every world. -/
theorem check_sound (dep : Deployment) (Γ : Catalog) (cert : Cert) (a b : Ast.Routine)
    (t : String) (h : check dep Γ cert a b = .confirmed t) :
    ∃ ca cb, lowerT dep Γ a = some ca ∧ lowerP Γ b = some cb ∧
      ∀ {U W : Type} (es : ExprSem U) (wm : WorldModel U W), Refines es wm ca cb := by
  unfold check at h
  split at h
  · cases h
  · cases h
  · next ca cb ha hb =>
    refine ⟨ca, cb, ha, hb, ?_⟩
    intro U W es wm
    split at h
    · cases h
    split at h
    · cases h
    split at h
    · cases h
    split at h
    · cases h
    split at h
    · cases h
    split at h
    · cases h
    split at h
    · cases h
    split at h
    · next hok =>
      obtain ⟨inj, heq⟩ := hok
      rw [heq]
      exact refines_settleTop_rename es wm ca inj
    · cases h

/-- A confirmed answer that names this theorem means, in addition: both
routines are in the C2a slice (`inC2a`), and what each of them does is
the same whatever the uninterpreted operators mean, so the refinement
does not rest on operator names meaning the same in both dialects. -/
theorem check_sound_c2a (dep : Deployment) (Γ : Catalog) (cert : Cert) (a b : Ast.Routine)
    (h : check dep Γ cert a b = .confirmed theoremC2a) :
    ∃ ca cb, lowerT dep Γ a = some ca ∧ lowerP Γ b = some cb ∧
      inC2a dep Γ a b ca cb = true ∧
      (∀ {U W : Type} (es es' : ExprSem U) (wm : WorldModel U W) (args : List (Val U)) (w : W)
          (db : Db U) (n : Nat),
        ca.obs es wm args w db n = ca.obs es' wm args w db n ∧
        cb.obs es wm args w db n = cb.obs es' wm args w db n) ∧
      ∀ {U W : Type} (es : ExprSem U) (wm : WorldModel U W), Refines es wm ca cb := by
  obtain ⟨ca, cb, ha, hb, href⟩ := check_sound dep Γ cert a b _ h
  have hc2a : inC2a dep Γ a b ca cb = true := by
    unfold check at h
    rw [ha, hb] at h
    simp only at h
    split at h
    · cases h
    split at h
    · cases h
    split at h
    · cases h
    split at h
    · cases h
    split at h
    · cases h
    split at h
    · cases h
    split at h
    · cases h
    split at h
    · split at h
      · assumption
      · simp [theoremC1, theoremC2a, Answer.confirmed.injEq] at h
    · cases h
  refine ⟨ca, cb, ha, hb, hc2a, ?_, href⟩
  have hf : ca.opFree = true ∧ cb.opFree = true := by
    simp only [inC2a, Bool.and_eq_true] at hc2a
    exact ⟨hc2a.1.1.1.1.2, hc2a.1.1.1.2⟩
  intro U W es es' wm args w db n
  exact ⟨Routine.obs_opFree es es' wm ca hf.1 args w db n,
    Routine.obs_opFree es es' wm cb hf.2 args w db n⟩

end Sqldiff.Mirror
