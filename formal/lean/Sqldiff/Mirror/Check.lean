/-
The C1 mirror check (docs/verified-conversion.md, "certifying converter"):
a T-SQL routine `a` (the reference) and its PL/pgSQL conversion `b` are
confirmed when `lowerP b` is `lowerT a` with its variables renamed by the
certificate, and the renaming is injective on `a`'s variables. Lean does
not search: the certificate says the renaming, the checker verifies it.

The certificate is untrusted: a wrong one makes the answer unconfirmed,
never wrong. What is trusted: the lowering (it is the spec), the
deployment facts `dep` and the target catalog `Γ`, and the parsers that
built `a` and `b`.

What a confirmation does not say yet: operators are uninterpreted in C1,
so it holds for every meaning of each operator name *provided the name
means the same in both dialects* (`Sqldiff.Mirror.lowerExpr`). It is a
proof about the control structure, variables, parameters, calls and the
return status, not yet about real T-SQL against real PostgreSQL
expressions; that is C2.
-/
import Sqldiff.Core.Rename
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

/-- The check. The first three tests on the certificate only make a bad
certificate fail with a precise reason; soundness rests on the last test
alone: the renaming is injective on the source's variables, and the
lowered target is exactly the renamed lowered source. -/
def check (dep : Deployment) (Γ : Catalog) (cert : Cert) (a b : Ast.Routine) : Answer :=
  match lowerT dep a, lowerP Γ b with
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
      .confirmed "Sqldiff.Mirror.check_sound"
    else
      .unconfirmed "the lowered routines differ beyond the renaming"

/-- A confirmed answer means: both routines are inside the C1 spec, and
the PL/pgSQL routine refines the T-SQL one, for every meaning of the
operators (the same on both sides) and every world. -/
theorem check_sound (dep : Deployment) (Γ : Catalog) (cert : Cert) (a b : Ast.Routine)
    (t : String) (h : check dep Γ cert a b = .confirmed t) :
    ∃ ca cb, lowerT dep a = some ca ∧ lowerP Γ b = some cb ∧
      ∀ {V W : Type} (es : ExprSem V) (wm : WorldModel V W), Refines es wm ca cb := by
  unfold check at h
  split at h
  · cases h
  · cases h
  · next ca cb ha hb =>
    refine ⟨ca, cb, ha, hb, ?_⟩
    intro V W es wm
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

end Sqldiff.Mirror
