/-
The verified gate (decision 2 in docs/verification-roadmap.md), phase 0:
the trivial fragment. Two sides in the same dialect, analysed for the same
target, whose syntax trees are identical once whitespace and comments are
left out, are the same program; a program refines itself whatever the
semantics, so the check needs no specification and no axioms.

What is trusted here: SQLDiff's parser (it built the trees),
`sqldiff_core::proof::tree_line`, which writes a tree injectively, and
`Sqldiff.Tree.decode`, which reads it back.
-/
import Sqldiff.Tree

namespace Sqldiff.Gate

open Sqldiff.Tree

/-- A program as SQLDiff's parser read it: dialect, canonical target and
the syntax tree without whitespace and comments. -/
structure Prog where
  dialect : String
  target : String
  tree : Tree
deriving DecidableEq, Repr

/-- The checker's answer. Its default, which `panic!` returns, is
"unconfirmed": no default reads as confirmed (decision 2). -/
inductive Answer where
  | confirmed (theoremName : String)
  | unconfirmed (reason : String)
deriving Repr

instance : Inhabited Answer := ⟨.unconfirmed "no answer"⟩

/-- Refinement (decision 1) within one dialect version, where the
correspondence is the identity: every observation `b` can produce in an
environment, `a` can produce in the same environment. `run p e o` says that
running `p` in environment `e` can produce observation `o`; the theorems
hold for every such semantics. -/
def Refines {Env Obs : Type} (run : Prog → Env → Obs → Prop) (a b : Prog) : Prop :=
  ∀ e o, run b e o → run a e o

theorem refines_of_eq {Env Obs : Type} (run : Prog → Env → Obs → Prop) {a b : Prog}
    (h : a = b) : Refines run a b := by
  subst h
  intro e o ho
  exact ho

/-- Why two programs are outside the fragment. -/
def reason (a b : Prog) : String :=
  if a.dialect ≠ b.dialect then "different dialects: outside the verified fragment"
  else if a.target ≠ b.target then "different targets: outside the verified fragment"
  else "the syntax trees differ: outside the verified fragment"

def check (a b : Prog) : Answer :=
  if a = b then .confirmed "Sqldiff.Gate.check_sound" else .unconfirmed (reason a b)

/-- A confirmed answer means B refines A, for every semantics. -/
theorem check_sound {Env Obs : Type} (run : Prog → Env → Obs → Prop) (a b : Prog)
    (t : String) (h : check a b = .confirmed t) : Refines run a b := by
  unfold check at h
  split at h
  · next hab => exact refines_of_eq run hab
  · cases h

end Sqldiff.Gate
