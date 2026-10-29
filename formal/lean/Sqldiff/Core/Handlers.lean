/-
Handled errors (phase C4): when T-SQL's `TRY`/`CATCH` and PL/pgSQL's
`EXCEPTION` block mean the same.

The two differ in one thing (`Sqldiff.Core.exec`, `tryCatch`): when the
body fails, PL/pgSQL rolls back what the body wrote before the handler
runs (`undo`), T-SQL does not. So they agree exactly when the body cannot
fail after it may have written: then the database the error leaves is the
one the body began with, and putting it back changes nothing.
`Stmt.undoSafe` says so syntactically, and `exec_undoSafe` proves it for
every run, every world, every meaning of the operators and every starting
database (including one that breaks the catalog's constraints: the
property does not rest on the data).

`Stmt.settle` sets `undo` on every `tryCatch` whose body is `undoSafe`;
`exec_settle` proves that this changes no outcome at all, so the mirror
check compares the PL/pgSQL routine with the settled T-SQL one
(`Sqldiff.Mirror.check`), and a T-SQL `TRY` whose body can fail after a
write stays without `undo` and is refused against an `EXCEPTION` block.

"Cannot fail" must hold for every run of the core, not only for
well-typed ones: an argument can be a value of any kind, and an operator
on a value of the wrong kind fails (`typeErr`). So it is narrow
(`SExpr.total`, `Stmt.noFail`): reading a variable or a column, a literal,
`IS [NOT] NULL`, `COALESCE` and `CASE` of such, and statements built from
them; no data statement, call or comparison counts as unable to fail.
-/
import Sqldiff.Core.Rename

namespace Sqldiff.Core

variable {U W : Type}

/-! ## What cannot fail -/

/-- The expression evaluates without an error for every variable and
column value. -/
def SExpr.total : SExpr → Bool
  | .var _ | .int _ | .null | .col _ | .str _ => true
  | .isNull a _ => a.total
  | .coalesce a b => a.total && b.total
  | .cond c a b => c.total && a.total && b.total
  | _ => false

theorem SExpr.eval_total (σ ρ : Store U) :
    ∀ s : SExpr, s.total = true → ∀ k, s.eval σ ρ ≠ .err k
  | .var _, _, _ => by simp [SExpr.eval]
  | .int _, _, _ => by simp [SExpr.eval]
  | .null, _, _ => by simp [SExpr.eval]
  | .col _, _, _ => by simp [SExpr.eval]
  | .str _, _, _ => by simp [SExpr.eval]
  | .isNull a n, h, k => by
    simp only [SExpr.total] at h
    simp only [SExpr.eval]
    cases ha : a.eval σ ρ with
    | err k' => exact absurd ha (SExpr.eval_total σ ρ a h k')
    | ok v => cases v <;> simp
  | .coalesce a b, h, k => by
    simp only [SExpr.total, Bool.and_eq_true] at h
    simp only [SExpr.eval]
    cases ha : a.eval σ ρ with
    | err k' => exact absurd ha (SExpr.eval_total σ ρ a h.1 k')
    | ok v =>
      cases v
      · exact SExpr.eval_total σ ρ b h.2 k
      all_goals simp
  | .cond c a b, h, k => by
    simp only [SExpr.total, Bool.and_eq_true] at h
    simp only [SExpr.eval]
    cases hc : c.eval σ ρ with
    | err k' => exact absurd hc (SExpr.eval_total σ ρ c h.1.1 k')
    | ok v =>
      dsimp only
      split
      · exact SExpr.eval_total σ ρ a h.1.2 k
      · exact SExpr.eval_total σ ρ b h.2 k
  | .arith .., h, _ | .neg _, h, _ | .cmp .., h, _ | .and .., h, _ | .or .., h, _ | .not _, h, _
  | .concat .., h, _ | .rtrim _, h, _ | .streq .., h, _ | .len _, h, _ => by
    simp [SExpr.total] at h

def Expr.total : Expr → Bool
  | .sc s => s.total
  | .op .. => false

theorem Expr.eval_total (es : ExprSem U) (σ ρ : Store U) (e : Expr) (h : e.total = true)
    (k : ErrKind) : e.eval es σ ρ ≠ .err k := by
  cases e with
  | sc s => exact SExpr.eval_total σ ρ s h k
  | op f args => simp [Expr.total] at h

/-- The statement never ends with an error, in any run. -/
def Stmt.noFail : Stmt → Bool
  | .skip | .commit | .ret none => true
  | .assign _ e => e.total
  | .ret (some e) => e.total
  | .seq s t => s.noFail && t.noFail
  | .ite c s t => c.total && s.noFail && t.noFail
  | .while c b => c.total && b.noFail
  | .tryCatch _ _ h => h.noFail
  | .call .. | .insert .. | .update .. | .delete .. | .raise _ | .reraise | .query .. => false

theorem Stmt.noFail_bindErr (k : ErrKind) : ∀ s : Stmt, (s.bindErr k).noFail = s.noFail
  | .reraise => rfl
  | .seq s t => by
    simp [Stmt.bindErr, Stmt.noFail, Stmt.noFail_bindErr k s, Stmt.noFail_bindErr k t]
  | .ite c s t => by
    simp [Stmt.bindErr, Stmt.noFail, Stmt.noFail_bindErr k s, Stmt.noFail_bindErr k t]
  | .while c b => by simp [Stmt.bindErr, Stmt.noFail, Stmt.noFail_bindErr k b]
  | .tryCatch u b h => rfl
  | .skip | .assign .. | .ret _ | .call .. | .insert .. | .update .. | .delete .. | .raise _
  | .commit | .query .. => rfl

theorem Stmt.hasWrite_bindErr (k : ErrKind) : ∀ s : Stmt, (s.bindErr k).hasWrite = s.hasWrite
  | .reraise => rfl
  | .seq s t => by
    simp [Stmt.bindErr, Stmt.hasWrite, Stmt.hasWrite_bindErr k s, Stmt.hasWrite_bindErr k t]
  | .ite c s t => by
    simp [Stmt.bindErr, Stmt.hasWrite, Stmt.hasWrite_bindErr k s, Stmt.hasWrite_bindErr k t]
  | .while c b => by simp [Stmt.bindErr, Stmt.hasWrite, Stmt.hasWrite_bindErr k b]
  | .tryCatch u b h => by simp [Stmt.bindErr, Stmt.hasWrite, Stmt.hasWrite_bindErr k b]
  | .skip | .assign .. | .ret _ | .call .. | .insert .. | .update .. | .delete .. | .raise _
  | .commit | .query .. => rfl

/-- A statement that is not one of `seq`, `ite`, `while`, `tryCatch`. -/
def Stmt.compound : Stmt → Bool
  | .seq .. | .ite .. | .while .. | .tryCatch .. => true
  | _ => false

/-- A statement that is not compound fails, if at all, with the state it
started from: it does nothing when it fails (`exec_error_atomic` for the
data statements). -/
theorem exec_leaf_error (es : ExprSem U) (wm : WorldModel U W) (n : Nat) (s : Stmt)
    (st st' : State U W) (k : ErrKind) (hc : s.compound = false)
    (h : exec es wm (n + 1) s st = .error k st') : st' = st := by
  cases s with
  | ret e => cases e <;> simp only [exec] at h <;> (try split at h) <;> (cases h <;> rfl)
  | query t c p =>
    simp only [exec] at h
    split at h
    · cases h; rfl
    · split at h
      · cases h; rfl
      · cases h
  | _ =>
    simp [Stmt.compound] at hc <;> (try simp only [exec] at h) <;>
      first
      | (cases h <;> rfl)
      | (split at h <;> (first | (cases h <;> rfl) | simp at h))

theorem exec_noFail (es : ExprSem U) (wm : WorldModel U W) :
    ∀ (n : Nat) (s : Stmt) (st : State U W), s.noFail = true →
      ∀ k st', exec es wm n s st ≠ .error k st'
  | 0, _, _, _, _, _ => by simp [exec]
  | _ + 1, .skip, _, _, _, _ => by simp [exec]
  | _ + 1, .commit, _, _, _, _ => by simp [exec]
  | _ + 1, .ret none, _, _, _, _ => by simp [exec]
  | _ + 1, .ret (some e), st, h, k, st' => by
    simp only [Stmt.noFail] at h
    simp only [exec]
    cases he : e.eval es st.store nullStore with
    | err k' => exact absurd he (Expr.eval_total es _ _ e h k')
    | ok v => simp
  | _ + 1, .assign x e, st, h, k, st' => by
    simp only [Stmt.noFail] at h
    simp only [exec]
    cases he : e.eval es st.store nullStore with
    | err k' => exact absurd he (Expr.eval_total es _ _ e h k')
    | ok v => simp
  | n + 1, .seq s t, st, h, k, st' => by
    simp only [Stmt.noFail, Bool.and_eq_true] at h
    simp only [exec]
    cases hs : exec es wm n s st with
    | error k' st1 => exact absurd hs (exec_noFail es wm n s st h.1 k' st1)
    | normal st1 => exact exec_noFail es wm n t st1 h.2 k st'
    | returned v st1 => simp
    | outOfFuel st1 => simp
  | n + 1, .ite c s t, st, h, k, st' => by
    simp only [Stmt.noFail, Bool.and_eq_true] at h
    simp only [exec]
    cases hc : c.eval es st.store nullStore with
    | err k' => exact absurd hc (Expr.eval_total es _ _ c h.1.1 k')
    | ok v =>
      dsimp only
      split
      · exact exec_noFail es wm n s st h.1.2 k st'
      · exact exec_noFail es wm n t st h.2 k st'
  | n + 1, .while c b, st, h, k, st' => by
    have h' := h
    simp only [Stmt.noFail, Bool.and_eq_true] at h'
    simp only [exec]
    cases hc : c.eval es st.store nullStore with
    | err k' => exact absurd hc (Expr.eval_total es _ _ c h'.1 k')
    | ok v =>
      dsimp only
      split
      · cases hb : exec es wm n b st with
        | error k' st1 => exact absurd hb (exec_noFail es wm n b st h'.2 k' st1)
        | normal st1 => exact exec_noFail es wm n (.while c b) st1 h k st'
        | returned v st1 => simp
        | outOfFuel st1 => simp
      · simp
  | n + 1, .tryCatch u b hd, st, h, k, st' => by
    simp only [Stmt.noFail] at h
    simp only [exec]
    cases exec es wm n b st with
    | error k1 st1 => exact exec_noFail es wm n _ _ (by rw [Stmt.noFail_bindErr]; exact h) k st'
    | normal st1 => simp
    | returned v st1 => simp
    | outOfFuel st1 => simp
  | _ + 1, .call .., _, h, _, _ | _ + 1, .insert .., _, h, _, _ | _ + 1, .update .., _, h, _, _
  | _ + 1, .delete .., _, h, _, _ | _ + 1, .raise _, _, h, _, _ | _ + 1, .reraise, _, h, _, _
  | _ + 1, .query .., _, h, _, _ => by
    simp [Stmt.noFail] at h

/-! ## What leaves the database alone -/

/-- The state an outcome carries. -/
def Outcome.state : Outcome U W → State U W
  | .normal s | .returned _ s | .error _ s | .outOfFuel s => s

/-- A statement without data statements and calls (`hasWrite` is false)
ends, however it ends, with the database it began with. -/
theorem exec_db_of_noWrite (es : ExprSem U) (wm : WorldModel U W) :
    ∀ (n : Nat) (s : Stmt) (st : State U W), s.hasWrite = false →
      (exec es wm n s st).state.db = st.db
  | 0, _, _, _ => rfl
  | _ + 1, .skip, _, _ => rfl
  | _ + 1, .commit, _, _ => rfl
  | _ + 1, .raise _, _, _ => rfl
  | _ + 1, .reraise, _, _ => rfl
  | _ + 1, .ret none, _, _ => rfl
  | _ + 1, .ret (some e), st, _ => by
    simp only [exec]
    cases e.eval es st.store nullStore <;> rfl
  | _ + 1, .assign x e, st, _ => by
    simp only [exec]
    cases e.eval es st.store nullStore <;> rfl
  | _ + 1, .query t c p, st, _ => by
    simp only [exec]
    split
    · rfl
    · split <;> rfl
  | n + 1, .seq s t, st, h => by
    simp only [Stmt.hasWrite, Bool.or_eq_false_iff] at h
    simp only [exec]
    have hs := exec_db_of_noWrite es wm n s st h.1
    cases hx : exec es wm n s st with
    | normal st1 =>
      rw [hx] at hs
      rw [exec_db_of_noWrite es wm n t st1 h.2]
      exact hs
    | returned v st1 => rw [hx] at hs; exact hs
    | error k st1 => rw [hx] at hs; exact hs
    | outOfFuel st1 => rw [hx] at hs; exact hs
  | n + 1, .ite c s t, st, h => by
    simp only [Stmt.hasWrite, Bool.or_eq_false_iff] at h
    simp only [exec]
    cases c.eval es st.store nullStore with
    | err k => rfl
    | ok v =>
      dsimp only
      split
      · exact exec_db_of_noWrite es wm n s st h.1
      · exact exec_db_of_noWrite es wm n t st h.2
  | n + 1, .while c b, st, h => by
    have h' : b.hasWrite = false := by simpa [Stmt.hasWrite] using h
    simp only [exec]
    cases c.eval es st.store nullStore with
    | err k => rfl
    | ok v =>
      dsimp only
      split
      · have hb := exec_db_of_noWrite es wm n b st h'
        cases hx : exec es wm n b st with
        | normal st1 =>
          rw [hx] at hb
          rw [exec_db_of_noWrite es wm n (.while c b) st1 h]
          exact hb
        | returned v st1 => rw [hx] at hb; exact hb
        | error k st1 => rw [hx] at hb; exact hb
        | outOfFuel st1 => rw [hx] at hb; exact hb
      · rfl
  | n + 1, .tryCatch u b hd, st, h => by
    simp only [Stmt.hasWrite, Bool.or_eq_false_iff] at h
    simp only [exec]
    have hb := exec_db_of_noWrite es wm n b st h.1
    cases hx : exec es wm n b st with
    | error k st1 =>
      rw [hx] at hb
      rw [exec_db_of_noWrite es wm n _ _ (by rw [Stmt.hasWrite_bindErr]; exact h.2)]
      cases u
      · exact hb
      · rfl
    | normal st1 => rw [hx] at hb; exact hb
    | returned v st1 => rw [hx] at hb; exact hb
    | outOfFuel st1 => rw [hx] at hb; exact hb
  | _ + 1, .call .., _, h | _ + 1, .insert .., _, h | _ + 1, .update .., _, h
  | _ + 1, .delete .., _, h => by simp [Stmt.hasWrite] at h

/-! ## Bodies that cannot fail after a write -/

/-- `s.undoSafe w`: if `s` ends with an error, no write happened before
it. `w` says whether one may have happened before `s` began; then `s`
must not fail at all. A statement that fails by itself changes nothing
(`exec_leaf_error`), so it may fail as long as nothing was written
before it; an error the body of a `tryCatch` raises is handled, and the
handler starts from the database the body began with when `undo`. -/
def Stmt.undoSafe : Bool → Stmt → Bool
  | w, .seq s t => s.undoSafe w && t.undoSafe (w || s.hasWrite)
  | w, .ite c s t => (!w || c.total) && s.undoSafe w && t.undoSafe w
  | w, .while c b => (!(w || b.hasWrite) || c.total) && b.undoSafe (w || b.hasWrite)
  | w, .tryCatch u b h => h.undoSafe (w || (!u && b.hasWrite))
  | w, s => !w || s.noFail

theorem Stmt.undoSafe_leaf (s : Stmt) (w : Bool) (hc : s.compound = false) :
    s.undoSafe w = (!w || s.noFail) := by
  cases s <;> simp [Stmt.compound] at hc <;> rfl

theorem Stmt.undoSafe_bindErr (k : ErrKind) :
    ∀ (s : Stmt) (w : Bool), (s.bindErr k).undoSafe w = s.undoSafe w
  | .reraise, _ => rfl
  | .seq s t, w => by
    simp [Stmt.bindErr, Stmt.undoSafe, Stmt.undoSafe_bindErr k s, Stmt.undoSafe_bindErr k t,
      Stmt.hasWrite_bindErr]
  | .ite c s t, w => by
    simp [Stmt.bindErr, Stmt.undoSafe, Stmt.undoSafe_bindErr k s, Stmt.undoSafe_bindErr k t]
  | .while c b, w => by
    simp [Stmt.bindErr, Stmt.undoSafe, Stmt.undoSafe_bindErr k b, Stmt.hasWrite_bindErr]
  | .tryCatch u b h, w => by simp [Stmt.bindErr, Stmt.undoSafe, Stmt.hasWrite_bindErr]
  | .skip, _ | .assign .., _ | .ret _, _ | .call .., _ | .insert .., _ | .update .., _
  | .delete .., _ | .raise _, _ | .commit, _ | .query .., _ => rfl

theorem undoSafe_leaf_case (es : ExprSem U) (wm : WorldModel U W) (n : Nat) (s : Stmt)
    (st st' : State U W) (w : Bool) (k : ErrKind) (hc : s.compound = false)
    (hu : s.undoSafe w = true) (h : exec es wm (n + 1) s st = .error k st') :
    w = false ∧ st'.db = st.db := by
  have hst := exec_leaf_error es wm n s st st' k hc h
  subst hst
  refine ⟨?_, rfl⟩
  rw [Stmt.undoSafe_leaf s w hc] at hu
  cases w
  · rfl
  · simp only [Bool.not_true, Bool.false_or] at hu
    exact absurd h (exec_noFail es wm (n + 1) s _ hu k _)

/-- The meaning of `undoSafe`: an error leaves the database the statement
began with, and with `w` there is no error at all. -/
theorem exec_undoSafe (es : ExprSem U) (wm : WorldModel U W) :
    ∀ (n : Nat) (s : Stmt) (st : State U W) (w : Bool) (k : ErrKind) (st' : State U W),
      s.undoSafe w = true → exec es wm n s st = .error k st' → w = false ∧ st'.db = st.db
  | 0, _, _, _, _, _, _, h => by simp [exec] at h
  | n + 1, .seq s t, st, w, k, st', hu, h => by
    simp only [Stmt.undoSafe, Bool.and_eq_true] at hu
    simp only [exec] at h
    cases hs : exec es wm n s st with
    | error k1 st1 =>
      rw [hs] at h
      simp only [Outcome.error.injEq] at h
      obtain ⟨rfl, rfl⟩ := h
      exact exec_undoSafe es wm n s st w k1 st1 hu.1 hs
    | normal st1 =>
      rw [hs] at h
      obtain ⟨hw, hdb⟩ := exec_undoSafe es wm n t st1 _ k st' hu.2 h
      simp only [Bool.or_eq_false_iff] at hw
      refine ⟨hw.1, ?_⟩
      have := exec_db_of_noWrite es wm n s st hw.2
      rw [hs] at this
      rw [hdb]
      exact this
    | returned v st1 => rw [hs] at h; cases h
    | outOfFuel st1 => rw [hs] at h; cases h
  | n + 1, .ite c s t, st, w, k, st', hu, h => by
    simp only [Stmt.undoSafe, Bool.and_eq_true] at hu
    simp only [exec] at h
    cases hc : c.eval es st.store nullStore with
    | err k1 =>
      rw [hc] at h
      simp only [Outcome.error.injEq] at h
      obtain ⟨rfl, rfl⟩ := h
      refine ⟨?_, rfl⟩
      cases w
      · rfl
      · simp only [Bool.not_true, Bool.false_or] at hu
        exact absurd hc (Expr.eval_total es _ _ c hu.1.1 k1)
    | ok v =>
      rw [hc] at h
      dsimp only at h
      split at h
      · exact exec_undoSafe es wm n s st w k st' hu.1.2 h
      · exact exec_undoSafe es wm n t st w k st' hu.2 h
  | n + 1, .while c b, st, w, k, st', hu, h => by
    have hu' := hu
    simp only [Stmt.undoSafe, Bool.and_eq_true] at hu
    simp only [exec] at h
    cases hc : c.eval es st.store nullStore with
    | err k1 =>
      rw [hc] at h
      simp only [Outcome.error.injEq] at h
      obtain ⟨rfl, rfl⟩ := h
      refine ⟨?_, rfl⟩
      cases hw : (w || b.hasWrite)
      · simp only [Bool.or_eq_false_iff] at hw
        exact hw.1
      · rw [hw] at hu
        simp only [Bool.not_true, Bool.false_or] at hu
        exact absurd hc (Expr.eval_total es _ _ c hu.1 k1)
    | ok v =>
      rw [hc] at h
      dsimp only at h
      split at h
      · cases hb : exec es wm n b st with
        | error k1 st1 =>
          rw [hb] at h
          simp only [Outcome.error.injEq] at h
          obtain ⟨rfl, rfl⟩ := h
          obtain ⟨hw, hdb⟩ := exec_undoSafe es wm n b st _ k1 st1 hu.2 hb
          simp only [Bool.or_eq_false_iff] at hw
          exact ⟨hw.1, hdb⟩
        | normal st1 =>
          rw [hb] at h
          have hu2 : (Stmt.while c b).undoSafe (w || b.hasWrite) = true := by
            simp only [Stmt.undoSafe, Bool.or_assoc, Bool.or_self]
            exact hu'
          obtain ⟨hw, hdb⟩ := exec_undoSafe es wm n (.while c b) st1 _ k st' hu2 h
          simp only [Bool.or_eq_false_iff] at hw
          refine ⟨hw.1, ?_⟩
          have := exec_db_of_noWrite es wm n b st hw.2
          rw [hb] at this
          rw [hdb]
          exact this
        | returned v st1 => rw [hb] at h; cases h
        | outOfFuel st1 => rw [hb] at h; cases h
      · cases h
  | n + 1, .tryCatch u b hd, st, w, k, st', hu, h => by
    simp only [Stmt.undoSafe] at hu
    simp only [exec] at h
    cases hb : exec es wm n b st with
    | error k1 st1 =>
      rw [hb] at h
      obtain ⟨hw, hdb⟩ := exec_undoSafe es wm n _ _ _ k st'
        (by rw [Stmt.undoSafe_bindErr]; exact hu) h
      simp only [Bool.or_eq_false_iff] at hw
      refine ⟨hw.1, ?_⟩
      rw [hdb]
      cases u
      · simp only [Bool.not_false, Bool.true_and] at hw
        have := exec_db_of_noWrite es wm n b st hw.2
        rw [hb] at this
        exact this
      · rfl
    | normal st1 => rw [hb] at h; cases h
    | returned v st1 => rw [hb] at h; cases h
    | outOfFuel st1 => rw [hb] at h; cases h
  | n + 1, .skip, st, w, k, st', hu, h =>
    undoSafe_leaf_case es wm n _ st st' w k rfl hu h
  | n + 1, .assign .., st, w, k, st', hu, h =>
    undoSafe_leaf_case es wm n _ st st' w k rfl hu h
  | n + 1, .ret _, st, w, k, st', hu, h =>
    undoSafe_leaf_case es wm n _ st st' w k rfl hu h
  | n + 1, .call .., st, w, k, st', hu, h =>
    undoSafe_leaf_case es wm n _ st st' w k rfl hu h
  | n + 1, .insert .., st, w, k, st', hu, h =>
    undoSafe_leaf_case es wm n _ st st' w k rfl hu h
  | n + 1, .update .., st, w, k, st', hu, h =>
    undoSafe_leaf_case es wm n _ st st' w k rfl hu h
  | n + 1, .delete .., st, w, k, st', hu, h =>
    undoSafe_leaf_case es wm n _ st st' w k rfl hu h
  | n + 1, .raise _, st, w, k, st', hu, h =>
    undoSafe_leaf_case es wm n _ st st' w k rfl hu h
  | n + 1, .reraise, st, w, k, st', hu, h =>
    undoSafe_leaf_case es wm n _ st st' w k rfl hu h
  | n + 1, .commit, st, w, k, st', hu, h =>
    undoSafe_leaf_case es wm n _ st st' w k rfl hu h
  | n + 1, .query .., st, w, k, st', hu, h =>
    undoSafe_leaf_case es wm n _ st st' w k rfl hu h

/-! ## Settling: `undo` where it changes nothing -/

/-- Every `tryCatch` whose body is `undoSafe` gets `undo`. -/
def Stmt.settle : Stmt → Stmt
  | .seq s t => .seq s.settle t.settle
  | .ite c s t => .ite c s.settle t.settle
  | .while c b => .while c b.settle
  | .tryCatch u b h => .tryCatch (u || b.undoSafe false) b.settle h.settle
  | s => s

theorem Stmt.settle_bindErr (k : ErrKind) : ∀ s : Stmt, (s.bindErr k).settle = s.settle.bindErr k
  | .reraise => rfl
  | .seq s t => by
    simp [Stmt.bindErr, Stmt.settle, Stmt.settle_bindErr k s, Stmt.settle_bindErr k t]
  | .ite c s t => by
    simp [Stmt.bindErr, Stmt.settle, Stmt.settle_bindErr k s, Stmt.settle_bindErr k t]
  | .while c b => by simp [Stmt.bindErr, Stmt.settle, Stmt.settle_bindErr k b]
  | .tryCatch u b h => by
    simp [Stmt.bindErr, Stmt.settle, Stmt.settle_bindErr k b, Stmt.undoSafe_bindErr]
  | .skip | .assign .. | .ret _ | .call .. | .insert .. | .update .. | .delete .. | .raise _
  | .commit | .query .. => rfl

theorem Stmt.vars_settle : ∀ s : Stmt, s.settle.vars = s.vars
  | .seq s t => by simp [Stmt.settle, Stmt.vars, Stmt.vars_settle s, Stmt.vars_settle t]
  | .ite c s t => by simp [Stmt.settle, Stmt.vars, Stmt.vars_settle s, Stmt.vars_settle t]
  | .while c b => by simp [Stmt.settle, Stmt.vars, Stmt.vars_settle b]
  | .tryCatch u b h => by simp [Stmt.settle, Stmt.vars, Stmt.vars_settle b, Stmt.vars_settle h]
  | .skip | .assign .. | .ret _ | .call .. | .insert .. | .update .. | .delete .. | .raise _
  | .reraise | .commit | .query .. => rfl

/-- Settling changes no outcome: the same fuel gives the same result,
state, error and calls. -/
theorem exec_settle (es : ExprSem U) (wm : WorldModel U W) :
    ∀ (n : Nat) (s : Stmt) (st : State U W), exec es wm n s.settle st = exec es wm n s st
  | 0, _, _ => by simp [exec]
  | n + 1, .seq s t, st => by
    simp only [Stmt.settle, exec, exec_settle es wm n s st]
    cases exec es wm n s st with
    | normal st1 => exact exec_settle es wm n t st1
    | _ => rfl
  | n + 1, .ite c s t, st => by
    simp only [Stmt.settle, exec, exec_settle es wm n s st, exec_settle es wm n t st]
  | n + 1, .while c b, st => by
    have hw : ∀ st1, exec es wm n (.while c b.settle) st1 = exec es wm n (.while c b) st1 :=
      fun st1 => exec_settle es wm n (.while c b) st1
    simp only [Stmt.settle, exec, exec_settle es wm n b st, hw]
  | n + 1, .tryCatch u b hd, st => by
    simp only [Stmt.settle, exec, exec_settle es wm n b st]
    cases hb : exec es wm n b st with
    | error k st1 =>
      dsimp only
      rw [← Stmt.settle_bindErr, exec_settle es wm n (hd.bindErr k)]
      cases u
      · cases hsafe : b.undoSafe false
        · rfl
        · obtain ⟨_, hdb⟩ := exec_undoSafe es wm n b st false k st1 hsafe hb
          simp only [Bool.false_or, ite_true, Bool.false_eq_true, ite_false]
          rw [← hdb]
      · rfl
    | _ => rfl
  | _ + 1, .skip, _ => rfl
  | _ + 1, .assign .., _ => rfl
  | _ + 1, .ret _, _ => rfl
  | _ + 1, .call .., _ => rfl
  | _ + 1, .insert .., _ => rfl
  | _ + 1, .update .., _ => rfl
  | _ + 1, .delete .., _ => rfl
  | _ + 1, .raise _, _ => rfl
  | _ + 1, .reraise, _ => rfl
  | _ + 1, .commit, _ => rfl
  | _ + 1, .query .., _ => rfl

def Routine.settle (r : Routine) : Routine := { r with body := r.body.settle }

theorem Routine.vars_settle (r : Routine) : r.settle.vars = r.vars := by
  simp [Routine.settle, Routine.vars, Stmt.vars_settle]

/-- A settled routine shows the same as the routine, with the same fuel. -/
theorem Routine.obs_settle (es : ExprSem U) (wm : WorldModel U W) (r : Routine)
    (args : List (Val U)) (w : W) (db : Db U) (n : Nat) :
    r.settle.obs es wm args w db n = r.obs es wm args w db n := by
  have ho : ∀ o : Outcome U W, observe r.settle o = observe r o := fun o => by cases o <;> rfl
  unfold Routine.obs
  rw [show r.settle.params = r.params from rfl, show r.settle.body = r.body.settle from rfl]
  cases initStore es r.params args with
  | error k => rfl
  | ok σ => simp only [ho, exec_settle]

/-- What the mirror check confirms (`Sqldiff.Mirror.check`): the settled
routine, renamed injectively, refines the routine. -/
theorem refines_settle_rename (es : ExprSem U) (wm : WorldModel U W) (r : Routine)
    {f : Var → Var} (inj : InjOn f r.vars) : Refines es wm r (r.settle.rename f) := by
  have hr := refines_rename es wm r.settle (f := f) (by rw [Routine.vars_settle]; exact inj)
  intro args w db
  obtain ⟨h1, h2⟩ := hr args w db
  refine ⟨fun n => ?_, fun hd n => ?_⟩
  · obtain ⟨m, hm⟩ := h1 n
    exact ⟨m, by rw [hm, Routine.obs_settle]⟩
  · obtain ⟨w', cs, h⟩ := h2 hd n
    exact ⟨w', cs, by rw [← Routine.obs_settle]; exact h⟩

/-! ## Handlers that always fail

A handler that ends with an error on every run (`Stmt.mustFail`: it
re-raises, or raises its own error) and does not touch the database
(`hasWrite` false: no data statement, no call) shows the same whether or
not the body's writes were undone, except in the database of the error
it ends with. If that error then leaves the routine, a caller sees the
failed call, whose observation carries the error's kind and the calls
made, not the database (`Obs.failed`, `ASM-STATE-AFTER-ERROR`). So such a
`tryCatch` may get `undo` too, provided it is not inside the body of
another `tryCatch` (whose handler would see the database).
`Stmt.settleTop` does this on top of `settle`, and `exec_settleTop`
proves that the outcomes then agree up to the database of a failed (or
cut-off) run, which no observation shows (`Routine.obs_settleTop`). -/

/-- `s` has no `RETURN`. -/
def Stmt.noRet : Stmt → Bool
  | .ret _ => false
  | .seq s t | .ite _ s t | .tryCatch _ s t => s.noRet && t.noRet
  | .while _ b => b.noRet
  | _ => true

/-- Every run of `s` that finishes ends with an error. -/
def Stmt.mustFail : Stmt → Bool
  | .raise _ | .reraise => true
  | .seq s t => s.mustFail || (s.noRet && t.mustFail)
  | .ite _ s t => s.mustFail && t.mustFail
  | _ => false

theorem Stmt.noRet_bindErr (k : ErrKind) : ∀ s : Stmt, (s.bindErr k).noRet = s.noRet
  | .reraise => rfl
  | .seq s t => by simp [Stmt.bindErr, Stmt.noRet, Stmt.noRet_bindErr k s, Stmt.noRet_bindErr k t]
  | .ite c s t => by
    simp [Stmt.bindErr, Stmt.noRet, Stmt.noRet_bindErr k s, Stmt.noRet_bindErr k t]
  | .while c b => by simp [Stmt.bindErr, Stmt.noRet, Stmt.noRet_bindErr k b]
  | .tryCatch u b h => by simp [Stmt.bindErr, Stmt.noRet, Stmt.noRet_bindErr k b]
  | .skip | .assign .. | .ret _ | .call .. | .insert .. | .update .. | .delete .. | .raise _
  | .commit | .query .. => rfl

theorem Stmt.mustFail_bindErr (k : ErrKind) : ∀ s : Stmt, (s.bindErr k).mustFail = s.mustFail
  | .reraise => rfl
  | .seq s t => by
    simp [Stmt.bindErr, Stmt.mustFail, Stmt.mustFail_bindErr k s, Stmt.mustFail_bindErr k t,
      Stmt.noRet_bindErr]
  | .ite c s t => by
    simp [Stmt.bindErr, Stmt.mustFail, Stmt.mustFail_bindErr k s, Stmt.mustFail_bindErr k t]
  | .while c b => rfl
  | .tryCatch u b h => rfl
  | .skip | .assign .. | .ret _ | .call .. | .insert .. | .update .. | .delete .. | .raise _
  | .commit | .query .. => rfl

theorem Stmt.hasCatch_bindErr (k : ErrKind) : ∀ s : Stmt, (s.bindErr k).hasCatch = s.hasCatch
  | .reraise => rfl
  | .seq s t => by
    simp [Stmt.bindErr, Stmt.hasCatch, Stmt.hasCatch_bindErr k s, Stmt.hasCatch_bindErr k t]
  | .ite c s t => by
    simp [Stmt.bindErr, Stmt.hasCatch, Stmt.hasCatch_bindErr k s, Stmt.hasCatch_bindErr k t]
  | .while c b => by simp [Stmt.bindErr, Stmt.hasCatch, Stmt.hasCatch_bindErr k b]
  | .tryCatch u b h => rfl
  | .skip | .assign .. | .ret _ | .call .. | .insert .. | .update .. | .delete .. | .raise _
  | .commit | .query .. => rfl

/-- A run without `RETURN` does not return. -/
theorem exec_noRet (es : ExprSem U) (wm : WorldModel U W) :
    ∀ (n : Nat) (s : Stmt) (st : State U W), s.noRet = true →
      ∀ v st', exec es wm n s st ≠ .returned v st'
  | 0, _, _, _, _, _ => by simp [exec]
  | n + 1, .seq s t, st, h, v, st' => by
    simp only [Stmt.noRet, Bool.and_eq_true] at h
    simp only [exec]
    cases hs : exec es wm n s st with
    | normal st1 => exact exec_noRet es wm n t st1 h.2 v st'
    | returned v1 st1 => exact absurd hs (exec_noRet es wm n s st h.1 v1 st1)
    | error k st1 => simp
    | outOfFuel st1 => simp
  | n + 1, .ite c s t, st, h, v, st' => by
    simp only [Stmt.noRet, Bool.and_eq_true] at h
    simp only [exec]
    cases c.eval es st.store nullStore with
    | err k => simp
    | ok x =>
      dsimp only
      split
      · exact exec_noRet es wm n s st h.1 v st'
      · exact exec_noRet es wm n t st h.2 v st'
  | n + 1, .while c b, st, h, v, st' => by
    have h' : b.noRet = true := by simpa [Stmt.noRet] using h
    simp only [exec]
    cases c.eval es st.store nullStore with
    | err k => simp
    | ok x =>
      dsimp only
      split
      · cases hb : exec es wm n b st with
        | normal st1 => exact exec_noRet es wm n (.while c b) st1 h v st'
        | returned v1 st1 => exact absurd hb (exec_noRet es wm n b st h' v1 st1)
        | error k st1 => simp
        | outOfFuel st1 => simp
      · simp
  | n + 1, .tryCatch u b hd, st, h, v, st' => by
    simp only [Stmt.noRet, Bool.and_eq_true] at h
    simp only [exec]
    cases hb : exec es wm n b st with
    | error k st1 =>
      exact exec_noRet es wm n _ _ (by rw [Stmt.noRet_bindErr]; exact h.2) v st'
    | returned v1 st1 => exact absurd hb (exec_noRet es wm n b st h.1 v1 st1)
    | normal st1 => simp
    | outOfFuel st1 => simp
  | _ + 1, .ret _, _, h, _, _ => by simp [Stmt.noRet] at h
  | _ + 1, .skip, st, _, v, st' | _ + 1, .commit, st, _, v, st' | _ + 1, .raise _, st, _, v, st'
  | _ + 1, .reraise, st, _, v, st' => by simp [exec]
  | _ + 1, .assign x e, st, _, v, st' => by
    simp only [exec]
    cases e.eval es st.store nullStore <;> simp
  | _ + 1, .call p args, st, _, v, st' => by
    simp only [exec]
    split <;> simp
  | _ + 1, .insert t cs rows, st, _, v, st' => by
    simp only [exec]
    split <;> simp
  | _ + 1, .update t sets c, st, _, v, st' => by
    simp only [exec]
    split <;> simp
  | _ + 1, .delete t c, st, _, v, st' => by
    simp only [exec]
    split <;> simp
  | _ + 1, .query t c p, st, _, v, st' => by
    simp only [exec]
    split
    · simp
    · split <;> simp

/-- The run failed or was cut off: not `normal`, not `returned`. -/
def Outcome.failing : Outcome U W → Bool
  | .error .. | .outOfFuel _ => true
  | _ => false

theorem exec_mustFail (es : ExprSem U) (wm : WorldModel U W) :
    ∀ (n : Nat) (s : Stmt) (st : State U W), s.mustFail = true →
      (exec es wm n s st).failing = true
  | 0, _, _, _ => rfl
  | _ + 1, .raise _, _, _ => rfl
  | _ + 1, .reraise, _, _ => rfl
  | n + 1, .seq s t, st, h => by
    simp only [Stmt.mustFail, Bool.or_eq_true, Bool.and_eq_true] at h
    simp only [exec]
    cases hs : exec es wm n s st with
    | normal st1 =>
      rcases h with h | h
      · have := exec_mustFail es wm n s st h
        rw [hs] at this
        simp [Outcome.failing] at this
      · exact exec_mustFail es wm n t st1 h.2
    | returned v st1 =>
      rcases h with h | h
      · have := exec_mustFail es wm n s st h
        rw [hs] at this
        simp [Outcome.failing] at this
      · exact absurd hs (exec_noRet es wm n s st h.1 v st1)
    | error k st1 => rfl
    | outOfFuel st1 => rfl
  | n + 1, .ite c s t, st, h => by
    simp only [Stmt.mustFail, Bool.and_eq_true] at h
    simp only [exec]
    cases c.eval es st.store nullStore with
    | err k => rfl
    | ok v =>
      dsimp only
      split
      · exact exec_mustFail es wm n s st h.1
      · exact exec_mustFail es wm n t st h.2
  | _ + 1, .skip, _, h | _ + 1, .assign .., _, h | _ + 1, .ret _, _, h | _ + 1, .call .., _, h
  | _ + 1, .insert .., _, h | _ + 1, .update .., _, h | _ + 1, .delete .., _, h
  | _ + 1, .while .., _, h | _ + 1, .tryCatch .., _, h | _ + 1, .commit, _, h
  | _ + 1, .query .., _, h => by
    simp [Stmt.mustFail] at h

/-- The state with another database. -/
def State.setDb (st : State U W) (d : Db U) : State U W := { st with db := d }

def Outcome.setDb (d : Db U) : Outcome U W → Outcome U W
  | .normal s => .normal (s.setDb d)
  | .returned v s => .returned v (s.setDb d)
  | .error k s => .error k (s.setDb d)
  | .outOfFuel s => .outOfFuel (s.setDb d)

theorem Stmt.hasQuery_bindErr (k : ErrKind) : ∀ s : Stmt, (s.bindErr k).hasQuery = s.hasQuery
  | .reraise => rfl
  | .seq s t => by
    simp [Stmt.bindErr, Stmt.hasQuery, Stmt.hasQuery_bindErr k s, Stmt.hasQuery_bindErr k t]
  | .ite c s t => by
    simp [Stmt.bindErr, Stmt.hasQuery, Stmt.hasQuery_bindErr k s, Stmt.hasQuery_bindErr k t]
  | .while c b => by simp [Stmt.bindErr, Stmt.hasQuery, Stmt.hasQuery_bindErr k b]
  | .tryCatch u b h => by simp [Stmt.bindErr, Stmt.hasQuery, Stmt.hasQuery_bindErr k b]
  | .skip | .assign .. | .ret _ | .call .. | .insert .. | .update .. | .delete .. | .raise _
  | .commit | .query .. => rfl

/-- A statement that does not touch the database (no data statement, no
call, and (C3b) no query) runs the same from any database, which it
leaves as it found it. -/
theorem exec_setDb (es : ExprSem U) (wm : WorldModel U W) (d : Db U) :
    ∀ (n : Nat) (s : Stmt) (st : State U W), s.hasWrite = false → s.hasQuery = false →
      exec es wm n s (st.setDb d) = (exec es wm n s st).setDb d
  | 0, _, _, _, _ => rfl
  | _ + 1, .skip, _, _, _ => rfl
  | _ + 1, .commit, _, _, _ => rfl
  | _ + 1, .raise _, _, _, _ => rfl
  | _ + 1, .reraise, _, _, _ => rfl
  | _ + 1, .ret none, _, _, _ => rfl
  | _ + 1, .ret (some e), st, _, _ => by
    simp only [exec, State.setDb]
    cases e.eval es st.store nullStore <;> rfl
  | _ + 1, .assign x e, st, _, _ => by
    simp only [exec, State.setDb]
    cases e.eval es st.store nullStore <;> rfl
  | n + 1, .seq s t, st, h, hq => by
    simp only [Stmt.hasWrite, Bool.or_eq_false_iff] at h
    simp only [Stmt.hasQuery, Bool.or_eq_false_iff] at hq
    simp only [exec]
    rw [exec_setDb es wm d n s st h.1 hq.1]
    cases exec es wm n s st with
    | normal st1 => exact exec_setDb es wm d n t st1 h.2 hq.2
    | _ => rfl
  | n + 1, .ite c s t, st, h, hq => by
    simp only [Stmt.hasWrite, Bool.or_eq_false_iff] at h
    simp only [Stmt.hasQuery, Bool.or_eq_false_iff] at hq
    simp only [exec]
    have hc : c.eval es (st.setDb d).store nullStore = c.eval es st.store nullStore := rfl
    rw [hc]
    cases c.eval es st.store nullStore with
    | err k => rfl
    | ok v =>
      dsimp only
      split
      · exact exec_setDb es wm d n s st h.1 hq.1
      · exact exec_setDb es wm d n t st h.2 hq.2
  | n + 1, .while c b, st, h, hq => by
    have h' : b.hasWrite = false := by simpa [Stmt.hasWrite] using h
    have hq' : b.hasQuery = false := by simpa [Stmt.hasQuery] using hq
    simp only [exec]
    have hc : c.eval es (st.setDb d).store nullStore = c.eval es st.store nullStore := rfl
    rw [hc]
    cases c.eval es st.store nullStore with
    | err k => rfl
    | ok v =>
      dsimp only
      split
      · rw [exec_setDb es wm d n b st h' hq']
        cases exec es wm n b st with
        | normal st1 => exact exec_setDb es wm d n (.while c b) st1 h hq
        | _ => rfl
      · rfl
  | n + 1, .tryCatch u b hd, st, h, hq => by
    simp only [Stmt.hasWrite, Bool.or_eq_false_iff] at h
    simp only [Stmt.hasQuery, Bool.or_eq_false_iff] at hq
    simp only [exec]
    rw [exec_setDb es wm d n b st h.1 hq.1]
    cases exec es wm n b st with
    | error k st1 =>
      have hh : (hd.bindErr k).hasWrite = false := by rw [Stmt.hasWrite_bindErr]; exact h.2
      have hh' : (hd.bindErr k).hasQuery = false := by rw [Stmt.hasQuery_bindErr]; exact hq.2
      cases u
      · exact exec_setDb es wm d n _ st1 hh hh'
      · have := exec_setDb es wm d n _ { st1 with db := st.db } hh hh'
        simpa [State.setDb, Outcome.setDb] using this
    | _ => rfl
  | _ + 1, .call .., _, h, _ | _ + 1, .insert .., _, h, _ | _ + 1, .update .., _, h, _
  | _ + 1, .delete .., _, h, _ => by simp [Stmt.hasWrite] at h
  | _ + 1, .query .., _, _, hq => by simp [Stmt.hasQuery] at hq

/-- Two outcomes that agree except in the database of a failed or cut-off
run. -/
def Outcome.simDb : Outcome U W → Outcome U W → Prop
  | .normal a, .normal b => a = b
  | .returned v a, .returned u b => v = u ∧ a = b
  | .error k a, .error k' b => k = k' ∧ a.store = b.store ∧ a.world = b.world ∧ a.calls = b.calls
  | .outOfFuel a, .outOfFuel b => a.store = b.store ∧ a.world = b.world ∧ a.calls = b.calls
  | _, _ => False

theorem Outcome.simDb_refl : ∀ o : Outcome U W, o.simDb o
  | .normal _ => rfl
  | .returned _ _ => ⟨rfl, rfl⟩
  | .error _ _ => ⟨rfl, rfl, rfl, rfl⟩
  | .outOfFuel _ => ⟨rfl, rfl, rfl⟩

theorem Outcome.simDb_setDb (d : Db U) :
    ∀ o : Outcome U W, o.failing = true → (o.setDb d).simDb o
  | .error _ _, _ => ⟨rfl, rfl, rfl, rfl⟩
  | .outOfFuel _, _ => ⟨rfl, rfl, rfl⟩
  | .normal _, h | .returned _ _, h => by simp [Outcome.failing] at h

/-- The handler of `tryCatch` may get `undo` though its body is not
`undoSafe`: it always fails and does not touch the database (no write,
and (C3b) no read). -/
def Stmt.failHandler (h : Stmt) : Bool := h.mustFail && !h.hasWrite && !h.hasQuery && !h.hasCatch

/-- `settle`, and `undo` also on every `tryCatch` with a `failHandler`
that is not inside the body of another `tryCatch`. -/
def Stmt.settleTop : Stmt → Stmt
  | .seq s t => .seq s.settleTop t.settleTop
  | .ite c s t => .ite c s.settleTop t.settleTop
  | .while c b => .while c b.settleTop
  | .tryCatch u b h => .tryCatch (u || b.undoSafe false || h.failHandler) b.settle h.settleTop
  | s => s

theorem Stmt.settleTop_bindErr (k : ErrKind) :
    ∀ s : Stmt, (s.bindErr k).settleTop = s.settleTop.bindErr k
  | .reraise => rfl
  | .seq s t => by
    simp [Stmt.bindErr, Stmt.settleTop, Stmt.settleTop_bindErr k s, Stmt.settleTop_bindErr k t]
  | .ite c s t => by
    simp [Stmt.bindErr, Stmt.settleTop, Stmt.settleTop_bindErr k s, Stmt.settleTop_bindErr k t]
  | .while c b => by simp [Stmt.bindErr, Stmt.settleTop, Stmt.settleTop_bindErr k b]
  | .tryCatch u b h => by
    simp [Stmt.bindErr, Stmt.settleTop, Stmt.settle_bindErr, Stmt.undoSafe_bindErr]
  | .skip | .assign .. | .ret _ | .call .. | .insert .. | .update .. | .delete .. | .raise _
  | .commit | .query .. => rfl

theorem Stmt.settleTop_noCatch : ∀ s : Stmt, s.hasCatch = false → s.settleTop = s
  | .seq s t, h => by
    simp only [Stmt.hasCatch, Bool.or_eq_false_iff] at h
    simp [Stmt.settleTop, Stmt.settleTop_noCatch s h.1, Stmt.settleTop_noCatch t h.2]
  | .ite c s t, h => by
    simp only [Stmt.hasCatch, Bool.or_eq_false_iff] at h
    simp [Stmt.settleTop, Stmt.settleTop_noCatch s h.1, Stmt.settleTop_noCatch t h.2]
  | .while c b, h => by
    simp only [Stmt.hasCatch] at h
    simp [Stmt.settleTop, Stmt.settleTop_noCatch b h]
  | .tryCatch .., h => by simp [Stmt.hasCatch] at h
  | .skip, _ | .assign .., _ | .ret _, _ | .call .., _ | .insert .., _ | .update .., _
  | .delete .., _ | .raise _, _ | .reraise, _ | .commit, _ | .query .., _ => rfl

theorem Stmt.vars_settleTop : ∀ s : Stmt, s.settleTop.vars = s.vars
  | .seq s t => by simp [Stmt.settleTop, Stmt.vars, Stmt.vars_settleTop s, Stmt.vars_settleTop t]
  | .ite c s t => by
    simp [Stmt.settleTop, Stmt.vars, Stmt.vars_settleTop s, Stmt.vars_settleTop t]
  | .while c b => by simp [Stmt.settleTop, Stmt.vars, Stmt.vars_settleTop b]
  | .tryCatch u b h => by
    simp [Stmt.settleTop, Stmt.vars, Stmt.vars_settle b, Stmt.vars_settleTop h]
  | .skip | .assign .. | .ret _ | .call .. | .insert .. | .update .. | .delete .. | .raise _
  | .reraise | .commit | .query .. => rfl

/-- `settleTop` changes an outcome at most in the database of a failed or
cut-off run. -/
theorem exec_settleTop (es : ExprSem U) (wm : WorldModel U W) :
    ∀ (n : Nat) (s : Stmt) (st : State U W),
      (exec es wm n s.settleTop st).simDb (exec es wm n s st)
  | 0, _, _ => ⟨rfl, rfl, rfl⟩
  | n + 1, .seq s t, st => by
    have ih := exec_settleTop es wm n s st
    simp only [Stmt.settleTop, exec]
    generalize exec es wm n s.settleTop st = oA at ih ⊢
    generalize exec es wm n s st = oB at ih ⊢
    cases oA <;> cases oB <;> simp only [Outcome.simDb] at ih ⊢
    all_goals first | exact ih | exact ih.elim | skip
    subst ih
    exact exec_settleTop es wm n t _
  | n + 1, .ite c s t, st => by
    simp only [Stmt.settleTop, exec]
    cases c.eval es st.store nullStore with
    | err k => exact Outcome.simDb_refl _
    | ok v =>
      dsimp only
      split
      · exact exec_settleTop es wm n s st
      · exact exec_settleTop es wm n t st
  | n + 1, .while c b, st => by
    simp only [Stmt.settleTop, exec]
    cases c.eval es st.store nullStore with
    | err k => exact Outcome.simDb_refl _
    | ok v =>
      dsimp only
      split
      · have ih := exec_settleTop es wm n b st
        generalize exec es wm n b.settleTop st = oA at ih ⊢
        generalize exec es wm n b st = oB at ih ⊢
        cases oA <;> cases oB <;> simp only [Outcome.simDb] at ih ⊢
        all_goals first | exact ih | exact ih.elim | skip
        subst ih
        exact exec_settleTop es wm n (.while c b) _
      · exact Outcome.simDb_refl _
  | n + 1, .tryCatch u b hd, st => by
    simp only [Stmt.settleTop, exec, exec_settle es wm n b st]
    cases hb : exec es wm n b st with
    | error k st1 =>
      dsimp only
      rw [← Stmt.settleTop_bindErr]
      -- Either the handler starts from the same state as before, or it is
      -- a `failHandler` that starts from another database.
      by_cases hsame : (u || b.undoSafe false || hd.failHandler) = u ∨
          (u = false ∧ b.undoSafe false = true)
      · have hst : (if (u || b.undoSafe false || hd.failHandler) then { st1 with db := st.db } else st1)
            = (if u then { st1 with db := st.db } else st1) := by
          rcases hsame with h | ⟨hu, hs⟩
          · rw [h]
          · subst hu
            obtain ⟨_, hdb⟩ := exec_undoSafe es wm n b st false k st1 hs hb
            simp only [Bool.false_or, hs, Bool.true_or, ite_true, Bool.false_eq_true, ite_false]
            rw [← hdb]
        rw [hst]
        exact exec_settleTop es wm n _ _
      · have hu : u = false := by cases u <;> simp_all
        have hs : b.undoSafe false = false := by
          cases h' : b.undoSafe false <;> simp_all
        have hf : hd.failHandler = true := by subst hu; cases h' : hd.failHandler <;> simp_all
        subst hu
        simp only [hs, hf, Bool.false_or, ite_true, Bool.false_eq_true, ite_false]
        simp only [Stmt.failHandler, Bool.and_eq_true, Bool.not_eq_true'] at hf
        obtain ⟨⟨⟨hm, hw⟩, hq⟩, hc⟩ := hf
        rw [Stmt.settleTop_noCatch _ (by rw [Stmt.hasCatch_bindErr]; exact hc)]
        have := exec_setDb es wm st.db n (hd.bindErr k) st1 (by rw [Stmt.hasWrite_bindErr]; exact hw)
          (by rw [Stmt.hasQuery_bindErr]; exact hq)
        simp only [State.setDb] at this
        rw [this]
        exact Outcome.simDb_setDb _ _
          (exec_mustFail es wm n _ _ (by rw [Stmt.mustFail_bindErr]; exact hm))
    | _ => exact Outcome.simDb_refl _
  | _ + 1, .skip, _ | _ + 1, .assign .., _ | _ + 1, .ret _, _ | _ + 1, .call .., _
  | _ + 1, .insert .., _ | _ + 1, .update .., _ | _ + 1, .delete .., _ | _ + 1, .raise _, _
  | _ + 1, .reraise, _ | _ + 1, .commit, _ | _ + 1, .query .., _ => Outcome.simDb_refl _

def Routine.settleTop (r : Routine) : Routine := { r with body := r.body.settleTop }

theorem Routine.vars_settleTop (r : Routine) : r.settleTop.vars = r.vars := by
  simp [Routine.settleTop, Routine.vars, Stmt.vars_settleTop]

/-- Outcomes that agree but in the database of a failed or cut-off run
are observed alike. -/
theorem observe_simDb (r r' : Routine) (hp : r'.params = r.params) :
    ∀ o o' : Outcome U W, o'.simDb o → observe r' o' = observe r o
  | .normal _, .normal _, h => by simp only [Outcome.simDb] at h; subst h; simp [observe, hp]
  | .returned _ _, .returned _ _, h => by
    simp only [Outcome.simDb] at h; obtain ⟨rfl, rfl⟩ := h; simp [observe, hp]
  | .error _ _, .error _ _, h => by
    simp only [Outcome.simDb] at h; obtain ⟨rfl, _, _, hc⟩ := h; simp [observe, hc]
  | .outOfFuel _, .outOfFuel _, h => by
    simp only [Outcome.simDb] at h; obtain ⟨_, hw, hc⟩ := h; simp [observe, hw, hc]
  | .normal _, .returned .., h | .normal _, .error .., h | .normal _, .outOfFuel _, h
  | .returned .., .normal _, h | .returned .., .error .., h | .returned .., .outOfFuel _, h
  | .error .., .normal _, h | .error .., .returned .., h | .error .., .outOfFuel _, h
  | .outOfFuel _, .normal _, h | .outOfFuel _, .returned .., h | .outOfFuel _, .error .., h => by
    simp [Outcome.simDb] at h

/-- A routine settled at the top shows the same as the routine, with the
same fuel. -/
theorem Routine.obs_settleTop (es : ExprSem U) (wm : WorldModel U W) (r : Routine)
    (args : List (Val U)) (w : W) (db : Db U) (n : Nat) :
    r.settleTop.obs es wm args w db n = r.obs es wm args w db n := by
  unfold Routine.obs
  rw [show r.settleTop.params = r.params from rfl, show r.settleTop.body = r.body.settleTop from rfl]
  cases initStore es r.params args with
  | error k => rfl
  | ok σ => exact observe_simDb r r.settleTop rfl _ _ (exec_settleTop es wm n r.body _)

/-- What the mirror check confirms (`Sqldiff.Mirror.check`): the routine
settled at the top, renamed injectively, refines the routine. -/
theorem refines_settleTop_rename (es : ExprSem U) (wm : WorldModel U W) (r : Routine)
    {f : Var → Var} (inj : InjOn f r.vars) : Refines es wm r (r.settleTop.rename f) := by
  have hr := refines_rename es wm r.settleTop (f := f) (by rw [Routine.vars_settleTop]; exact inj)
  intro args w db
  obtain ⟨h1, h2⟩ := hr args w db
  refine ⟨fun n => ?_, fun hd n => ?_⟩
  · obtain ⟨m, hm⟩ := h1 n
    exact ⟨m, by rw [hm, Routine.obs_settleTop]⟩
  · obtain ⟨w', cs, h⟩ := h2 hd n
    exact ⟨w', cs, by rw [← Routine.obs_settleTop]; exact h⟩

end Sqldiff.Core
