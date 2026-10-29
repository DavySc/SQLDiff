# What SQLDiff takes from the QED prover

Status: analysis, second pass. Written 2026-10-28 from the fork
`davysc/qed-prover` at `9e9c262` and the extended paper
(`extended-paper.pdf` in that repository; Wang, Pan, Cheung, VLDB 2024).
How these findings shape the converter is in
[verified-conversion.md](verified-conversion.md).

QED decides equivalence of two SQL queries. It is the strongest open
implementation of the U-semiring line of work (Cosette and HoTTSQL in Coq,
2017; UDP, 2018; SPES, 2022; QED, 2024), and its query fragment is close to
what the [verification roadmap](verification-roadmap.md) wants for phase 2.
This note says what it does, where its soundness actually rests, which ideas
carry into the Lean work, and which do not.

## How QED works

Input: a JSON pair of relational-algebra plans in Calcite's shape, plus
table schemas (column types, nullability, keys, `CHECK` constraints as
`guaranteed` predicates). Output: "provable" or "not provable". The 444
pairs in `tests/calcite` and the CockroachDB rules in `tests/cockroach` are
its benchmark.

| Stage | Where | What it does | Stops when |
|---|---|---|---|
| Translate | `src/pipeline/relation.rs` | A relation becomes a function from a tuple to its multiplicity. `Filter` is `[P = true] × R(x)`, `Project` is `∑ s. [x = f(s)] × R(s)`, `Join` is a product, `UNION ALL` a sum, `DISTINCT` the squash `‖R‖` (0 stays 0, anything else becomes 1), `EXCEPT` and anti joins use `¬R` (1 when `R` is 0). Keys, `NOT NULL` and `CHECK` are conjoined at every scan | The two plans are equal |
| Partial, normal | `partial.rs`, `normal.rs` | Normalisation by evaluation (`explain.md`) to sum-product normal form: a sum of terms `∑ vars. logic × table applications`, with summations kept as closures so bound variables are numbered once | Normal forms equal |
| Stable | `stable.rs` | Asks Z3 for the equalities the term's own predicates imply (`get_implied_equalities`), then substitutes: `∑ x. [x = f(y)] × …` loses `x`. Drops `LIMIT 1` over a relation proven to hold at most one row, `OFFSET 0`, and whole terms under `LIMIT 0` | Stable forms equal |
| Unify | `unify.rs` | Matches terms one to one, trying up to 24 orderings of each summation's bound variables, and asks Z3 and CVC5 (raced; the first `unsat` wins) whether the term bodies agree, with congruence hypotheses for every pair of uninterpreted operators whose arguments unify | The solver says `unsat` |

Three modelling choices matter for SQLDiff:

- **Values are `Option`-sorted.** Every SMT value is `None` or `Some v`
  (`null.rs`), every operator is lifted (`None` in, `None` out), `AND` and
  `OR` follow SQL's three-valued truth tables, and a filter keeps a row only
  when its predicate is `Some true`. `a IN Q` and `a cmp SOME Q` are spelt
  out with NULL (true if a match exists, else NULL if a NULL comparison
  exists, else false). So NULL in predicates is handled, and handled the way
  SQL does it.
- **Keys are functional dependencies.** For table `t` with key `k`, each
  other column is `f_i(k)` for an uninterpreted `f_i`, and the table is a
  set of keys. Two rows with the same key are the same row, which is what
  makes `DISTINCT` over a key redundant and lets a `LIMIT 1` over a keyed
  lookup disappear.
- **Aggregates, `ORDER BY`, `LIMIT` and unknown functions are
  uninterpreted.** `SUM(x)` over `R` equals `SUM(x)` over `R'` when `R` and
  `R'` are equal bags; nothing else is known about `SUM`.

The paper's theorem is completeness for a fragment (Table, Values, Filter,
Project, Join, Union, with primary keys, over a decidable theory for the
scalar expressions): two queries in it are equal under the U-semiring
semantics if and only if the procedure says so. Outside the fragment the
procedure is a sound heuristic (relative to the semantics and a correct
solver). The Agda files (`agda/`) are an intrinsically typed mirror of the
Rust stages: they show the stages are well scoped. They define no
semantics, so they prove no soundness; they use `postulate` and
`{-# TERMINATING #-}`, and nothing links them to the Rust.

## Where "provable" actually rests

| Trusted by QED | What can go wrong |
|---|---|
| The Rust translation and normaliser | Any bug is a false "provable"; no certificate is produced |
| Z3 and CVC5, as processes | An `unsat` is taken from either; a solver bug is a false "provable" |
| A text rewrite of the SMT-LIB script between the two (`unify.rs`, `smt`): `" and"` to `" true"`, `" or"` to `" false"`, `"(* "` to `"(* 1 "`, `"(+ "` to `"(+ 0 "` by plain string replacement, string literals included | **A false "provable."** `x = 'rock and roll'` against `x = 'rock true roll'`: honest Z3 says the negated goal is `sat`; after the rewrite both literals read `"rock true roll"` and Z3 says `unsat`. Reproduced on 2026-10-28 with the Python Z3 bindings and the same replacements. Any literal containing ` and`, ` or`, `(* ` or `(+ ` is affected |
| `REAL` literals parsed as `f32` (`normal.rs`, `parse`) | Two decimal literals that round to the same `f32` are equal |
| Unbounded integers, mathematical reals, Z3 strings | No overflow, no `numeric(p,s)` rounding, no collation, no trailing-space rule, no length limits |
| The semantics itself | No errors: division by zero, conversion failures and a scalar subquery with two rows are not outcomes. Aggregates have no meaning: `SUM` of no rows (NULL), `COUNT(*)` against `COUNT(col)` and integer overflow in `SUM(int)` are invisible. `ORDER BY` and `LIMIT` have no meaning, so "B orders what A leaves unordered" cannot be expressed |
| Calcite's dialect, queries only | One semantics, so a function name means the same on both sides; across T-SQL and PostgreSQL it does not (`LEN` ignores trailing spaces, `length` counts them) |

QED has no law of excluded middle (`A + ¬A = 1` is listed as unsupported),
so it cannot case-split: `WHERE p OR NOT p` against no filter is not
provable. A proof assistant gets this for free.

Nothing here is a criticism of QED's purpose, an optimiser-rule checker. It
is what any solver-backed checker without a mechanised semantics looks like,
and it is why decision 2 of the roadmap keeps solvers and Rust out of the
trusted base. QED is MIT licensed, so taking code is allowed; what carries
over is ideas, lemmas and test data.

## What carries over

### The U-semiring as the query semantics (phase 2)

A bag is `Tuple σ → ℕ`. Over a finite database the sums are finite, so
`ℕ` suffices and QED's `ℕ̄` is not needed. `‖r‖ t := if r t = 0 then 0 else
1`, `¬r t := if r t = 0 then 1 else 0`. Every phase 2 construct that QED
covers has a one-line meaning (`relation.rs` is a checklist: every join
kind, semi and anti joins, `UNION`, `UNION ALL`, `INTERSECT`, `EXCEPT`,
`EXCEPT ALL`, `DISTINCT`, `VALUES`, `IN`, `EXISTS`, lateral joins, grouping
as `‖∑ s. [x = k(s)] × R(s)‖` times the aggregate of the group).

What SQLDiff adds on top, per dialect version, as decision 1 requires:
scalar expressions evaluate to a value, NULL **or an error**, and a
statement that hits an error is an error observation, so `Run` of a query
is a bag or an error; comparison and `LIKE` go through the dialect's
collation; `ORDER BY` makes a list, `TOP`/`LIMIT` without a total order is
A's set of allowed bags; aggregates have their meaning (below).

### Decide by normalising, prove the normaliser once

In Lean the shape is

```lean
def norm : Query → NF                         -- sum-product normal form
theorem norm_sound (q) : ⟦norm q⟧ = ⟦q⟧        -- once, for the fragment
def check (a b : Query) (hint : Renaming) : Answer :=
  if nfEq (norm a) (norm b) hint then .confirmed "Query.check_sound" else …
```

`nfEq` compares up to commutativity of `+` and `×` and a renaming of bound
variables; the renaming is supplied by Rust as an untrusted hint, like the
alignment in decision 2, so Lean checks instead of searching (QED's
24-permutation loop is the search to avoid). One theorem then covers join
reordering, predicate pushdown, subquery-to-join, `IN` to semi join and
projection merging, rules `canon.rs` handles one by one today.

QED's "complete fragment" gives the staging: inside it the normaliser is a
decision procedure, so a Lean "not equal" is a real difference, and that is
where `require` mode (decision 3) is honest first. Outside it the check is a
sound heuristic and an "unconfirmed" means "not shown", as the roadmap
already says.

### Catalog facts as hypotheses

QED's encodings of keys, `NOT NULL` and `CHECK` are the phase 2 item
"catalog keys and nullability". In Lean they are hypotheses from Γ on the
database (`Γ.holds db`), used by lemmas such as `‖R‖ = R` for a keyed
table, the one-point rule `∑ x. [x = e] × f x = f e`, and the two below.

### Two lemmas worth taking verbatim

- **`LIMIT 1` over a degenerate relation** (`partial.rs`, `Term::splitter`):
  when `∑ R = ‖∑ R‖` (the relation has at most one row), `LIMIT 1` is the
  identity and the pick is deterministic. This is the lemma that discharges
  `ASM-SINGLE-ROW` for `SELECT @v = col … WHERE key = @k`, `SELECT … INTO`
  and `TOP 1` over a key lookup: the degeneracy follows from the key
  hypothesis, so the converted `SELECT INTO STRICT` or `LIMIT 1` is equal,
  not equal under an assumption.
- **Solver-proposed equalities as a certificate** (`stable.rs`,
  `stablize`): Z3 proposes which bound variables a term's predicates fix;
  QED substitutes. In Lean the substitution is checked (each equality
  follows from the term's own predicates by the one-point rule or a key
  hypothesis), so the solver may stay as an untrusted proposer.

### Three-valued predicates and `IN`

QED's `Option` lifting and its `IN`/`SOME` definition (paper section 3.2,
`normal.rs` `quant_cmp`) are a correct first draft of phase 1 spec rules.
They are drafts: each gets its vendor citation and engine test like any
spec rule (decision 5). Note what it does not cover: `IS [NOT] DISTINCT
FROM`, T-SQL `ANSI_NULLS OFF`, and that `COUNT(*)` against `COUNT(k)` is
handled by a pre-filter in the input encoding, not by a semantics of
`COUNT`.

### Staged early exits, and recording which stage confirmed

QED exits at raw plan equality, then normal form, then stable form, then
SMT, and records in `Stats` whether the pair was in the complete fragment.
The gate wants the same: AST equality (today's trivial fragment), lowered
terms, normal form, each a theorem with its own name in `confirmed`, so the
coverage reports of decision 3 say which stage and fragment confirmed each
verdict.

### Test data

`tests/calcite` (444 pairs, about 290 provable by QED) and `tests/cockroach`
are optimiser rewrites an engine's authors consider correct, as JSON plans
with schemas. An importer into the Lean query AST gives phase 2 an outside
benchmark: a Lean "not equal" on one is a gap in the normaliser or a
finding. They are plans, not SQL text, and not T-SQL or PostgreSQL, so they
exercise the normaliser, not the parsers or the dialect specs.

## What does not carry over

- **The Rust code.** Nightly features, a forked `z3` crate, two solver
  processes per query, shared `Rc<RefCell>` state; and taking it would be a
  second lowering to keep in sync. The algorithms are short enough to write
  in Lean, mirroring `canon.rs` as the convergence plan requires.
- **SMT in the trusted path.** A solver may propose (a renaming, an
  equality, an alignment); Lean checks. Arithmetic side conditions go to
  `omega`, `decide` or `bv_decide`.
- **Uninterpreted aggregates, ordering and functions as the end state.**
  Congruence (`f(R) = f(R')` when `R = R'`) is sound only when `f` means the
  same on both sides. Within one dialect it does; across dialects every
  function pair needs its own fact (`profiles/builtins.toml` is where those
  live), and the aggregates SQLDiff's conversions actually touch (`SUM` of
  no rows, `SUM(int)` overflowing in T-SQL where PostgreSQL widens to
  `bigint`, `COUNT` variants, `AVG` of integers truncating in T-SQL) need
  their meaning in the spec.
- **Unbounded integers and exact reals.** The spec needs each dialect's
  numeric types as they are: widths, overflow as an error, `decimal`
  precision and scale rules, `float` as IEEE binary64.

## Related tools, for orientation

- Cosette and HoTTSQL (Coq, 2017), UDP (2018), SPES (2022): the semiring
  line QED belongs to.
- SQLSolver (SIGMOD 2023): the same problem through LIA* (linear integer
  arithmetic with unbounded sums); proves 346 of 359 Calcite and Spark
  pairs. Same trust position as QED.
- VeriEQL (OOPSLA 2024): bounded equivalence, so it also *refutes*, with
  counterexamples that found bugs in MySQL and Calcite. The analogue of
  SQLDiff's witness search (`equiv/xwit.rs`, `eval.rs`), which may use
  solvers freely because "different" is not in the trusted base.
- Mechanised SQL semantics with NULLs and bags: Benzaken and Contejean
  (Coq, 2018, with an extracted certified analyser) and Ricciotti and Cheney
  (Coq, 2022: sets, bags, lateral joins, nested subqueries, NULLs, and a
  certified translation of a relational calculus into SQL). No Lean 4
  formalisation of SQL was found. These are the definitions to port, and
  the Ricciotti–Cheney translation is the closest precedent for a verified
  translation whose target is SQL.

## Leads for the fork

- Fix the SMT-LIB text rewrite: emit the empty `and`/`or` cases as
  `true`/`false` and the unary `*`/`+` cases as proper terms before dumping,
  instead of editing the script. A regression test with a string literal
  containing ` and`. Worth an upstream report.
- Parse `REAL` literals as exact rationals, not `f32`.
