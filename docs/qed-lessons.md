# What SQLDiff takes from the QED prover

Status: analysis, nothing below is implemented. Written 2026-10-28 from the
fork `davysc/qed-prover` at `9e9c262`.

QED (Wang et al., VLDB 2024) decides equivalence of two SQL queries. It is
the strongest open implementation of the U-semiring approach (UDP, Chu et
al. 2018), and it is close to what phase 2 of the
[verification roadmap](verification-roadmap.md) (queries) needs. This note
says which ideas carry over into the Lean gate, which do not, and why QED
itself can never be part of the trusted base.

## How QED works

Input is a JSON pair of relational-algebra plans (Calcite's), plus table
schemas: column types, nullability, keys, `CHECK` constraints
(`src/pipeline/relation.rs`). Output is "provable" or "not provable".

| Stage | File | What it does | Exit when equal |
|---|---|---|---|
| Translate | `relation.rs` | Each plan becomes a U-semiring expression: a relation is a function from tuples to multiplicities, `Filter` is `[P] × R(x)`, `Project` is `∑ x. [a = f(x)] × R(x)`, `DISTINCT` is squash `‖R‖`, `EXCEPT` and anti joins use `¬` | Plans equal |
| Partial, normal | `partial.rs`, `normal.rs` | Normalisation by evaluation to sum-product normal form (a sum of `∑ vars. preds × squash × not × table apps`) | Normal forms equal |
| Stable | `stable.rs` | Drops summation variables that equalities fix (`∑ x. [x = f(y)] × …` becomes a substitution), and summations that are redundant under the solver | Stable forms equal |
| Unify | `unify.rs` | Matches terms one to one, trying up to 24 permutations of bound variables per summation, and asks Z3 and CVC5 (raced, either may answer) whether predicates and table applications agree | SMT says `unsat` |

Keys become uninterpreted functions: for a key `k` of table `t`, every other
column is `rpn!t-i(k)`, so two rows with the same key are the same row.
Non-null columns and `CHECK` constraints are conjoined at every scan.
Aggregates, `ORDER BY` and `LIMIT` are uninterpreted operators over the
normalised argument relation: equal when their arguments are.

The Agda files (`agda/`) are an intrinsically typed mirror of the stages,
so they show the stages are well scoped. They contain no soundness theorem,
use `postulate` and `{-# TERMINATING #-}`, and are not linked to the Rust.

## Why QED cannot gate a SQLDiff "equal"

Its "provable" rests on the Rust pipeline, two SMT solvers and a
text-rewriting step between them, with no certificate. Found while reading
it:

| Issue | Where | Effect |
|---|---|---|
| The SMT-LIB text is edited with plain string replacement (`" and"` to `" true"`, `" or"` to `" false"`, ...) before it goes to the solvers, string literals included | `unify.rs`, `smt` | **A false "provable".** `x = 'rock and roll'` against `x = 'rock true roll'`: honest Z3 says the negated goal is `sat`; after QED's rewrite both literals read `"rock true roll"` and Z3 says `unsat`. Reproduced with the Python Z3 bindings and the same replacements, 2026-10-28 |
| `REAL` literals are parsed as `f32` | `normal.rs`, `parse` | Distinct literals that round to the same `f32` compare equal |
| Integers are unbounded, decimals are mathematical reals | `null.rs` | No overflow, no `numeric(p,s)` rounding |
| Strings are Z3 strings | `normal.rs` | No collation, no case-insensitive comparison, no T-SQL trailing-space padding (`'a' = 'a '` is true in T-SQL) |
| No errors | everywhere | Division by zero, conversion errors and a scalar subquery returning two rows are not observations |
| One dialect (Calcite's), queries only, equivalence only | | No DML, routines, transactions, cross-dialect correspondence, or refinement (decision 1) |
| Nightly Rust, a forked `z3` crate, solvers spawned per query | `Cargo.toml`, `unify.rs` | Not shippable on the four release targets |

Each row is a mismatch with decision 1 or a plain bug; none of it is
specific to QED, it is what an SMT-backed checker without a semantics looks
like. QED is MIT licensed, so code may be taken, but the plan (decision 2)
keeps the Rust and any solver out of the trusted base, so what carries over
is ideas and test data.

## What to take

### 1. The U-semiring as the Lean query semantics (phase 2)

A bag is `Tuple → ℕ`; the operators are `+`, `×`, `∑` over a finite
database, squash and not. Every phase 2 construct that QED covers has a
one-line meaning in it (the comments in `relation.rs` are a ready list):
joins of every kind, semi and anti joins, `UNION ALL`, `UNION`,
`INTERSECT`, `EXCEPT ALL`, `DISTINCT`, `VALUES`, `IN` and `EXISTS`
subqueries, lateral joins. Proposed for `Spec/Query.lean`:

- `Rel σ := Tuple σ → ℕ`, with `‖r‖ t := if r t = 0 then 0 else 1` and
  `¬r t := if r t = 0 then 1 else 0`. Over finite databases `ℕ` suffices;
  QED's infinite sums are not needed.
- The SQL-specific parts stay explicit and per dialect version, as decision
  1 requires: predicates are three-valued and `WHERE` keeps only `true`;
  comparison goes through the dialect's collation; expression errors make
  the run an error observation, so `Run` for a query is a bag or an error,
  never a bag alone.
- `ORDER BY` is not a bag operator. A totally ordered result is a list; an
  unordered one is a bag; `TOP`/`LIMIT` without a total order is A's set of
  allowed bags (decision 1, unspecified behaviour). QED's uninterpreted
  treatment is sound for equivalence but cannot express "B orders what A
  leaves unordered".

### 2. Decide by normalisation, prove the normaliser once

QED's design makes the check a normal-form comparison. In Lean that
becomes:

```lean
def norm : Query → NF                      -- sum-product normal form
theorem norm_sound : ⟦norm q⟧ = ⟦q⟧          -- once, for the fragment
def check (a b : Query) : Answer :=
  if nfEqUpTo (norm a) (norm b) then .confirmed "Query.check_sound" else ...
```

`nfEqUpTo` is equality up to commutativity of `+` and `×` and a renaming of
bound variables; QED's 24-permutation search is the part to replace. The
Rust side already sends an alignment hint (decision 2); it can also send
the variable renaming per summation, which Lean checks instead of
searching. One theorem then covers join reordering, predicate pushdown,
subquery-to-join, `IN` to semi join and projection merging, rules
`canon.rs` handles one by one today.

### 3. Catalog facts as hypotheses

QED's encodings of keys (functional dependency through an uninterpreted
function), `NOT NULL` and `CHECK` at each scan are the rewrites phase 2
needs for "catalog keys and nullability". In Lean they are hypotheses on
the environment from Γ (`∀ db, Γ.holds db → …`), used by lemmas such as
`‖R‖ = R` for a table with a key, and `∑ y. [y = f(k)] × …` elimination
(QED's `stable.rs`). The squash-of-keyed-table lemma is what proves
`DISTINCT` over a key redundant, a common edit in real code.

### 4. Staged early exits

QED stops at the first stage where both sides agree (`pipeline.rs`). The
gate's budgets (decision 2) want the same: raw AST equality (today's
trivial fragment), then lowered terms, then normal forms. Each stage is a
separate theorem with its own `confirmed` name, so coverage reports show
which stage confirmed each verdict.

### 5. Three-valued predicates over subqueries

`normal.rs` `quant_cmp` spells out `ANY`/`ALL`/`IN` with NULLs (true if a
match exists, else unknown if a NULL comparison exists, else false). It is
a useful first draft for phase 1 and 2 spec rules, to be checked against
vendor text and the engines like any spec rule (decision 5), not copied.

### 6. Test data

`tests/calcite` holds 444 rewrite pairs from Calcite's optimiser tests
(QED proves about 290) and `tests/cockroach` CockroachDB's normalisation
rules, as JSON plans with schemas. A small importer from this format into
the Lean query AST gives phase 2 an external benchmark with a known
baseline: every pair is a rewrite an optimiser considers correct, so a
Lean "not equal" on one is either a gap or a finding. They are not SQL
text and not T-SQL or PostgreSQL, so they test the normaliser, not the
parser or the dialect spec.

## What not to take

- **SMT in the trusted path.** If a solver is used, it is an untrusted
  search (for a renaming or an alignment) whose answer Lean checks. Small
  arithmetic side conditions go to `omega`, `decide` or `bv_decide` in
  Lean.
- **The Rust code.** It needs nightly Rust, a forked `z3` crate and shared
  `Rc<RefCell>` state, and would add a second lowering to keep in sync. The
  algorithms are short enough to write again in Lean, mirroring `canon.rs`
  as the convergence plan requires.
- **Uninterpreted aggregates as the end state.** They prove only `f(R) =
  f(R')` for equal `R`, `R'`. SQLDiff's real edits change aggregates
  (`COUNT(*)` against `COUNT(col)`, `SUM` on an empty input is NULL, not 0),
  so aggregates need their meaning in the spec; congruence is a fallback.

## Leads for the fork

- The string-replacement bug above makes QED report false equivalences for
  string literals containing ` and` or ` or`. A fix builds the empty
  `and`/`or` cases as `true`/`false` terms before dumping, instead of
  editing text. Worth reporting upstream.
