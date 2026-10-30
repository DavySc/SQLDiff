/-
(C3d) What the key closure gives: a query block whose every source is
determined (`Block.atMostOne`) keeps at most one tuple on a database
that satisfies the catalog (`kept_length_le_one`).

Why it matters. T-SQL `SELECT @x = e FROM ...` assigns from one of the
rows the query returns, which one neither vendor specifies, and the
converter's PL/pgSQL `FOR` loop assigns from the last it iterates; the
core takes the last tuple in list order (`Pick.row false`). The
lowerings build that pick only on a closed block, so on every database
an engine can be in (one where every table's rows have distinct primary
keys, `ASM-TABLES-AS-DECLARED`) there is at most one tuple to take and
the core's choice is not a claim about either engine. This file proves
the "at most one". The core's `Refines` still quantifies over every list
of rows, including ones that break a key; there the core's choice is the
same on both sides, which is all the mirror check needs.

The argument. Two kept tuples agree on every source, by induction along
the closure's order: a source is determined when each of its key
columns `k` is equated (`COp.eq`, which is TRUE only for two equal
values of one kind) with an expression that reads only columns of
sources already shown to agree, so both tuples give the key column the
same value; the source's rows then have the same key, and a table whose
keys are distinct has one row with that key. The product of lists
without repetitions has none, and a list without repetitions whose
elements are all equal has at most one.
-/
import Sqldiff.Core.Semantics

namespace Sqldiff.Core

variable {U : Type}

/-! ## Databases that satisfy the catalog -/

/-- The rows of table `t` satisfy its definition: each row has one value
per column, no required column (`NOT NULL` or key) is NULL, each row has
a key of integers, dates or timestamps (`keyOf`), and, when the table has
a key (`TableDef.hasKey`), no two rows have the same key. A table without
a key may hold equal rows (`keyOf` is `some []` for every row), so the
distinctness is asked only of a keyed one. Both engines keep every table
in such a state at every statement boundary (the primary key is not
deferrable in PostgreSQL, and checked on every write in SQL Server;
`ASM-TABLES-AS-DECLARED`). -/
def TableOk (t : TableDef) (rs : List (Row U)) : Prop :=
  (∀ r ∈ rs, r.length = t.cols.length ∧ rowOk t.cols r = true ∧ (keyOf t.cols r).isSome = true) ∧
    (t.hasKey = true → rs.Pairwise fun r r' => keyOf t.cols r ≠ keyOf t.cols r')

theorem pairwise_eq_of {α β : Type} {f : α → β} :
    ∀ {l : List α}, l.Pairwise (fun a b => f a ≠ f b) →
      ∀ {a b : α}, a ∈ l → b ∈ l → f a = f b → a = b
  | [], _, _, _, ha, _, _ => by simp at ha
  | x :: xs, h, a, b, ha, hb, he => by
    rw [List.pairwise_cons] at h
    rcases List.mem_cons.mp ha with ha' | ha' <;> rcases List.mem_cons.mp hb with hb' | hb'
    · rw [ha', hb']
    · subst ha'; exact absurd he (h.1 _ hb')
    · subst hb'; exact absurd he.symm (h.1 _ ha')
    · exact pairwise_eq_of h.2 ha' hb' he

theorem TableOk.eq_of_key {t : TableDef} {rs : List (Row U)} (h : TableOk t rs)
    (hkey : t.hasKey = true)
    {r r' : Row U} (hr : r ∈ rs) (hr' : r' ∈ rs) (hk : keyOf t.cols r = keyOf t.cols r') :
    r = r' :=
  pairwise_eq_of (h.2 hkey) hr hr' hk

theorem TableOk.nodup {t : TableDef} {rs : List (Row U)} (h : TableOk t rs)
    (hkey : t.hasKey = true) : rs.Nodup :=
  (h.2 hkey).imp fun hk e => hk (by rw [e])

/-! ## The product -/

theorem mem_tuples {α : Type} :
    ∀ (Ls : List (List α)) (T : List α), T ∈ tuples Ls →
      T.length = Ls.length ∧ ∀ (k : Nat) (r : α), T[k]? = some r → ∃ rs, Ls[k]? = some rs ∧ r ∈ rs
  | [], T, h => by
    simp only [tuples, List.mem_singleton] at h
    subst h
    exact ⟨rfl, fun k r hk => by simp at hk⟩
  | rs :: Ls, T, h => by
    simp only [tuples, List.mem_flatMap, List.mem_map] at h
    obtain ⟨r, hr, T₀, hT₀, rfl⟩ := h
    obtain ⟨hl, hm⟩ := mem_tuples Ls T₀ hT₀
    refine ⟨by simp [hl], fun k r' hk => ?_⟩
    cases k with
    | zero => simp at hk; subst hk; exact ⟨rs, rfl, hr⟩
    | succ k => simpa using hm k r' (by simpa using hk)

theorem tuples_nodup {α : Type} :
    ∀ (Ls : List (List α)), (∀ rs ∈ Ls, rs.Nodup) → (tuples Ls).Nodup
  | [], _ => by simp [tuples]
  | rs :: Ls, h => by
    have ih := tuples_nodup Ls (fun rs' h' => h rs' (by simp [h']))
    have hrs : rs.Nodup := h rs (by simp)
    simp only [tuples, List.Nodup]
    rw [List.pairwise_flatMap]
    refine ⟨fun r _ => ?_, ?_⟩
    · rw [List.pairwise_map]
      exact ih.imp fun hne e => hne (List.cons.inj e).2
    · refine hrs.imp fun hne => ?_
      intro x hx y hy e
      obtain ⟨_, _, rfl⟩ := List.mem_map.mp hx
      obtain ⟨_, _, rfl⟩ := List.mem_map.mp hy
      exact hne (List.cons.inj e).1

/-! ## What a kept tuple satisfies -/

theorem keepsAll_true {ev : Store U → Expr → Res U} {ρ : Store U} :
    ∀ (cs : List Expr), keepsAll ev ρ cs = .ok true →
      ∀ c ∈ cs, ∃ v, ev ρ c = .ok v ∧ isTrue v = true
  | [], _ => by simp
  | c :: cs, h => by
    simp only [keepsAll] at h
    cases hc : ev ρ c with
    | err k => rw [hc] at h; cases h
    | ok v =>
      rw [hc] at h
      dsimp only at h
      cases hr : keepsAll ev ρ cs with
      | error k => rw [hr] at h; cases h
      | ok b =>
        rw [hr] at h
        simp only [Except.ok.injEq, Bool.and_eq_true] at h
        intro c' hc'
        rcases List.mem_cons.mp hc' with rfl | hc'
        · exact ⟨v, hc, h.1⟩
        · exact keepsAll_true cs (by rw [hr, h.2]) c' hc'

theorem keptTuples_sub {ev : Store U → Expr → Res U} {b : Block} :
    ∀ (Ts rows : List (List (Row U))), keptTuples ev b Ts = .ok rows →
      rows.Sublist Ts ∧ ∀ T ∈ rows, keepsAll ev (tupleStore 0 b.srcs T) b.conds = .ok true
  | [], rows, h => by
    simp only [keptTuples, Except.ok.injEq] at h
    subst h
    exact ⟨List.Sublist.slnil, by simp⟩
  | T :: Ts, rows, h => by
    simp only [keptTuples] at h
    cases hk : keepsAll ev (tupleStore 0 b.srcs T) b.conds with
    | error e => rw [hk] at h; cases h
    | ok k =>
      rw [hk] at h
      dsimp only at h
      cases hr : keptTuples ev b Ts with
      | error e => rw [hr] at h; cases h
      | ok Ts' =>
        rw [hr] at h
        simp only [Except.ok.injEq] at h
        obtain ⟨hs, hm⟩ := keptTuples_sub Ts Ts' hr
        cases k
        · simp only [Bool.false_eq_true, ↓reduceIte] at h
          subst h
          exact ⟨hs.cons _, hm⟩
        · simp only [↓reduceIte] at h
          subst h
          refine ⟨hs.cons_cons _, fun T' hT' => ?_⟩
          rcases List.mem_cons.mp hT' with rfl | hT'
          · exact hk
          · exact hm T' hT'

/-! ## The tuple's columns -/

/-- Two tuples that have the same row at the source that owns `n` give
`n` the same value. -/
theorem tupleStore_congr (n : Col) :
    ∀ (ts : List TableDef) (i : Nat) (T T' : List (Row U)),
      T.length = ts.length → T'.length = ts.length →
      (∀ k, ownerFrom i ts n = some k → T[k]? = T'[k]?) →
      tupleStore i ts T n = tupleStore i ts T' n
  | [], _, _, _, _, _, _ => by simp [tupleStore, nullStore]
  | _ :: _, _, [], _, h, _, _ => by simp at h
  | _ :: _, _, _ :: _, [], _, h, _ => by simp at h
  | t :: ts, i, r :: R, r' :: R', hl, hl', h => by
    simp only [tupleStore]
    split
    · next hn =>
      have := h 0 (by simp [ownerFrom, hn])
      simp only [List.getElem?_cons_zero, Option.some.injEq] at this
      rw [this]
    · next hn =>
      refine tupleStore_congr n ts (i + 1) R R' (by simpa using hl) (by simpa using hl')
        (fun k hk => ?_)
      have := h (k + 1) (by simp [ownerFrom, hn, hk])
      simpa using this

/-- The value of a name is the value of its owner's row there. -/
theorem tupleStore_owner (n : Col) :
    ∀ (ts : List TableDef) (i : Nat) (T : List (Row U)) (k : Nat),
      T.length = ts.length → ownerFrom i ts n = some k →
      ∃ t r, ts[k]? = some t ∧ T[k]? = some r ∧ tupleStore i ts T n = rowStore (srcCols (i + k) t) r n
  | [], _, _, _, _, h => by simp [ownerFrom] at h
  | _ :: _, _, [], _, hl, _ => by simp at hl
  | t :: ts, i, r :: R, k, hl, h => by
    simp only [ownerFrom] at h
    simp only [tupleStore]
    split at h
    · next hn =>
      simp only [Option.some.injEq] at h
      subst h
      exact ⟨t, r, rfl, rfl, by simp [hn]⟩
    · next hn =>
      obtain ⟨k', hk', rfl⟩ := Option.map_eq_some_iff.mp h
      obtain ⟨t', r', h1, h2, h3⟩ := tupleStore_owner n ts (i + 1) R k' (by simpa using hl) hk'
      refine ⟨t', r', by simpa using h1, by simpa using h2, ?_⟩
      simp only [hn, Bool.false_eq_true, ↓reduceIte, h3]
      congr 2
      omega

/-! ## Key columns -/

/-- The row store of a table's columns renamed by `g`: column `g c`
reads the value of the first column named `c`. With distinct names and
rows of full length, two rows that agree on every key column's renamed
name have the same key columns. -/
theorem keyFilter_eq (g : Col → Col) :
    ∀ (ds : List ColDef) (r r' : Row U),
      r.length = ds.length → r'.length = ds.length →
      ((ds.map fun d => { d with name := g d.name }).map (·.name)).Nodup →
      (∀ d ∈ ds, d.key = true →
        rowStore (ds.map fun d => { d with name := g d.name }) r (g d.name) =
          rowStore (ds.map fun d => { d with name := g d.name }) r' (g d.name)) →
      (ds.zip r).filter (·.1.key) = (ds.zip r').filter (·.1.key)
  | [], _, _, _, _, _, _ => by simp
  | _ :: _, [], _, h, _, _, _ => by simp at h
  | _ :: _, _ :: _, [], _, h, _, _ => by simp at h
  | d :: ds, v :: r, v' :: r', hl, hl', hn, h => by
    simp only [List.map_cons, List.nodup_cons, List.mem_map] at hn
    have ih := keyFilter_eq g ds r r' (by simpa using hl) (by simpa using hl') hn.2 (by
      intro d₂ hd₂ hk
      have := h d₂ (by simp [hd₂]) hk
      have hne : g d₂.name ≠ g d.name := by
        intro e
        exact hn.1 ⟨{ d₂ with name := g d₂.name }, ⟨d₂, hd₂, rfl⟩, by simp [e]⟩
      simpa [rowStore, hne] using this)
    simp only [List.zip_cons_cons, List.filter_cons]
    split
    · next hk =>
      have hv := h d (by simp) hk
      simp only [List.map_cons, rowStore, ↓reduceIte] at hv
      rw [hv, ih]
    · exact ih

/-- Two rows of a table that agree on the block's names of its key
columns have the same key. -/
theorem keyOf_eq (i : Nat) (t : TableDef) (r r' : Row U)
    (hl : r.length = t.cols.length) (hl' : r'.length = t.cols.length)
    (hn : ((srcCols i t).map (·.name)).Nodup)
    (h : ∀ d ∈ t.cols, d.key = true →
      rowStore (srcCols i t) r (srcCol i d.name) = rowStore (srcCols i t) r' (srcCol i d.name)) :
    keyOf t.cols r = keyOf t.cols r' := by
  unfold keyOf
  rw [keyFilter_eq (srcCol i) t.cols r r' hl hl' hn h]

/-! ## Expressions read only their columns -/

theorem SExpr.eval_congr (σ : Store U) {ρ ρ' : Store U} :
    ∀ s : SExpr, (∀ m ∈ s.cols, ρ m = ρ' m) → s.eval σ ρ = s.eval σ ρ' := by
  intro s
  induction s <;> intro h <;> simp only [SExpr.cols, List.mem_append, List.mem_singleton] at h <;>
    simp only [SExpr.eval] <;> simp_all

theorem isTrue_bool (b : Bool) : isTrue (Val.bool b : Val U) = b := by cases b <;> rfl

/-- The core's `=` is TRUE only for two equal values. -/
theorem cmpVal_eq_true {v w : Val U} {r : Val U} (h : cmpVal .eq v w = .ok r)
    (ht : isTrue r = true) : v = w := by
  cases v <;> cases w <;> simp [cmpVal, cmpInt, typeErr] at h <;> subst h <;>
    (try simp only [isTrue_bool, beq_iff_eq] at ht) <;> simp_all [isTrue]

/-! ## At most one tuple -/

section
variable (es : ExprSem U) (σ : Store U) (b : Block)

/-- A condition stating `n = e` (`eqFixes`) that is TRUE on a tuple gives
`n` the value of `e` there. -/
theorem fix_value {ρ : Store U} {c : Expr} {n : Col} {e : SExpr} (hf : (n, e) ∈ eqFixes c)
    {v : Val U} (hv : c.eval es σ ρ = .ok v) (ht : isTrue v = true) :
    e.eval σ ρ = .ok (ρ n) := by
  unfold eqFixes at hf
  split at hf
  · next a b' =>
    simp only [Expr.eval, SExpr.eval] at hv
    simp only [List.mem_append] at hf
    rcases hf with hf | hf
    · split at hf
      · next n' =>
        simp only [List.mem_singleton, Prod.mk.injEq] at hf
        obtain ⟨rfl, rfl⟩ := hf
        simp only [SExpr.eval] at hv
        unfold bind2 at hv
        cases he : e.eval σ ρ with
        | err k => rw [he] at hv; cases hv
        | ok w =>
          rw [he] at hv
          exact congrArg Res.ok (cmpVal_eq_true hv ht).symm
      · simp at hf
    · split at hf
      · next n' =>
        simp only [List.mem_singleton, Prod.mk.injEq] at hf
        obtain ⟨rfl, rfl⟩ := hf
        simp only [SExpr.eval] at hv
        unfold bind2 at hv
        cases he : e.eval σ ρ with
        | err k => rw [he] at hv; cases hv
        | ok w =>
          rw [he] at hv
          exact congrArg Res.ok (cmpVal_eq_true hv ht)
      · simp at hf
  · simp at hf

/-- Along the chain, two kept tuples agree on every source taken so far. -/
theorem chain_agree (db : Db U) (hdb : ∀ t ∈ b.srcs, TableOk t (db.get t.name))
    {T T' : List (Row U)} (hT : T ∈ tuples (b.srcs.map fun t => db.get t.name))
    (hT' : T' ∈ tuples (b.srcs.map fun t => db.get t.name))
    (kT : keepsAll (fun ρ e => e.eval es σ ρ) (tupleStore 0 b.srcs T) b.conds = .ok true)
    (kT' : keepsAll (fun ρ e => e.eval es σ ρ) (tupleStore 0 b.srcs T') b.conds = .ok true) :
    ∀ (is done : List Nat), b.chain done is = true → (∀ j ∈ done, T[j]? = T'[j]?) →
      ∀ j ∈ is ++ done, T[j]? = T'[j]? := by
  obtain ⟨hl, hm⟩ := mem_tuples _ T hT
  obtain ⟨hl', hm'⟩ := mem_tuples _ T' hT'
  simp only [List.length_map] at hl hl'
  intro is
  induction is with
  | nil => intro done _ hd j hj; exact hd j (by simpa using hj)
  | cons i is ih =>
    intro done hc hd
    simp only [Block.chain, Bool.and_eq_true] at hc
    obtain ⟨hi, hc⟩ := hc
    split at hi
    · next t ht =>
      -- Source `i` is determined: both tuples have the same row there.
      simp only [Block.determines, Bool.and_eq_true, decide_eq_true_eq, List.all_eq_true,
        List.any_eq_true] at hi
      obtain ⟨⟨hhk, hnd⟩, hkeys⟩ := hi
      have hsrc : ∀ {X : List (Row U)}, X ∈ tuples (b.srcs.map fun t => db.get t.name) →
          ∃ r, X[i]? = some r ∧ r ∈ db.get t.name := by
        intro X hX
        obtain ⟨hlX, hmX⟩ := mem_tuples _ X hX
        simp only [List.length_map] at hlX
        have hi' : i < X.length := by
          rw [hlX]; exact (List.getElem?_eq_some_iff.mp ht).1
        refine ⟨X[i], List.getElem?_eq_getElem hi', ?_⟩
        obtain ⟨rs, hrs, hr⟩ := hmX i X[i] (List.getElem?_eq_getElem hi')
        simp only [List.getElem?_map, ht, Option.map_some, Option.some.injEq] at hrs
        subst hrs; exact hr
      obtain ⟨r, hr, hrm⟩ := hsrc hT
      obtain ⟨r', hr', hrm'⟩ := hsrc hT'
      have htok := hdb t (List.mem_of_getElem? ht)
      have hagree : ∀ d ∈ t.cols, d.key = true →
          rowStore (srcCols i t) r (srcCol i d.name) =
            rowStore (srcCols i t) r' (srcCol i d.name) := by
        intro d hdc hk
        obtain ⟨c, hc, ⟨n, e⟩, hfix, hcond⟩ := hkeys d (List.mem_filter.mpr ⟨hdc, by simpa using hk⟩)
        simp only [beq_iff_eq] at hcond
        obtain ⟨⟨rfl, hown⟩, hcols⟩ := hcond
        obtain ⟨v, hv, htv⟩ := keepsAll_true _ kT c hc
        obtain ⟨v', hv', htv'⟩ := keepsAll_true _ kT' c hc
        have e1 := fix_value es σ hfix hv htv
        have e2 := fix_value es σ hfix hv' htv'
        -- `e` reads only sources already shown to agree.
        have ee : e.eval σ (tupleStore 0 b.srcs T) = e.eval σ (tupleStore 0 b.srcs T') := by
          refine SExpr.eval_congr σ e (fun m hm => ?_)
          have := hcols m hm
          split at this
          · next j hj =>
            refine tupleStore_congr m b.srcs 0 T T' hl hl' (fun k hk => ?_)
            have : k = j := by
              simp only [Block.owner] at hj
              rw [hj] at hk; exact (Option.some.inj hk).symm
            subst this
            exact hd k (by simpa using ‹done.contains k = true›)
          · cases this
        rw [e1, e2] at ee
        have hv1 := Res.ok.inj ee
        obtain ⟨t1, r1, ht1, hr1, hs1⟩ :=
          tupleStore_owner (srcCol i d.name) b.srcs 0 T i hl (by simpa [Block.owner] using hown)
        obtain ⟨t2, r2, ht2, hr2, hs2⟩ :=
          tupleStore_owner (srcCol i d.name) b.srcs 0 T' i hl' (by simpa [Block.owner] using hown)
        rw [ht] at ht1 ht2
        cases ht1; cases ht2
        rw [hr] at hr1; rw [hr'] at hr2
        cases hr1; cases hr2
        simp only [Nat.zero_add] at hs1 hs2
        rw [← hs1, ← hs2]; exact hv1
      have hkey := keyOf_eq i t r r' (htok.1 r hrm).1 (htok.1 r' hrm').1 hnd hagree
      have hrr := htok.eq_of_key hhk hrm hrm' hkey
      subst hrr
      have hd' : ∀ j ∈ i :: done, T[j]? = T'[j]? := by
        intro j hj
        rcases List.mem_cons.mp hj with rfl | hj
        · rw [hr, hr']
        · exact hd j hj
      intro j hj
      refine ih (i :: done) hc hd' j ?_
      simp only [List.cons_append, List.mem_cons, List.mem_append] at hj ⊢
      rcases hj with h | h | h <;> simp [h]
    · cases hi

/-- Every source the chain goes through is determined, so it has a key. -/
theorem chain_hasKey : ∀ (is done : List Nat), b.chain done is = true →
    ∀ i ∈ is, ∀ t, b.srcs[i]? = some t → t.hasKey = true
  | [], _, _, i, hi, _, _ => by simp at hi
  | j :: is, done, hc, i, hi, t, ht => by
    simp only [Block.chain, Bool.and_eq_true] at hc
    obtain ⟨hj, hc⟩ := hc
    rcases List.mem_cons.mp hi with rfl | hi
    · rw [ht] at hj
      simp only [Block.determines, Bool.and_eq_true] at hj
      exact hj.1.1
    · exact chain_hasKey is (j :: done) hc i hi t ht

/-- **The key closure gives at most one tuple.** On a database where
every source's table satisfies its definition (`TableOk`: distinct
primary keys), a block whose every source is determined
(`Block.atMostOne`) keeps at most one tuple. The lowerings ask it of
every pick that assigns from "the" row without a cardinality error
(`Pick.row false`: T-SQL `SELECT @x = ...` and its PL/pgSQL `FOR` loop,
`Sqldiff.Mirror.rowPick`). -/
theorem kept_length_le_one (db : Db U) (hb : b.atMostOne = true)
    (hdb : ∀ t ∈ b.srcs, TableOk t (db.get t.name)) {rows : List (List (Row U))}
    (h : blockRows (fun ρ e => e.eval es σ ρ) b db = .ok rows) : rows.length ≤ 1 := by
  simp only [Block.atMostOne, Bool.and_eq_true, List.all_eq_true] at hb
  obtain ⟨hc, hcov⟩ := hb
  obtain ⟨hsub, hkeep⟩ := keptTuples_sub _ rows h
  -- Every source is in the closure, so it has a key and its rows are
  -- distinct.
  have hkeyed : ∀ t ∈ b.srcs, t.hasKey = true := by
    intro t ht
    obtain ⟨i, hi, hti⟩ := List.getElem_of_mem ht
    exact chain_hasKey b b.closure [] hc i (by simpa using hcov i (List.mem_range.mpr hi)) t
      (by rw [List.getElem?_eq_getElem hi, hti])
  have hnd : (tuples (b.srcs.map fun t => db.get t.name)).Nodup :=
    tuples_nodup _ (fun rs hrs => by
      obtain ⟨t, ht, rfl⟩ := List.mem_map.mp hrs
      exact (hdb t ht).nodup (hkeyed t ht))
  have hrows : rows.Nodup := hnd.sublist hsub
  have heq : ∀ T ∈ rows, ∀ T' ∈ rows, T = T' := by
    intro T hT T' hT'
    have hTm := hsub.subset hT
    have hTm' := hsub.subset hT'
    have hag := chain_agree es σ b db hdb hTm hTm' (hkeep T hT) (hkeep T' hT') b.closure []
      hc (by simp)
    obtain ⟨hl, _⟩ := mem_tuples _ T hTm
    obtain ⟨hl', _⟩ := mem_tuples _ T' hTm'
    simp only [List.length_map] at hl hl'
    apply List.ext_getElem?
    intro j
    by_cases hj : j < b.srcs.length
    · exact hag j (by simpa using hcov j (List.mem_range.mpr hj))
    · rw [List.getElem?_eq_none (by omega), List.getElem?_eq_none (by omega)]
  match rows, hrows, heq with
  | [], _, _ => simp
  | [_], _, _ => simp
  | x :: y :: _, hn, he =>
    exfalso
    have hxy := he x (by simp) y (by simp)
    exact (List.nodup_cons.mp hn).1 (by simp [hxy])
end

end Sqldiff.Core
