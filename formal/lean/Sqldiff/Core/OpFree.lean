/-
A routine without uninterpreted operators means the same for every
meaning of them: its observations do not depend on `ExprSem.op`. This is
what lets a C2a confirmation (`Sqldiff.Mirror.check_sound_c2a`) drop the
C1 premise that an operator name means the same function in both
dialects: in such a routine every expression is a slice expression
(`SExpr`), whose meaning the core fixes.
-/
import Sqldiff.Core.Data

namespace Sqldiff.Core

variable {U W : Type}

theorem Expr.eval_opFree (es es' : ExprSem U) (σ ρ : Store U) :
    ∀ e : Expr, e.opFree = true → e.eval es σ ρ = e.eval es' σ ρ
  | .sc _, _ => rfl
  | .op .., h => by simp [Expr.opFree] at h

theorem Arg.evalAll_opFree (es es' : ExprSem U) (σ : Store U) :
    ∀ args : List Arg, args.all Arg.opFree = true →
      Arg.evalAll es σ args = Arg.evalAll es' σ args
  | [], _ => rfl
  | a :: as, h => by
    simp only [List.all_cons, Bool.and_eq_true] at h
    have ha : a.eval es σ = a.eval es' σ := by
      cases a with
      | val e => exact Expr.eval_opFree es es' σ nullStore e h.1
      | ref _ => rfl
    simp only [Arg.evalAll, ha, Arg.evalAll_opFree es es' σ as h.2]

theorem exec_opFree (es es' : ExprSem U) (wm : WorldModel U W) :
    ∀ (n : Nat) (s : Stmt) (st : State U W), s.opFree = true →
      exec es wm n s st = exec es' wm n s st
  | 0, _, _, _ => rfl
  | _ + 1, .skip, _, _ => rfl
  | _ + 1, .assign _ e, st, h => by
    simp only [exec, Expr.eval_opFree es es' st.store nullStore e h]
  | n + 1, .seq s t, st, h => by
    simp only [Stmt.opFree, Bool.and_eq_true] at h
    simp only [exec, exec_opFree es es' wm n s st h.1]
    cases exec es' wm n s st with
    | normal st' => exact exec_opFree es es' wm n t st' h.2
    | _ => rfl
  | n + 1, .ite c s t, st, h => by
    simp only [Stmt.opFree, Bool.and_eq_true] at h
    simp only [exec, Expr.eval_opFree es es' st.store nullStore c h.1.1, exec_opFree es es' wm n s st h.1.2,
      exec_opFree es es' wm n t st h.2]
  | n + 1, .while c b, st, h => by
    have h' := h
    simp only [Stmt.opFree, Bool.and_eq_true] at h'
    simp only [exec, Expr.eval_opFree es es' st.store nullStore c h'.1, exec_opFree es es' wm n b st h'.2]
    cases c.eval es' st.store nullStore with
    | err _ => rfl
    | ok v =>
      dsimp only
      split
      · cases exec es' wm n b st with
        | normal st' => exact exec_opFree es es' wm n (.while c b) st' h
        | _ => rfl
      · rfl
  | _ + 1, .ret none, _, _ => rfl
  | _ + 1, .ret (some e), st, h => by
    simp only [exec, Expr.eval_opFree es es' st.store nullStore e h]
  | _ + 1, .call _ args, st, h => by
    simp only [exec, Arg.evalAll_opFree es es' st.store args h]
  | _ + 1, .insert t cs rows, st, h => by
    have := insertRows_map (ev := fun ρ e => e.eval es st.store ρ)
      (ev' := fun ρ e => e.eval es' st.store ρ) (g := fun e => e) t cs rows (st.db.get t.name)
      (fun xs hxs e he ρ => (Expr.eval_opFree es es' st.store ρ e (by
        simp only [Stmt.opFree, List.all_eq_true] at h
        exact h xs hxs e he)).symm)
    simp only [List.map_id'] at this
    simp only [exec, this]
  | _ + 1, .update t sets c, st, h => by
    simp only [Stmt.opFree, Bool.and_eq_true, List.all_eq_true] at h
    have := updateRows_map (ev := fun ρ e => e.eval es st.store ρ)
      (ev' := fun ρ e => e.eval es' st.store ρ) (g := fun e => e) t sets c
      (fun p hp ρ => (Expr.eval_opFree es es' st.store ρ p.2 (h.1 p hp)).symm)
      (fun e he ρ => (Expr.eval_opFree es es' st.store ρ e (by subst he; simpa using h.2)).symm)
      (st.db.get t.name)
    simp only [List.map_id', Option.map_id'] at this
    simp only [exec, this]
  | _ + 1, .delete t c, st, h => by
    simp only [Stmt.opFree] at h
    have := deleteRows_map (ev := fun ρ e => e.eval es st.store ρ)
      (ev' := fun ρ e => e.eval es' st.store ρ) (g := fun e => e) t c
      (fun e he ρ => (Expr.eval_opFree es es' st.store ρ e (by subst he; simpa using h)).symm)
      (st.db.get t.name)
    simp only [Option.map_id'] at this
    simp only [exec, this]

theorem bindParams_opFree (es es' : ExprSem U) :
    ∀ (ps : List Param) (vs : List (Val U)) (σ : Store U), ps.all Param.opFree = true →
      bindParams es ps vs σ = bindParams es' ps vs σ
  | [], _, _, _ => rfl
  | p :: ps, vs, σ, h => by
    simp only [List.all_cons, Bool.and_eq_true] at h
    have hp : ∀ arg, paramValue es p arg = paramValue es' p arg := by
      intro arg
      cases p with
      | mk x m d =>
        cases m <;> cases arg <;> cases d <;> simp only [paramValue]
        all_goals first | rfl | exact Expr.eval_opFree es es' _ _ _ h.1
    cases vs with
    | nil =>
      simp only [bindParams, hp none]
      cases paramValue es' p none with
      | err _ => rfl
      | ok v => exact bindParams_opFree es es' ps [] _ h.2
    | cons v vs =>
      simp only [bindParams, hp (some v)]
      cases paramValue es' p (some v) with
      | err _ => rfl
      | ok v' => exact bindParams_opFree es es' ps vs _ h.2

/-- A routine without uninterpreted operators gives the same observation
whatever they mean. -/
theorem Routine.obs_opFree (es es' : ExprSem U) (wm : WorldModel U W) (r : Routine)
    (h : r.opFree = true) (args : List (Val U)) (w : W) (db : Db U) (n : Nat) :
    r.obs es wm args w db n = r.obs es' wm args w db n := by
  simp only [Routine.opFree, Bool.and_eq_true] at h
  simp only [Routine.obs, initStore, bindParams_opFree es es' r.params args nullStore h.1]
  cases bindParams es' r.params args nullStore with
  | error _ => rfl
  | ok σ => simp only [exec_opFree es es' wm n r.body _ h.2]

end Sqldiff.Core
