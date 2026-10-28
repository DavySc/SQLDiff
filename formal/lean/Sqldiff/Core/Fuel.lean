/-
Fuel only cuts runs off: once a run finishes with some fuel, more fuel
gives the same outcome. So a finished observation is a property of the
run, not of the fuel used to compute it, and "terminates" (some fuel
finishes) and "diverges" (every fuel runs out) are the only two cases.
-/
import Sqldiff.Core.Semantics

namespace Sqldiff.Core

variable {U W : Type}

/-- The run ended (normally, by `return` or by an error), rather than
running out. -/
def Outcome.finished : Outcome U W → Bool
  | .outOfFuel _ => false
  | _ => true

theorem exec_succ (es : ExprSem U) (wm : WorldModel U W) :
    ∀ (n : Nat) (s : Stmt) (st : State U W), (exec es wm n s st).finished = true →
      exec es wm (n + 1) s st = exec es wm n s st
  | 0, _, _, h => by simp [exec, Outcome.finished] at h
  | _ + 1, .skip, _, _ => rfl
  | _ + 1, .assign .., _, _ => rfl
  | _ + 1, .ret none, _, _ => rfl
  | _ + 1, .ret (some _), _, _ => rfl
  | _ + 1, .call .., _, _ => rfl
  | _ + 1, .insert .., _, _ => rfl
  | _ + 1, .update .., _, _ => rfl
  | _ + 1, .delete .., _, _ => rfl
  | n + 1, .seq s t, st, h => by
    have e1 : exec es wm (n + 1 + 1) (.seq s t) st =
        match exec es wm (n + 1) s st with
        | .normal st' => exec es wm (n + 1) t st'
        | o => o := rfl
    have e2 : exec es wm (n + 1) (.seq s t) st =
        match exec es wm n s st with
        | .normal st' => exec es wm n t st'
        | o => o := rfl
    rw [e2] at h
    rw [e1, e2]
    cases hs : exec es wm n s st with
    | normal st' =>
      rw [hs] at h
      rw [exec_succ es wm n s st (by rw [hs]; rfl), hs]
      exact exec_succ es wm n t st' h
    | returned v st' =>
      rw [exec_succ es wm n s st (by rw [hs]; rfl), hs]
    | error k st' =>
      rw [exec_succ es wm n s st (by rw [hs]; rfl), hs]
    | outOfFuel st' =>
      rw [hs] at h
      simp [Outcome.finished] at h
  | n + 1, .ite c s t, st, h => by
    have e1 : exec es wm (n + 1 + 1) (.ite c s t) st =
        match c.eval es st.store nullStore with
        | .err k => .error k st
        | .ok v => if isTrue v then exec es wm (n + 1) s st else exec es wm (n + 1) t st := rfl
    have e2 : exec es wm (n + 1) (.ite c s t) st =
        match c.eval es st.store nullStore with
        | .err k => .error k st
        | .ok v => if isTrue v then exec es wm n s st else exec es wm n t st := rfl
    rw [e2] at h
    rw [e1, e2]
    generalize c.eval es st.store nullStore = r at h ⊢
    cases r with
    | err k => rfl
    | ok v =>
      dsimp only at h ⊢
      split
      · next hc => simp only [hc, ↓reduceIte] at h; exact exec_succ es wm n s st h
      · next hc => simp only [hc] at h; exact exec_succ es wm n t st h
  | n + 1, .while c b, st, h => by
    have e1 : exec es wm (n + 1 + 1) (.while c b) st =
        match c.eval es st.store nullStore with
        | .err k => .error k st
        | .ok v =>
          if isTrue v then
            match exec es wm (n + 1) b st with
            | .normal st' => exec es wm (n + 1) (.while c b) st'
            | o => o
          else .normal st := rfl
    have e2 : exec es wm (n + 1) (.while c b) st =
        match c.eval es st.store nullStore with
        | .err k => .error k st
        | .ok v =>
          if isTrue v then
            match exec es wm n b st with
            | .normal st' => exec es wm n (.while c b) st'
            | o => o
          else .normal st := rfl
    rw [e2] at h
    rw [e1, e2]
    generalize c.eval es st.store nullStore = r at h ⊢
    cases r with
    | err k => rfl
    | ok v =>
      dsimp only at h ⊢
      split
      · next hc =>
        simp only [hc, ↓reduceIte] at h
        cases hs : exec es wm n b st with
        | normal st' =>
          rw [hs] at h
          rw [exec_succ es wm n b st (by rw [hs]; rfl), hs]
          exact exec_succ es wm n (.while c b) st' h
        | returned v st' =>
          rw [exec_succ es wm n b st (by rw [hs]; rfl), hs]
        | error k st' =>
          rw [exec_succ es wm n b st (by rw [hs]; rfl), hs]
        | outOfFuel st' =>
          rw [hs] at h
          simp [Outcome.finished] at h
      · rfl

theorem exec_mono (es : ExprSem U) (wm : WorldModel U W) (n : Nat) (s : Stmt)
    (st : State U W) (h : (exec es wm n s st).finished = true) :
    ∀ m, n ≤ m → exec es wm m s st = exec es wm n s st := by
  intro m hm
  induction m with
  | zero =>
    have : n = 0 := Nat.le_zero.mp hm
    subst this; rfl
  | succ m ih =>
    rcases Nat.lt_or_eq_of_le hm with hlt | heq
    · have ih' := ih (Nat.le_of_lt_succ hlt)
      rw [← ih'] at h
      rw [exec_succ es wm m s st h, ih']
    · rw [heq]

/-- A finished observation (a result or an error) stays the same with
more fuel. -/
theorem obs_mono (es : ExprSem U) (wm : WorldModel U W) (r : Routine) (args : List (Val U))
    (w : W) (db : Db U) (n : Nat) (h : ∀ w' cs, r.obs es wm args w db n ≠ .pending w' cs) :
    ∀ m, n ≤ m → r.obs es wm args w db m = r.obs es wm args w db n := by
  intro m hm
  simp only [Routine.obs] at h ⊢
  cases hi : initStore es r.params args with
  | error k => rfl
  | ok σ =>
    rw [hi] at h
    dsimp only at h ⊢
    have hfin : (exec es wm n r.body ⟨σ, w, db, []⟩).finished = true := by
      generalize exec es wm n r.body _ = o at h ⊢
      cases o <;> simp_all [observe, Outcome.finished]
    rw [exec_mono es wm n r.body _ hfin m hm]

end Sqldiff.Core
