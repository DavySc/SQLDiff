/-
Renaming variables injectively changes nothing a caller observes. This is
the one semantic fact the C1 mirror check (`Sqldiff.Mirror.Check`) needs:
it confirms a conversion only when the lowered target is the lowered
source with its variables renamed, by a renaming injective on the
source's variables.

The proof is a lock-step simulation: with the same fuel, the source and
its renaming take the same steps, their stores agree through the renaming
on every variable the source names, and their worlds and call lists are
equal. So the two observations are equal for every fuel, which gives both
clauses of `Refines` with the same fuel on each side (`refines_rename`).
-/
import Sqldiff.Core.Semantics

namespace Sqldiff.Core

variable {U W : Type}

/-- `f` maps distinct names of `D` to distinct names. -/
def InjOn (f : Var → Var) (D : List Var) : Prop :=
  ∀ x ∈ D, ∀ y ∈ D, f x = f y → x = y

instance (f : Var → Var) (D : List Var) : Decidable (InjOn f D) := by
  unfold InjOn; infer_instance

/-- The stores agree through `f` on the names in `D`. -/
def Agree (f : Var → Var) (D : List Var) (σA σB : Store U) : Prop :=
  ∀ x ∈ D, σA x = σB (f x)

/-- `xs ⊆ D`, spelt out so the definitions stay in core Lean. -/
def Sub (xs D : List Var) : Prop := ∀ x ∈ xs, x ∈ D

section
variable {f : Var → Var} {D : List Var}

theorem Agree.set {σA σB : Store U} {x : Var} (inj : InjOn f D) (hx : x ∈ D)
    (h : Agree f D σA σB) (v : Val U) : Agree f D (σA.set x v) (σB.set (f x) v) := by
  intro y hy
  simp only [Store.set]
  by_cases hyx : y = x
  · simp [hyx]
  · have hf : f y ≠ f x := fun e => hyx (inj y hy x hx e)
    simp [hyx, hf, h y hy]

theorem SExpr.eval_rename {σA σB : Store U} (h : Agree f D σA σB) :
    ∀ s : SExpr, Sub s.vars D → (s.rename f).eval σB = s.eval σA
  | .var x, hs => by
    simp only [SExpr.rename, SExpr.eval]
    rw [h x (hs x (by simp [SExpr.vars]))]
  | .int _, _ => rfl
  | .null, _ => rfl
  | .arith _ a b, hs => by
    simp only [SExpr.rename, SExpr.eval]
    rw [SExpr.eval_rename h a (fun y hy => hs y (by simp [SExpr.vars, hy])),
      SExpr.eval_rename h b (fun y hy => hs y (by simp [SExpr.vars, hy]))]
  | .neg a, hs => by
    simp only [SExpr.rename, SExpr.eval]
    rw [SExpr.eval_rename h a (fun y hy => hs y (by simp [SExpr.vars, hy]))]
  | .cmp _ a b, hs => by
    simp only [SExpr.rename, SExpr.eval]
    rw [SExpr.eval_rename h a (fun y hy => hs y (by simp [SExpr.vars, hy])),
      SExpr.eval_rename h b (fun y hy => hs y (by simp [SExpr.vars, hy]))]
  | .and a b, hs => by
    simp only [SExpr.rename, SExpr.eval]
    rw [SExpr.eval_rename h a (fun y hy => hs y (by simp [SExpr.vars, hy])),
      SExpr.eval_rename h b (fun y hy => hs y (by simp [SExpr.vars, hy]))]
  | .or a b, hs => by
    simp only [SExpr.rename, SExpr.eval]
    rw [SExpr.eval_rename h a (fun y hy => hs y (by simp [SExpr.vars, hy])),
      SExpr.eval_rename h b (fun y hy => hs y (by simp [SExpr.vars, hy]))]
  | .not a, hs => by
    simp only [SExpr.rename, SExpr.eval]
    rw [SExpr.eval_rename h a (fun y hy => hs y (by simp [SExpr.vars, hy]))]
  | .isNull a _, hs => by
    simp only [SExpr.rename, SExpr.eval]
    rw [SExpr.eval_rename h a (fun y hy => hs y (by simp [SExpr.vars, hy]))]
  | .coalesce a b, hs => by
    simp only [SExpr.rename, SExpr.eval]
    rw [SExpr.eval_rename h a (fun y hy => hs y (by simp [SExpr.vars, hy])),
      SExpr.eval_rename h b (fun y hy => hs y (by simp [SExpr.vars, hy]))]
  | .cond c a b, hs => by
    simp only [SExpr.rename, SExpr.eval]
    rw [SExpr.eval_rename h c (fun y hy => hs y (by simp [SExpr.vars, hy])),
      SExpr.eval_rename h a (fun y hy => hs y (by simp [SExpr.vars, hy])),
      SExpr.eval_rename h b (fun y hy => hs y (by simp [SExpr.vars, hy]))]

mutual
  theorem Expr.eval_rename (es : ExprSem U) {σA σB : Store U} (h : Agree f D σA σB) :
      ∀ e : Expr, Sub e.vars D → (e.rename f).eval es σB = e.eval es σA
    | .sc s, hs => SExpr.eval_rename h s hs
    | .op g args, hs => by
      simp only [Expr.rename, Expr.eval]
      rw [Expr.evalList_rename es h args (fun y hy => hs y (by simpa [Expr.vars] using hy))]

  theorem Expr.evalList_rename (es : ExprSem U) {σA σB : Store U} (h : Agree f D σA σB) :
      ∀ args : List Expr, Sub (Expr.varsList args) D →
        Expr.evalList es σB (Expr.renameList f args) = Expr.evalList es σA args
    | [], _ => rfl
    | e :: es', hs => by
      simp only [Expr.renameList, Expr.evalList]
      rw [Expr.eval_rename es h e (fun y hy => hs y (by simp [Expr.varsList, hy])),
        Expr.evalList_rename es h es' (fun y hy => hs y (by simp [Expr.varsList, hy]))]
end

theorem Arg.eval_rename (es : ExprSem U) {σA σB : Store U} (h : Agree f D σA σB) :
    ∀ a : Arg, Sub a.vars D → (a.rename f).eval es σB = a.eval es σA
  | .val e, hs => Expr.eval_rename es h e hs
  | .ref x, hs => by
    simp only [Arg.rename, Arg.eval]
    rw [h x (hs x (by simp [Arg.vars]))]

theorem args_eval_rename (es : ExprSem U) {σA σB : Store U} (h : Agree f D σA σB) :
    ∀ args : List Arg, Sub (args.map Arg.vars).flatten D →
      Arg.evalAll es σB (args.map (Arg.rename f)) = Arg.evalAll es σA args
  | [], _ => rfl
  | a :: as, hs => by
    simp only [List.map_cons, Arg.evalAll]
    rw [Arg.eval_rename es h a (fun x hx => hs x (by simp [hx])),
      args_eval_rename es h as (fun x hx => hs x (by simp_all))]

theorem Agree.setRefs (inj : InjOn f D) :
    ∀ (args : List Arg) (vs : List (Val U)) (σA σB : Store U), Sub (args.map Arg.vars).flatten D →
      Agree f D σA σB →
      Agree f D (σA.setRefs args vs) (σB.setRefs (args.map (Arg.rename f)) vs)
  | [], _, _, _, _, h => by simpa [Store.setRefs] using h
  | .ref _ :: _, [], _, _, _, h => by simpa [Store.setRefs, Arg.rename] using h
  | .val _ :: _, [], _, _, _, h => by simpa [Store.setRefs, Arg.rename] using h
  | .ref x :: as, v :: vs, σA, σB, hs, h => by
    simp only [List.map_cons, Arg.rename, Store.setRefs]
    exact Agree.setRefs inj as vs _ _ (fun y hy => hs y (by simp [hy]))
      (h.set inj (hs x (by simp [Arg.vars])) v)
  | .val _ :: as, _ :: vs, σA, σB, hs, h => by
    simp only [List.map_cons, Arg.rename, Store.setRefs]
    exact Agree.setRefs inj as vs _ _ (fun y hy => hs y (by simp [hy])) h

/-! ## The lock-step simulation -/

/-- Related states: stores agree through `f` on `D`; same world, same calls. -/
def StRel (f : Var → Var) (D : List Var) (a b : State U W) : Prop :=
  Agree f D a.store b.store ∧ a.world = b.world ∧ a.calls = b.calls

/-- Related outcomes: the same kind of ending, related states, and the
same returned value or error. -/
def OutRel (f : Var → Var) (D : List Var) : Outcome U W → Outcome U W → Prop
  | .normal a, .normal b => StRel f D a b
  | .returned v a, .returned u b => v = u ∧ StRel f D a b
  | .error k a, .error k' b => k = k' ∧ StRel f D a b
  | .outOfFuel a, .outOfFuel b => StRel f D a b
  | _, _ => False

theorem exec_rename (es : ExprSem U) (wm : WorldModel U W) (inj : InjOn f D) :
    ∀ (n : Nat) (s : Stmt) (a b : State U W), Sub s.vars D → StRel f D a b →
      OutRel f D (exec es wm n s a) (exec es wm n (s.rename f) b)
  | 0, _, _, _, _, h => by simpa [exec, OutRel] using h
  | _ + 1, .skip, _, _, _, h => by simpa [exec, Stmt.rename, OutRel] using h
  | _ + 1, .assign x e, a, b, hs, h => by
    obtain ⟨hst, hw, hc⟩ := h
    simp only [exec, Stmt.rename]
    rw [Expr.eval_rename es hst e (fun y hy => hs y (by simp [Stmt.vars, hy]))]
    cases Expr.eval es a.store e with
    | err k => exact ⟨rfl, hst, hw, hc⟩
    | ok v => exact ⟨hst.set inj (hs x (by simp [Stmt.vars])) v, hw, hc⟩
  | n + 1, .seq s t, a, b, hs, h => by
    have ih := exec_rename es wm inj n s a b (fun y hy => hs y (by simp [Stmt.vars, hy])) h
    simp only [exec, Stmt.rename]
    generalize exec es wm n s a = oA at ih ⊢
    generalize exec es wm n (s.rename f) b = oB at ih ⊢
    cases oA <;> cases oB <;> simp only [OutRel] at ih ⊢
    · exact exec_rename es wm inj n t _ _ (fun y hy => hs y (by simp [Stmt.vars, hy])) ih
    all_goals first | exact ih | exact ih.elim
  | n + 1, .ite c s t, a, b, hs, h => by
    simp only [exec, Stmt.rename]
    rw [Expr.eval_rename es h.1 c (fun y hy => hs y (by simp [Stmt.vars, hy]))]
    cases Expr.eval es a.store c with
    | err k => exact ⟨rfl, h⟩
    | ok v =>
      dsimp only
      split
      · exact exec_rename es wm inj n s a b (fun y hy => hs y (by simp [Stmt.vars, hy])) h
      · exact exec_rename es wm inj n t a b (fun y hy => hs y (by simp [Stmt.vars, hy])) h
  | n + 1, .while c body, a, b, hs, h => by
    simp only [exec, Stmt.rename]
    rw [Expr.eval_rename es h.1 c (fun y hy => hs y (by simp [Stmt.vars, hy]))]
    cases Expr.eval es a.store c with
    | err k => exact ⟨rfl, h⟩
    | ok v =>
      dsimp only
      split
      · have ih := exec_rename es wm inj n body a b
          (fun y hy => hs y (by simp [Stmt.vars, hy])) h
        generalize exec es wm n body a = oA at ih ⊢
        generalize exec es wm n (body.rename f) b = oB at ih ⊢
        cases oA <;> cases oB <;> dsimp only
        · have := exec_rename es wm inj n (.while c body) _ _ hs ih
          simpa only [Stmt.rename] using this
        all_goals first | exact ih | exact (ih : False).elim
      · simpa [OutRel] using h
  | _ + 1, .ret none, _, _, _, h => by simpa [exec, Stmt.rename, OutRel] using h
  | _ + 1, .ret (some e), a, b, hs, h => by
    simp only [exec, Stmt.rename, Option.map_some]
    rw [Expr.eval_rename es h.1 e (fun y hy => hs y (by simp [Stmt.vars, hy]))]
    cases Expr.eval es a.store e with
    | err k => exact ⟨rfl, h⟩
    | ok v => exact ⟨rfl, h⟩
  | _ + 1, .call p args, a, b, hs, h => by
    obtain ⟨hst, hw, hc⟩ := h
    have hargs : Sub (args.map Arg.vars).flatten D := fun y hy => hs y (by simpa [Stmt.vars] using hy)
    simp only [exec, Stmt.rename]
    rw [args_eval_rename es hst args hargs]
    cases Arg.evalAll es a.store args with
    | error k => exact ⟨rfl, hst, hw, hc⟩
    | ok vs =>
      dsimp only
      rw [hw]
      simp only [OutRel, StRel]
      refine ⟨Agree.setRefs inj args _ _ _ hargs hst, ?_⟩
      simp [hc]

/-! ## Routines -/

theorem Agree.null : Agree f D (nullStore : Store U) nullStore :=
  fun _ _ => rfl

theorem paramValue_rename (es : ExprSem U) (p : Param) (hs : Sub p.vars D) (arg : Option (Val U)) :
    paramValue es (p.rename f) arg = paramValue es p arg := by
  cases p with
  | mk x m d =>
    cases m <;> cases arg <;> cases d <;>
      simp only [paramValue, Param.rename, Option.map_some, Option.map_none]
    all_goals
      first
      | rfl
      | exact Expr.eval_rename es Agree.null _
          (fun y hy => hs y (by simp [Param.vars, hy]))

/-- Related entry stores: the same error, or stores that agree. -/
def BindRel (f : Var → Var) (D : List Var) :
    Except ErrKind (Store U) → Except ErrKind (Store U) → Prop
  | .error k, .error k' => k = k'
  | .ok σA, .ok σB => Agree f D σA σB
  | _, _ => False

theorem Agree.bindParams (es : ExprSem U) (inj : InjOn f D) :
    ∀ (ps : List Param) (vs : List (Val U)) (σA σB : Store U), Sub (ps.map Param.vars).flatten D →
      Agree f D σA σB →
      BindRel f D (Core.bindParams es ps vs σA) (Core.bindParams es (ps.map (Param.rename f)) vs σB)
  | [], _, _, _, _, h => by simpa [Core.bindParams, BindRel] using h
  | p :: ps, [], σA, σB, hs, h => by
    simp only [List.map_cons, Core.bindParams]
    have hp : Sub p.vars D := fun y hy => hs y (by simp [hy])
    rw [paramValue_rename es p hp]
    cases paramValue es p none with
    | err k => exact rfl
    | ok v =>
      exact Agree.bindParams es inj ps [] _ _ (fun y hy => hs y (by simp_all))
        (h.set inj (hp p.name (by simp [Param.vars])) _)
  | p :: ps, v :: vs, σA, σB, hs, h => by
    simp only [List.map_cons, Core.bindParams]
    have hp : Sub p.vars D := fun y hy => hs y (by simp [hy])
    rw [paramValue_rename es p hp]
    cases paramValue es p (some v) with
    | err k => exact rfl
    | ok v' =>
      exact Agree.bindParams es inj ps vs _ _ (fun y hy => hs y (by simp_all))
        (h.set inj (hp p.name (by simp [Param.vars])) _)

theorem outVals_rename {σA σB : Store U} (h : Agree f D σA σB) :
    ∀ ps : List Param, Sub (ps.map Param.vars).flatten D →
      outVals σB (ps.map (Param.rename f)) = outVals σA ps
  | [], _ => rfl
  | p :: ps, hs => by
    have ih := outVals_rename h ps (fun y hy => hs y (by simp_all))
    have hx := h p.name (hs p.name (by simp [Param.vars]))
    simp only [List.map_cons, outVals, Param.rename]
    split <;> simp_all

/-- With the same fuel, a routine and its injective renaming give the
same observation. -/
theorem obs_rename (es : ExprSem U) (wm : WorldModel U W) (r : Routine)
    (inj : InjOn f r.vars) (args : List (Val U)) (w : W) (n : Nat) :
    (r.rename f).obs es wm args w n = r.obs es wm args w n := by
  have hp : Sub (r.params.map Param.vars).flatten r.vars := fun y hy => by
    simp only [Routine.vars, List.mem_append]; exact Or.inl (Or.inl hy)
  have hb : Sub r.body.vars r.vars := fun y hy => by
    simp only [Routine.vars, List.mem_append]; exact Or.inr hy
  have hbind := Agree.bindParams es inj r.params args _ _ hp (Agree.null (f := f) (D := r.vars))
  simp only [Routine.obs, Routine.rename, initStore]
  generalize Core.bindParams es r.params args nullStore = iA at hbind ⊢
  generalize Core.bindParams es (r.params.map (Param.rename f)) args nullStore = iB at hbind ⊢
  cases iA <;> cases iB <;> simp only [BindRel] at hbind
  · simp [hbind]
  · next σA σB =>
    have hsim := exec_rename es wm inj n r.body ⟨σA, w, []⟩ ⟨σB, w, []⟩ hb ⟨hbind, rfl, rfl⟩
    dsimp only
    generalize exec es wm n r.body _ = oA at hsim ⊢
    generalize exec es wm n (r.body.rename f) _ = oB at hsim ⊢
    cases oA <;> cases oB <;> simp only [OutRel] at hsim
    · obtain ⟨hst, hw, hc⟩ := hsim
      simp [observe, outVals_rename hst r.params hp, hw, hc]
    · obtain ⟨hv, hst, hw, hc⟩ := hsim
      simp [observe, outVals_rename hst r.params hp, hv, hw, hc]
    · obtain ⟨hk, _, hw, hc⟩ := hsim
      simp [observe, hk, hw, hc]
    · obtain ⟨_, hw, hc⟩ := hsim
      simp [observe, hw, hc]

/-- An injective renaming of a routine refines it (and, by the same
proof, is refined by it). -/
theorem refines_rename (es : ExprSem U) (wm : WorldModel U W) (r : Routine)
    (inj : InjOn f r.vars) : Refines es wm r (r.rename f) := by
  intro args w
  refine ⟨fun n => ⟨n, obs_rename es wm r inj args w n⟩, fun hd n => ?_⟩
  rw [← obs_rename es wm r inj args w n]
  exact hd n

end

end Sqldiff.Core
