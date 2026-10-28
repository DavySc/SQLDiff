/-
Fuel only cuts runs off: once a run finishes with some fuel, more fuel
gives the same outcome. So a finished observation is a property of the
run, not of the fuel used to compute it, and "terminates" (some fuel
finishes) and "diverges" (every fuel runs out) are the only two cases.
-/
import Sqldiff.Core.Semantics

namespace Sqldiff.Core

variable {V W : Type}

/-- The run ended (normally or by `return`), rather than running out. -/
def Outcome.finished : Outcome V W → Bool
  | .outOfFuel _ => false
  | _ => true

theorem exec_succ (es : ExprSem V) (wm : WorldModel V W) :
    ∀ (n : Nat) (s : Stmt) (st : State V W), (exec es wm n s st).finished = true →
      exec es wm (n + 1) s st = exec es wm n s st
  | 0, _, _, h => by simp [exec, Outcome.finished] at h
  | _ + 1, .skip, _, _ => rfl
  | _ + 1, .assign .., _, _ => rfl
  | _ + 1, .ret _, _, _ => rfl
  | _ + 1, .call .., _, _ => rfl
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
    | outOfFuel st' =>
      rw [hs] at h
      simp [Outcome.finished] at h
  | n + 1, .ite c s t, st, h => by
    simp only [exec] at h ⊢
    split
    · next hc => simp only [hc, ↓reduceIte] at h; exact exec_succ es wm n s st h
    · next hc => simp only [hc] at h; exact exec_succ es wm n t st h
  | n + 1, .while c b, st, h => by
    have e1 : exec es wm (n + 1 + 1) (.while c b) st =
        if es.truthy (c.eval es st.store) then
          match exec es wm (n + 1) b st with
          | .normal st' => exec es wm (n + 1) (.while c b) st'
          | o => o
        else .normal st := rfl
    have e2 : exec es wm (n + 1) (.while c b) st =
        if es.truthy (c.eval es st.store) then
          match exec es wm n b st with
          | .normal st' => exec es wm n (.while c b) st'
          | o => o
        else .normal st := rfl
    rw [e2] at h
    rw [e1, e2]
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
      | outOfFuel st' =>
        rw [hs] at h
        simp [Outcome.finished] at h
    · rfl

theorem exec_mono (es : ExprSem V) (wm : WorldModel V W) (n : Nat) (s : Stmt)
    (st : State V W) (h : (exec es wm n s st).finished = true) :
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

/-- A finished observation stays the same with more fuel. -/
theorem obs_mono (es : ExprSem V) (wm : WorldModel V W) (r : Routine) (args : List V) (w : W)
    (n : Nat) {outs : List V} {ret : Option V} {w' : W} {cs : List (CallEvent V)}
    (h : r.obs es wm args w n = .done outs ret w' cs) :
    ∀ m, n ≤ m → r.obs es wm args w m = .done outs ret w' cs := by
  intro m hm
  have hfin : (exec es wm n r.body ⟨initStore es r.params args, w, []⟩).finished = true := by
    simp only [Routine.obs] at h
    generalize exec es wm n r.body _ = o at h ⊢
    cases o <;> simp_all [observe, Outcome.finished]
  simp only [Routine.obs] at h ⊢
  rw [exec_mono es wm n r.body _ hfin m hm]
  exact h

end Sqldiff.Core
