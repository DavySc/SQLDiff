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
import Sqldiff.Core.Data

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

theorem SExpr.eval_rename {σA σB ρ : Store U} (h : Agree f D σA σB) :
    ∀ s : SExpr, Sub s.vars D → (s.rename f).eval σB ρ = s.eval σA ρ
  | .col _, _ => rfl
  | .var x, hs => by
    simp only [SExpr.rename, SExpr.eval]
    rw [h x (hs x (by simp [SExpr.vars]))]
  | .int _, _ => rfl
  | .null, _ => rfl
  | .arith _ _ a b, hs => by
    simp only [SExpr.rename, SExpr.eval]
    rw [SExpr.eval_rename h a (fun y hy => hs y (by simp [SExpr.vars, hy])),
      SExpr.eval_rename h b (fun y hy => hs y (by simp [SExpr.vars, hy]))]
  | .neg _ a, hs => by
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
  | .str _, _ => rfl
  | .concat a b, hs => by
    simp only [SExpr.rename, SExpr.eval]
    rw [SExpr.eval_rename h a (fun y hy => hs y (by simp [SExpr.vars, hy])),
      SExpr.eval_rename h b (fun y hy => hs y (by simp [SExpr.vars, hy]))]
  | .rtrim a, hs => by
    simp only [SExpr.rename, SExpr.eval]
    rw [SExpr.eval_rename h a (fun y hy => hs y (by simp [SExpr.vars, hy]))]
  | .streq a b, hs => by
    simp only [SExpr.rename, SExpr.eval]
    rw [SExpr.eval_rename h a (fun y hy => hs y (by simp [SExpr.vars, hy])),
      SExpr.eval_rename h b (fun y hy => hs y (by simp [SExpr.vars, hy]))]
  | .len a, hs => by
    simp only [SExpr.rename, SExpr.eval]
    rw [SExpr.eval_rename h a (fun y hy => hs y (by simp [SExpr.vars, hy]))]
  | .bool _, _ => rfl
  | .date _, _ => rfl
  | .ts _, _ => rfl
  | .dateAdd _ a b, hs => by
    simp only [SExpr.rename, SExpr.eval]
    rw [SExpr.eval_rename h a (fun y hy => hs y (by simp [SExpr.vars, hy])),
      SExpr.eval_rename h b (fun y hy => hs y (by simp [SExpr.vars, hy]))]
  | .dateDiff a b, hs => by
    simp only [SExpr.rename, SExpr.eval]
    rw [SExpr.eval_rename h a (fun y hy => hs y (by simp [SExpr.vars, hy])),
      SExpr.eval_rename h b (fun y hy => hs y (by simp [SExpr.vars, hy]))]
  | .toDate a, hs => by
    simp only [SExpr.rename, SExpr.eval]
    rw [SExpr.eval_rename h a (fun y hy => hs y (by simp [SExpr.vars, hy]))]
  | .toTs a, hs => by
    simp only [SExpr.rename, SExpr.eval]
    rw [SExpr.eval_rename h a (fun y hy => hs y (by simp [SExpr.vars, hy]))]

mutual
  theorem Expr.eval_rename (es : ExprSem U) {σA σB ρ : Store U} (h : Agree f D σA σB) :
      ∀ e : Expr, Sub e.vars D → (e.rename f).eval es σB ρ = e.eval es σA ρ
    | .sc s, hs => SExpr.eval_rename h s hs
    | .op g args, hs => by
      simp only [Expr.rename, Expr.eval]
      rw [Expr.evalList_rename es h args (fun y hy => hs y (by simpa [Expr.vars] using hy))]

  theorem Expr.evalList_rename (es : ExprSem U) {σA σB ρ : Store U} (h : Agree f D σA σB) :
      ∀ args : List Expr, Sub (Expr.varsList args) D →
        Expr.evalList es σB ρ (Expr.renameList f args) = Expr.evalList es σA ρ args
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

theorem Stmt.rename_bindErr (k : ErrKind) :
    ∀ s : Stmt, (s.bindErr k).rename f = (s.rename f).bindErr k
  | .reraise => rfl
  | .seq s t => by
    simp [Stmt.bindErr, Stmt.rename, Stmt.rename_bindErr k s, Stmt.rename_bindErr k t]
  | .ite c s t => by
    simp [Stmt.bindErr, Stmt.rename, Stmt.rename_bindErr k s, Stmt.rename_bindErr k t]
  | .while c b => by simp [Stmt.bindErr, Stmt.rename, Stmt.rename_bindErr k b]
  | .tryCatch u b h => by simp [Stmt.bindErr, Stmt.rename, Stmt.rename_bindErr k b]
  | .skip | .assign .. | .ret _ | .call .. | .insert .. | .update .. | .delete .. | .raise _
  | .commit | .query .. => rfl

theorem Stmt.vars_bindErr (k : ErrKind) : ∀ s : Stmt, (s.bindErr k).vars = s.vars
  | .reraise => rfl
  | .seq s t => by simp [Stmt.bindErr, Stmt.vars, Stmt.vars_bindErr k s, Stmt.vars_bindErr k t]
  | .ite c s t => by simp [Stmt.bindErr, Stmt.vars, Stmt.vars_bindErr k s, Stmt.vars_bindErr k t]
  | .while c b => by simp [Stmt.bindErr, Stmt.vars, Stmt.vars_bindErr k b]
  | .tryCatch u b h => by simp [Stmt.bindErr, Stmt.vars, Stmt.vars_bindErr k b]
  | .skip | .assign .. | .ret _ | .call .. | .insert .. | .update .. | .delete .. | .raise _
  | .commit | .query .. => rfl

/-- Related stores after a step that may fail: the same error, or stores
that agree (entry stores, C3b picks). -/
def BindRel (f : Var → Var) (D : List Var) :
    Except ErrKind (Store U) → Except ErrKind (Store U) → Prop
  | .error k, .error k' => k = k'
  | .ok σA, .ok σB => Agree f D σA σB
  | _, _ => False

theorem Agree.setAll (inj : InjOn f D) :
    ∀ (xs : List Var) (vs : List (Val U)) (σA σB : Store U), Sub xs D → Agree f D σA σB →
      Agree f D (σA.setAll xs vs) (σB.setAll (xs.map f) vs)
  | [], _, _, _, _, h => by simpa [Store.setAll] using h
  | _ :: _, [], _, _, _, h => by simpa [Store.setAll] using h
  | x :: xs, v :: vs, σA, σB, hs, h => by
    simp only [List.map_cons, Store.setAll]
    exact Agree.setAll inj xs vs _ _ (fun y hy => hs y (by simp [hy]))
      (h.set inj (hs x (by simp)) v)

/-- (C3b) A pick and its renaming, on the same rows, with expressions that
agree through the renaming. -/
theorem pickRows_rename (inj : InjOn f D) (ev ev' : Store U → Expr → Res U) (t : TableDef)
    {σA σB : Store U} (h : Agree f D σA σB) (rows : List (Row U)) :
    ∀ p : Pick, Sub p.vars D → (∀ e ∈ p.exprs, ∀ ρ, ev' ρ (e.rename f) = ev ρ e) →
      BindRel f D (pickRows ev t σA rows p) (pickRows ev' t σB rows (p.rename f))
  | .row n sets, hs, he => by
    have h1 : (sets.map fun p => (f p.1, p.2.rename f)).map (·.1) = (sets.map (·.1)).map f := by
      simp [List.map_map, Function.comp_def]
    have h2 : (sets.map fun p => (f p.1, p.2.rename f)).map (·.2) =
        (sets.map (·.2)).map (Expr.rename f) := by
      simp [List.map_map, Function.comp_def]
    have h3 : (sets.map fun p => (f p.1, p.2.rename f)).map (fun _ => (Val.null : Val U)) =
        sets.map (fun _ => (Val.null : Val U)) := by
      simp [List.map_map, Function.comp_def]
    have hx : Sub (sets.map (·.1)) D := fun y hy => hs y (by
      obtain ⟨q, hq, rfl⟩ := List.mem_map.mp hy
      simp only [Pick.vars, List.mem_flatten, List.mem_map]
      exact ⟨_, ⟨q, hq, rfl⟩, by simp⟩)
    simp only [pickRows, Pick.rename, h1, h2, h3]
    cases rows.getLast? with
    | none =>
      cases n
      · exact h
      · exact Agree.setAll inj _ _ _ _ hx h
    | some r =>
      dsimp only
      rw [evalVals_map (ev := ev) (ev' := ev') (g := Expr.rename f) _ _
        (fun e he' ρ => he e (by simpa [Pick.exprs] using he') ρ)]
      cases evalVals ev (rowStore t.cols r) (sets.map (·.2)) with
      | error k => exact rfl
      | ok vs => exact Agree.setAll inj _ vs _ _ hx h
  | .count x, hs, _ => by
    simp only [pickRows, Pick.rename]
    cases (toInt32 rows.length : Res U) with
    | err k => exact rfl
    | ok v => exact h.set inj (hs x (by simp [Pick.vars])) v
  | .any x, hs, _ => by
    simp only [pickRows, Pick.rename]
    exact h.set inj (hs x (by simp [Pick.vars])) _

/-! ## The lock-step simulation -/

/-- Related states: stores agree through `f` on `D`; same world, same
database, same calls. -/
def StRel (f : Var → Var) (D : List Var) (a b : State U W) : Prop :=
  Agree f D a.store b.store ∧ a.world = b.world ∧ a.db = b.db ∧ a.calls = b.calls

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
    obtain ⟨hst, hw, hdb, hc⟩ := h
    simp only [exec, Stmt.rename]
    rw [Expr.eval_rename es hst e (fun y hy => hs y (by simp [Stmt.vars, hy]))]
    cases Expr.eval es a.store nullStore e with
    | err k => exact ⟨rfl, hst, hw, hdb, hc⟩
    | ok v => exact ⟨hst.set inj (hs x (by simp [Stmt.vars])) v, hw, hdb, hc⟩
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
    cases Expr.eval es a.store nullStore c with
    | err k => exact ⟨rfl, h⟩
    | ok v =>
      dsimp only
      split
      · exact exec_rename es wm inj n s a b (fun y hy => hs y (by simp [Stmt.vars, hy])) h
      · exact exec_rename es wm inj n t a b (fun y hy => hs y (by simp [Stmt.vars, hy])) h
  | n + 1, .while c body, a, b, hs, h => by
    simp only [exec, Stmt.rename]
    rw [Expr.eval_rename es h.1 c (fun y hy => hs y (by simp [Stmt.vars, hy]))]
    cases Expr.eval es a.store nullStore c with
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
    cases Expr.eval es a.store nullStore e with
    | err k => exact ⟨rfl, h⟩
    | ok v => exact ⟨rfl, h⟩
  | _ + 1, .call p args, a, b, hs, h => by
    obtain ⟨hst, hw, hdb, hc⟩ := h
    have hargs : Sub (args.map Arg.vars).flatten D := fun y hy => hs y (by simpa [Stmt.vars] using hy)
    simp only [exec, Stmt.rename]
    rw [args_eval_rename es hst args hargs]
    cases Arg.evalAll es a.store args with
    | error k => exact ⟨rfl, hst, hw, hdb, hc⟩
    | ok vs =>
      dsimp only
      rw [hw, hdb]
      generalize wm.call p vs b.world b.db = res
      obtain ⟨w', db', rs⟩ := res
      simp only [OutRel, StRel]
      refine ⟨Agree.setRefs inj args _ _ _ hargs hst, ?_⟩
      simp [hc]
  | _ + 1, .insert t cs rows, a, b, hs, h => by
    obtain ⟨hst, hw, hdb, hc⟩ := h
    simp only [exec, Stmt.rename]
    rw [insertRows_map (ev := fun ρ e => e.eval es a.store ρ) t cs rows _
      (fun es' hes' e he _ => Expr.eval_rename es hst e (fun y hy => hs y (by
        simp only [Stmt.vars, List.mem_flatten, List.mem_map]
        exact ⟨_, ⟨es', hes', rfl⟩, List.mem_flatten.mpr ⟨e.vars, List.mem_map.mpr ⟨e, he, rfl⟩, hy⟩⟩))),
      hdb]
    cases insertRows (fun ρ e => e.eval es a.store ρ) t cs rows (b.db.get t.name) with
    | error k => exact ⟨rfl, hst, hw, hdb, hc⟩
    | ok rs => exact ⟨hst, hw, rfl, hc⟩
  | _ + 1, .update t sets c, a, b, hs, h => by
    obtain ⟨hst, hw, hdb, hc⟩ := h
    simp only [exec, Stmt.rename]
    rw [updateRows_map (ev := fun ρ e => e.eval es a.store ρ) t sets c
      (fun p hp _ => Expr.eval_rename es hst p.2 (fun y hy => hs y (by
        simp only [Stmt.vars, List.mem_append, List.mem_flatten, List.mem_map]
        exact Or.inl ⟨_, ⟨p, hp, rfl⟩, hy⟩)))
      (fun e he _ => Expr.eval_rename es hst e (fun y hy => hs y (by
        simp only [Stmt.vars, List.mem_append, he, Expr.varsOpt]
        exact Or.inr hy))), hdb]
    cases updateRows (fun ρ e => e.eval es a.store ρ) t sets c (b.db.get t.name) with
    | error k => exact ⟨rfl, hst, hw, hdb, hc⟩
    | ok rs => exact ⟨hst, hw, rfl, hc⟩
  | _ + 1, .delete t c, a, b, hs, h => by
    obtain ⟨hst, hw, hdb, hc⟩ := h
    simp only [exec, Stmt.rename]
    rw [deleteRows_map (ev := fun ρ e => e.eval es a.store ρ) t c
      (fun e he _ => Expr.eval_rename es hst e (fun y hy => hs y (by
        simp only [Stmt.vars, he, Expr.varsOpt]
        exact hy))), hdb]
    cases deleteRows (fun ρ e => e.eval es a.store ρ) t c (b.db.get t.name) with
    | error k => exact ⟨rfl, hst, hw, hdb, hc⟩
    | ok rs => exact ⟨hst, hw, rfl, hc⟩
  | _ + 1, .raise _, _, _, _, h => ⟨rfl, h⟩
  | _ + 1, .reraise, _, _, _, h => ⟨rfl, h⟩
  | _ + 1, .commit, _, _, _, h => h
  | _ + 1, .query t c p, a, b, hs, h => by
    obtain ⟨hst, hw, hdb, hc⟩ := h
    simp only [exec, Stmt.rename]
    rw [keptRows_map (ev := fun ρ e => e.eval es a.store ρ) t c
      (fun e he _ => Expr.eval_rename es hst e (fun y hy => hs y (by
        simp only [Stmt.vars, he, Expr.varsOpt, List.mem_append]
        exact Or.inl hy))), hdb]
    cases keptRows (fun ρ e => e.eval es a.store ρ) t c (b.db.get t.name) with
    | error k => exact ⟨rfl, hst, hw, hdb, hc⟩
    | ok rows =>
      dsimp only
      have hp := pickRows_rename inj (fun ρ e => e.eval es a.store ρ)
        (fun ρ e => e.eval es b.store ρ) t hst rows p
        (fun y hy => hs y (by simp [Stmt.vars, hy]))
        (fun e he ρ => Expr.eval_rename es hst e (fun y hy => hs y (by
          simp only [Stmt.vars, List.mem_append]
          refine Or.inr ?_
          cases p with
          | row n sets =>
            obtain ⟨x, hq⟩ : ∃ x, (x, e) ∈ sets := by simpa [Pick.exprs] using he
            simp only [Pick.vars, List.mem_flatten, List.mem_map]
            exact ⟨_, ⟨(x, e), hq, rfl⟩, by simp [hy]⟩
          | count _ => simp [Pick.exprs] at he
          | any _ => simp [Pick.exprs] at he)))
      generalize pickRows (fun ρ e => e.eval es a.store ρ) t a.store rows p = rA at hp ⊢
      generalize pickRows (fun ρ e => e.eval es b.store ρ) t b.store rows (p.rename f) = rB at hp ⊢
      cases rA <;> cases rB <;> simp only [BindRel] at hp
      · exact ⟨hp, hst, hw, hdb, hc⟩
      · exact ⟨hp, hw, rfl, hc⟩
  | n + 1, .tryCatch u body hd, a, b, hs, h => by
    have ih := exec_rename es wm inj n body a b (fun y hy => hs y (by simp [Stmt.vars, hy])) h
    simp only [exec, Stmt.rename]
    generalize exec es wm n body a = oA at ih ⊢
    generalize exec es wm n (body.rename f) b = oB at ih ⊢
    cases oA <;> cases oB <;> simp only [OutRel] at ih ⊢
    all_goals first | exact ih | exact ih.elim | skip
    obtain ⟨rfl, hst⟩ := ih
    rw [← Stmt.rename_bindErr]
    refine exec_rename es wm inj n _ _ _ (fun y hy => hs y (by
      simp only [Stmt.vars, List.mem_append]; exact Or.inr (by rwa [Stmt.vars_bindErr] at hy))) ?_
    cases u
    · exact hst
    · exact ⟨hst.1, hst.2.1, h.2.2.1, hst.2.2.2⟩

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
    (inj : InjOn f r.vars) (args : List (Val U)) (w : W) (db : Db U) (n : Nat) :
    (r.rename f).obs es wm args w db n = r.obs es wm args w db n := by
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
    have hsim := exec_rename es wm inj n r.body ⟨σA, w, db, []⟩ ⟨σB, w, db, []⟩ hb
      ⟨hbind, rfl, rfl, rfl⟩
    dsimp only
    generalize exec es wm n r.body _ = oA at hsim ⊢
    generalize exec es wm n (r.body.rename f) _ = oB at hsim ⊢
    cases oA <;> cases oB <;> simp only [OutRel] at hsim
    · obtain ⟨hst, hw, hdb, hc⟩ := hsim
      simp [observe, outVals_rename hst r.params hp, hw, hdb, hc]
    · obtain ⟨hv, hst, hw, hdb, hc⟩ := hsim
      simp [observe, outVals_rename hst r.params hp, hv, hw, hdb, hc]
    · obtain ⟨hk, _, _, _, hc⟩ := hsim
      simp [observe, hk, hc]
    · obtain ⟨_, hw, _, hc⟩ := hsim
      simp [observe, hw, hc]

/-- An injective renaming of a routine refines it (and, by the same
proof, is refined by it). -/
theorem refines_rename (es : ExprSem U) (wm : WorldModel U W) (r : Routine)
    (inj : InjOn f r.vars) : Refines es wm r (r.rename f) := by
  intro args w db
  refine ⟨fun n => ⟨n, obs_rename es wm r inj args w db n⟩, fun hd n => ?_⟩
  rw [← obs_rename es wm r inj args w db n]
  exact hd n

end

end Sqldiff.Core
