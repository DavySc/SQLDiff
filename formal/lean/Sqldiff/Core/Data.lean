/-
Facts about the data statements of C3a (`insertRows`, `updateRows`,
`deleteRows`):

* congruence: two evaluators that agree on every expression of a
  statement (after a map `g` on one side) give the same table, or the same
  error. The renaming simulation (`Sqldiff.Core.Rename`, `g` the renaming)
  and the operator independence (`Sqldiff.Core.OpFree`, `g` the identity)
  both use it;
* statement atomicity: a data statement that fails leaves the state as it
  was (`exec_error_atomic`), which is how the core models T-SQL's and
  PostgreSQL's statement-level rollback.
-/
import Sqldiff.Core.Semantics

namespace Sqldiff.Core

variable {U W : Type}

section
variable {ev ev' : Store U → Expr → Res U} {g : Expr → Expr}

theorem evalVals_map (ρ : Store U) :
    ∀ es : List Expr, (∀ e ∈ es, ∀ ρ, ev' ρ (g e) = ev ρ e) →
      evalVals ev' ρ (es.map g) = evalVals ev ρ es
  | [], _ => rfl
  | e :: es, h => by
    simp only [List.map_cons, evalVals]
    rw [h e (by simp) ρ, evalVals_map ρ es (fun e' he' => h e' (by simp [he']))]

theorem keeps_map (ρ : Store U) :
    ∀ c : Option Expr, (∀ e, c = some e → ∀ ρ, ev' ρ (g e) = ev ρ e) →
      keeps ev' ρ (c.map g) = keeps ev ρ c
  | none, _ => rfl
  | some e, h => by
    simp only [Option.map_some, keeps]
    rw [h e rfl ρ]

theorem insertRows_map (t : TableDef) (cs : List Col) :
    ∀ (rows : List (List Expr)) (acc : List (Row U)),
      (∀ es ∈ rows, ∀ e ∈ es, ∀ ρ, ev' ρ (g e) = ev ρ e) →
      insertRows ev' t cs (rows.map (·.map g)) acc = insertRows ev t cs rows acc
  | [], _, _ => rfl
  | es :: ess, acc, h => by
    simp only [List.map_cons, insertRows]
    rw [evalVals_map nullStore es (h es (by simp))]
    have ih := fun acc => insertRows_map t cs ess acc (fun es' hes' => h es' (by simp [hes']))
    cases evalVals ev nullStore es with
    | error k => rfl
    | ok vs =>
      dsimp only
      split
      · rfl
      · split
        · exact ih _
        · split
          · rfl
          · split
            · rfl
            · exact ih _

theorem updRow_map (t : TableDef) (sets : List (Col × Expr)) (r : Row U)
    (h : ∀ p ∈ sets, ∀ ρ, ev' ρ (g p.2) = ev ρ p.2) :
    updRow ev' t (sets.map fun p => (p.1, g p.2)) r = updRow ev t sets r := by
  have h2 : (sets.map fun p => (p.1, g p.2)).map (·.2) = (sets.map (·.2)).map g := by
    simp [List.map_map, Function.comp_def]
  have h1 : (sets.map fun p => (p.1, g p.2)).map (·.1) = sets.map (·.1) := by
    simp [List.map_map, Function.comp_def]
  simp only [updRow, h1, h2]
  rw [evalVals_map _ _ (fun e he ρ => by
    obtain ⟨p, hp, rfl⟩ := List.mem_map.mp he
    exact h p hp ρ)]

theorem updateRows_map (t : TableDef) (sets : List (Col × Expr)) (c : Option Expr)
    (hs : ∀ p ∈ sets, ∀ ρ, ev' ρ (g p.2) = ev ρ p.2)
    (hc : ∀ e, c = some e → ∀ ρ, ev' ρ (g e) = ev ρ e) :
    ∀ rs : List (Row U),
      updateRows ev' t (sets.map fun p => (p.1, g p.2)) (c.map g) rs = updateRows ev t sets c rs
  | [] => rfl
  | r :: rs => by
    simp only [updateRows]
    rw [keeps_map _ c hc, updRow_map t sets r hs, updateRows_map t sets c hs hc rs]

theorem deleteRows_map (t : TableDef) (c : Option Expr)
    (hc : ∀ e, c = some e → ∀ ρ, ev' ρ (g e) = ev ρ e) :
    ∀ rs : List (Row U), deleteRows ev' t (c.map g) rs = deleteRows ev t c rs
  | [] => rfl
  | r :: rs => by
    simp only [deleteRows]
    rw [keeps_map _ c hc, deleteRows_map t c hc rs]

end

/-! ## Statement atomicity -/

/-- A data statement. -/
def Stmt.isData : Stmt → Bool
  | .insert .. | .update .. | .delete .. => true
  | _ => false

/-- A data statement that fails leaves the state, and with it the
database, exactly as before the statement: T-SQL's and PostgreSQL's
statement-level rollback (`Sqldiff.Mirror`, "Statement atomicity"). The
same holds for every other statement that fails by itself (an assignment,
a condition, a `RETURN` value, a call's arguments): the interpreter keeps
the state from before it. -/
theorem exec_error_atomic (es : ExprSem U) (wm : WorldModel U W) (n : Nat) (s : Stmt)
    (st st' : State U W) (k : ErrKind) (hs : s.isData = true)
    (h : exec es wm n s st = .error k st') : st' = st := by
  cases n with
  | zero => simp [exec] at h
  | succ n =>
    cases s <;> simp [Stmt.isData] at hs <;> simp only [exec] at h <;>
      (split at h
       · cases h; rfl
       · cases h)

end Sqldiff.Core
