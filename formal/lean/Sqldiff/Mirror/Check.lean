/-
The C1 mirror check (docs/verified-conversion.md, "certifying converter"):
a T-SQL routine `a` (the reference) and its PL/pgSQL conversion `b` are
confirmed when `lowerP b` is `lowerT a` with its variables renamed by the
certificate, and the renaming is injective on `a`'s variables. Lean does
not search: the certificate says the renaming, the checker verifies it.

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
import Sqldiff.Mirror.Lower
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
raise, no data statement that can violate a constraint), so that the
core's "an error ends the routine" is what T-SQL does
(`Sqldiff.Mirror.lowerT`, file header). -/
def inC2a (dep : Deployment) (Γ : Catalog) (a b : Ast.Routine) (ca cb : Core.Routine) : Bool :=
  sliceT dep Γ a && sliceP Γ b && ca.opFree && cb.opFree && ca.dmlSafe && cb.dmlSafe &&
    (abortsT a || !ca.mayFail)

/-- The facts about the T-SQL session a C2a confirmation still rests on,
for the Rust side to record in place of `ASM-C1-OPERATORS`. Both are
settings the routine's text does not fix:

* `ASM-TSQL-ARITH-ERRORS`, when an expression can raise: integer overflow
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
  statement or a call) may have happened (`Core.Routine.errAfterWrite`):
  the caller does not rely on the database, or on what callees did, after
  a call that failed. A failed observation does not include them
  (`Core.Obs.failed`) because the dialects differ there: T-SQL under
  `XACT_ABORT ON` rolls back the open transaction, and with none open the
  statements before the failing one stay committed ("SET XACT_ABORT
  (Transact-SQL)"; autocommit, "Transactions (Transact-SQL)"), while
  PostgreSQL rolls back the whole `CALL` with its transaction. Phase C4
  models both. -/
def residual (ca : Core.Routine) : List String :=
  (if ca.mayRaise then ["ASM-TSQL-ARITH-ERRORS"] else []) ++
    (if ca.hasEqNe then ["ASM-TSQL-ANSI-NULLS"] else []) ++
    (if ca.errAfterWrite then ["ASM-STATE-AFTER-ERROR"] else [])

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
    else if !cert.consistent then
      .unconfirmed "certificate: a T-SQL variable is given two PL/pgSQL names"
    else if !cert.injective then
      .unconfirmed "certificate: two T-SQL variables are given one PL/pgSQL name"
    else if !cert.covers ca.vars then
      .unconfirmed "certificate: a T-SQL variable has no PL/pgSQL name"
    else if InjOn cert.ren ca.vars ∧ cb = ca.rename cert.ren then
      .confirmed (if inC2a dep Γ a b ca cb then theoremC2a else theoremC1)
    else
      .unconfirmed "the lowered routines differ beyond the renaming"

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
    · next hok =>
      obtain ⟨inj, heq⟩ := hok
      rw [heq]
      exact refines_rename es wm ca inj
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
