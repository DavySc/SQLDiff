# Verified conversion

Status: proposal, second pass over the verification plan with one target in
view. Written 2026-10-28. Companion to
[verification-roadmap.md](verification-roadmap.md) (the decided plan for the
diff gate) and [qed-lessons.md](qed-lessons.md) (what the QED prover
contributes). Nothing here is decided; the decisions it needs are listed at
the end.

## The target

> Convert a routine from one supported dialect version to another, and ship
> the result without re-reading it, as long as the proof behind the tool is
> trusted.

Stated as the theorem the tool ships with, for source `a` on version `vA`,
output `b` on `vB`, catalog Γ, assumptions `S`, observer profile `prof` and
value correspondence `R` (all as in decision 1 of the roadmap):

```lean
theorem convert_sound
    (h : verify vA vB Γ S prof a b cert = .confirmed) :
    Refines prof R vA a vB b S
```

plus one runtime fact per unit: the text written to disk parses back, with
the target dialect's parser, to exactly the `b` the theorem is about. What
the theorem does *not* cover is then the whole review surface, and the aim
is to make that surface small, explicit, and reviewed once per deployment
instead of once per routine:

| Reviewed once per deployment, not per routine | Why it stays |
|---|---|
| The assumptions `S` the converted tree still carries, as a short list with the facts that would discharge them | Facts about the target database the tool cannot see (collation of the instance, who calls the routines, whether row counts are read) |
| The observer profile | Which side channels callers read is a property of the callers |
| The catalog snapshot the proof used | The proof holds for that schema; a schema change re-runs the check |
| The trust base below | Lean, the specs, the axioms, the parsers |

| Never covered, by design | Where it is said |
|---|---|
| Concurrency and isolation, performance, plans, locks, resource limits | Decision 1: excluded by assumption or not observed |
| Dynamic SQL, constructs the converter refuses | They are `not converted` and `differs`, never silent |
| Versions in `preview` | Never confirmed (roadmap, RDBMS versions) |
| The fidelity of the spec to the engines | Mitigated by differential testing, never proven; the one place "trust the proof" means "trust the spec" |

So "no re-review" is reachable for a tree when every unit is `converted`
with `proof: confirmed`, its assumptions are a subset of the deployment's
accepted list, and the catalog hash matches. `sqldiff convert --proof
require` makes that the exit code.

## Where the current code stands against it

From a read of `crates/sqldiff-convert` and `crates/sqldiff-core/src/equiv`
on 2026-10-28:

| Fact | Consequence for the target |
|---|---|
| Conversion is CST to text: rules walk the lossless tree down to statements, rewrite expressions as flat token slices (`tok.rs`), and push strings into an output buffer (`out.rs`). There is no target AST | Nothing exists for a proof to be about. A checker can only re-parse the text, which is what `check` does today |
| Rules are about 143 hand-written methods in `tsql.rs` (6,250 lines, plus `bits.rs`, `temporal.rs`, `trigger.rs`), one walk for both targets, with `pick(pg, db2)` choosing spellings | Rule meaning is in Rust control flow, not in data a proof could name |
| The rule trace is incomplete: rewrites that need no assumption leave no note (`LEN` to `length(rtrim(…))`, `NEWID` to `gen_random_uuid()`, `tsql.rs:4109`, `:4119`); about 88 `ASM-*` ids and 33 rule ids are recorded only when a rule chooses to | A checker cannot be told which lemma to apply where |
| Correctness is decided by `sqldiff_core::diff(input, output)`: the general two-program problem, cross-dialect, on untyped `Term`s (`term.rs`), with hand-coded hooks in `compare.rs` | The hardest version of the problem is solved where the easiest is available: the tool produced `b` from `a` and knows every step |
| The Lean gate confirms identical trees in one dialect only; cross-dialect pairs are answered "unconfirmed" in Rust before reaching it (`proof.rs:197`); the convert path never sets a proof mode | No conversion is ever confirmed today, and the design (decision 2) sends the checker only the two ASTs, no trace |
| Dialect facts live in Rust (`canon.rs`, `literal.rs`, `eval.rs`), backed by probe evidence under `eval/` that is not loaded at run time; the error and transaction model exists three times (`ir/` in Rust, `ErrorTxn.tla`, `profiles/semantics/*.toml`), linked by test cases only | Three places to drift; none is the spec a proof would use |

None of this is wrong for a diff tool. It is the wrong shape for a converter
that proves its own output, and the point of this pass is that the
converter's position is *stronger* than the diff tool's, if the code lets it
use that position.

## Three shapes of a verified converter

| Shape | What is proven | Cost | Verdict |
|---|---|---|---|
| **Verified converter** (CompCert): `conv` is written in Lean and `∀ a, Refines a (conv a)` is proven once | Everything, for every input | Every rule change breaks proofs; the converter would be Lean from day one; the Rust rules are thrown away | The end state of convergence, not the start |
| **Translation validation** (decision 2 today): Rust converts, Lean lowers both ASTs and compares normal forms | Each output, when the comparison succeeds | Lean must *discover* why `b` equals `a`: alignment, renaming, which rewrite was used; coverage is bounded by what the normaliser can show | Right for human-written `b` (the diff tool), weak for machine-written `b` |
| **Certifying converter**: Rust converts and emits a trace (which rule, at which node, with which instantiation); Lean checks each step against that rule's proven lemma, then compares what is left | Each output, exactly as far as the trace reaches | Each rule needs a lemma once; no search in the checker; coverage equals rule coverage, measured per rule | **The shape for `convert`** |

Decision 2 rejected a certificate for the *diff* gate because it would mean
"a certificate format for every rule in `canon.rs`, and two lowerings
forever". Both reasons are about the diff tool and do not apply here: the
converter's rules are the certificate's vocabulary (a rule id, a node path,
an instantiation), not `canon.rs`'s, and the lowering stays one. The two
shapes also compose: a unit the trace covers is checked step by step; a unit
a person edited afterwards, or that an idiom pass rewrote beyond the trace,
falls back to normal-form comparison (the phase 2 gate) and is confirmed or
not on that basis.

## The design principle: generate for provability

A converter that wants to be proven right should emit the output that is
*easiest to prove*, not the most idiomatic one, and offer idioms as a
separate, separately verified pass. Concretely:

**Mirror lowering.** Every source construct maps to the target construct
with the closest semantics, with the difference between them closed by
explicit scaffolding in the output rather than by an assumption:

| Difference | Idiomatic output assumes | Provable output emits |
|---|---|---|
| Implicit conversions follow different precedence rules | `ASM-UNTYPED-*`, `ASM-ISNULL-TYPE` | Every conversion the source performs implicitly as an explicit `CAST` with the source's result type; the lemma is about the explicit form |
| `=` and `LIKE` on strings are case-insensitive and ignore trailing spaces in T-SQL under its default collation | `ASM-CI-COLLATION` | A `COLLATE` on the comparison with a target collation that has the same behaviour, when the target has one, or an explicit `rtrim`/case fold where the lemma can show it equal; otherwise the assumption, named |
| Unqualified names and case folding | `ASM-DEFAULT-SCHEMA`, `ASM-IDENT-FOLD` | Every name schema-qualified; every identifier quoted in one spelling the converted DDL also uses. When the converter converts the DDL too, the target catalog is its own output and the assumption is discharged by construction |
| Result column names fold to lower case in PostgreSQL | `ASM-RESULT-NAMES-CI` | Quoted aliases (`AS "OrderId"`), so names are equal, not equal up to case |
| `TOP 1` or `SELECT @v = col` picks an unspecified row | `ASM-SINGLE-ROW` | Either a proof that the relation is degenerate (at most one row from Γ's key: QED's `LIMIT 1` lemma) and the assumption is gone, or a deterministic tie-breaker `ORDER BY key` in the output: B then refines A's unspecified pick (decision 1) and still needs no assumption |
| `SUM(int)` overflows with an error in T-SQL; PostgreSQL widens to `bigint` | nothing today | `CAST(SUM(x) AS int)`, which raises the same error kind, or the refinement recorded; the point is the spec sees it |
| A T-SQL statement fails on its own and the batch continues (no `XACT_ABORT`); a PL/pgSQL statement fails and the block aborts | `ASM-TRY-SUBTRANSACTION`, `equiv.error-continues` findings | A per-statement `BEGIN … EXCEPTION` where the source's continuation is observable, generated from the CFG; slower, and equal |
| Routine runs with the owner's or the caller's privileges | `ASM-OWNER-PRIVILEGES` | Already mirrored (`SECURITY DEFINER`); keep |

Each row trades run-time cost or idiom for a smaller `S`. The deployment
decides which trades it wants: a profile may say "accept
`ASM-RESULT-NAMES-CI`, our callers are case-insensitive" and get lower-case
aliases back. The converter's own choice should be the provable form.

**Idiom passes.** `MERGE` to `INSERT … ON CONFLICT`, `LEFT JOIN … IS NULL`
to `NOT EXISTS`, `IF EXISTS … UPDATE ELSE INSERT` to an upsert, `ISNULL`
chains to `COALESCE`, `TOP` with `ORDER BY` to `LIMIT`: each is a rewrite
from target code to target code, within one dialect version, so its lemma
is a *single-dialect* equivalence or refinement over the target spec alone,
which is the problem the semiring normaliser is good at. The converter's
existing instinct is right (`convert-mode.md`: a plain `IF EXISTS … ELSE
INSERT` stays an `IF` "with the race intact" rather than becoming an atomic
`ON CONFLICT`): the upsert idiom changes concurrency, so it is a pass the
user turns on, not a default. In the certificate an idiom pass is one more
step with its own lemma; where no lemma exists yet the unit stays
`unconfirmed` and the GUI shows the mirror form beside the idiom.

The separation is what makes the proof effort finite: **all cross-dialect
reasoning is in the mirror lemmas, one per construct pair; all cleverness is
single-dialect.**

## The semantics, with one simplification to decide

The roadmap plans a semantics per dialect version in Lean plus a lowering
proven to preserve it (decision 2). The `Term` design already in `term.rs`
suggests a cheaper structure, worth deciding explicitly:

- One typed **core** language with one semantics: bags as `Tuple σ → ℕ`
  (the U-semiring, see qed-lessons.md), scalar values with NULL and errors,
  explicit operators for every behaviour that differs between dialects
  (`eq_ci_pad` and `eq_cs` for string equality, `div_trunc` and `div_num`,
  `order_nulls_low`/`_high`, `top_unspecified`), lists for ordered results,
  the CFG with the error and transaction model of `ErrorTxn.tla`, and
  version predicates.
- Each dialect version's spec **is** its lowering into the core: `⟦a⟧_tsql2022
  := ⟦lowerT a⟧_core`. There is no separate T-SQL semantics to prove the
  lowering against; the lowering rules are the spec, reviewed and locked as
  decision 5 says, and validated against the engine by running the core's
  executable semantics on the lowered program (the differential test the
  roadmap wants anyway).
- Checking a mirror conversion is then `lowerP b ⊑ lowerT a` in the core,
  and for a faithful mirror most of it is computation (`decide`, `rfl`) with
  the certificate naming the lemma only where the mirror is not literal.

The risk this takes: a lowering that conflates two behaviours the engine
distinguishes makes the core blind to the difference, and the proof would
hold for a program the engine does not run. That is the same risk decision 1
already names for "unspecified" rules, controlled the same way: a vendor
citation per lowering rule, engine differential tests that can refute, and
expert review of the rules by risk. The benefit is one semantics to write,
test and keep executable instead of three plus three proofs, and a `Term`
that is finally typed and meaningful: today's `canon.rs` becomes the Rust
twin of `lowerT`/`lowerP`, and convergence (the roadmap's last step) is
replacing it with the compiled Lean lowering.

The three existing models of errors and transactions (`ir/`, `ErrorTxn.tla`,
`profiles/semantics/*.toml`) collapse into the core's CFG semantics, with the
TOML tables either generated from the Lean spec or checked against it in CI.

## The trust base, listed

| Trusted | Why it has to be | How it is kept honest |
|---|---|---|
| Lean 4 kernel and compiler | | Pinned toolchain |
| The core semantics and each dialect's lowering rules (`Spec/`) | They define what "equal" means | Citations, engine differential tests, `SPEC.lock` signed by the owner, external review (decision 5) |
| Axioms: vendor facts not provable from anything | | `#print axioms` against the allowlist; each maps to a `V*` fact and its probe status |
| The two parsers and the AST meaning contract | The proof is about the AST the parser produced | Generated Lean types from the Rust AST, schema hash in both binaries, differential tests against `libpg_query` and ScriptDom (roadmap) |
| **The printer, or its re-parse check** | The file holds text, the proof holds an AST | Cheapest: after printing `b`, parse the text with the target parser and require the AST to be identical; the parser is already trusted. A proven printer in Lean would need the parser in Lean, which the plan avoids |
| The catalog adapter | Γ must describe the real schema | Snapshot hash in the certificate; the converted DDL is itself certified, so Γ_B is the converter's own output |
| The deployment profile | The accepted assumptions and the observer switches | A file under review, like a pack; the report lists exactly which `ASM-*` it absorbed |

Not trusted: the converter, every rule, the trace it emits, the alignment
and renaming hints, the normaliser's search, any SMT solver, the witness
search, the GUI, the review ledger.

## The certificate

Per unit, written next to `conversion-report.json` and embedded as a hash in
the output file's status line:

```
unit        file#id, source span, output span
versions    tsql 2022/160, postgres 18          (typed targets, decision 7)
catalog     sha256 of the source snapshot, sha256 of the converted DDL
profile     observer profile id, deployment profile id
source      sha256 of lowerT(a) as the checker computed it
target      sha256 of the target AST; re-parse check passed
trace       [(rule id, node path, instantiation)…]  -- untrusted hints
assumptions ASM-… (residual), ASM-… (discharged by: catalog fact | generated code | profile)
answer      confirmed <theorem names, one per step> | unconfirmed <reason>
axioms      closure from #print axioms, mapped to ASM-FACT-*
checker     sqldiff-check version, spec version
```

Re-checking a tree is deterministic and cheap (the phase 0 spike puts a
request in microseconds), so "no re-review" means "re-run the checker on
the certificate", in CI, on every schema or spec change.

## Where QED's ideas land in this plan

| Idea (qed-lessons.md) | Place in the converter |
|---|---|
| U-semiring semantics of queries | The core's bag semantics |
| Normalise then compare, normaliser proven once | The fallback gate for units the trace does not cover (human edits), and the proof technique for idiom-pass lemmas; not needed for the mirror |
| Keys and `NOT NULL` as hypotheses from Γ | The mirror lemmas' side conditions; the degenerate-`LIMIT 1` lemma discharges `ASM-SINGLE-ROW` |
| Solver proposes, checker verifies | Rust may use Z3 to find the renaming or the equalities it puts in the trace |
| Uninterpreted operators with congruence | Correct within one dialect (idiom passes); across dialects each function pair is a mirror lemma with a vendor fact (`profiles/builtins.toml` is the list) |
| Complete fragment | The staging unit for `require` mode: inside the fragment the fallback gate is a decision procedure, so "unconfirmed" there is "different" |
| Calcite and CockroachDB pairs | Benchmark for the fallback normaliser, not for the converter |

## Phases, reordered for the converter

The roadmap's phases are kept and their contents reused; what changes is
the order and the exit criteria, because the converter needs *breadth over a
whole routine at mirror depth* before it needs *depth in queries*. Estimates
are elapsed time for one engineer with agents, wide error bars, and are in
addition to nothing: they re-cut the roadmap's 14 to 24 months, they do not
add to it, except for C0's Rust work.

| Phase | Scope | What reaches "no re-review" | Estimate |
|---|---|---|---|
| **C0 Shape** | A typed target AST and printer in `sqldiff-syntax` for PostgreSQL (and T-SQL, for the reverse direction later); the converter emits AST, rule by rule, text-emitting rules kept but their units can never confirm; a complete trace (every rewrite records its rule, including the ones that need no assumption); the re-parse check; `--proof` on `convert` with the certificate file; the deployment profile file; the AST codegen to Lean the roadmap plans; the decision below on the core | Nothing yet; the gate confirms what phase 0 confirms | 3 to 5 months, mostly Rust |
| **C1 Skeleton** | Core CFG without data: `DECLARE`, `SET`/assignment of untyped placeholders, `IF`, `WHILE`, `RETURN`, blocks, routine headers and parameter modes, calls (`EXEC` to `CALL`); mirror lemmas for them; the error-free, transaction-free routine | Routines whose statements are all in C1 (rare, but the proof plumbing is complete end to end) | 2 to 3 months |
| **C2 Scalars** (roadmap phase 1) | Values, NULL, numeric types with widths and overflow, `decimal` precision and scale rules, strings and collation, casts and implicit conversion made explicit, `CASE`/`COALESCE`/`ISNULL`, dates and `DATEADD`/`DATEDIFF`, the built-in pairs of `builtins.toml` with their facts | Expression-level conversions; every `ASM-UNTYPED-*` and `ASM-ISNULL-TYPE` gone | 4 to 6 months |
| **C3 Statements** (roadmap phases 2 and 3, mirror depth) | Query operators by congruence (same operator, related inputs), `ORDER BY` and NULL ordering, `TOP`/`LIMIT` with the degenerate and tie-breaker lemmas, aggregates with their meaning, `INSERT`/`UPDATE`/`DELETE` including `UPDATE … FROM`, `MERGE` as a mirror where the target has it, generated keys, `RETURNING`; the converted DDL certified so Γ_B is output | Set-based routines without error handling: most of the corpus's statements | 4 to 6 months |
| **C4 Errors and transactions** (roadmap phase 4) | `ErrorTxn.tla` ported as the core's CFG semantics; `XACT_ABORT`, `TRY/CATCH` to `EXCEPTION`, `@@ERROR` and return codes, `THROW`/`RAISERROR` to `RAISE` with the pack's code mapping, `BEGIN TRAN`/`COMMIT`/`ROLLBACK` and `@@TRANCOUNT`, savepoints, the per-statement exception scaffolding; T-SQL and PostgreSQL rows of the model as lemmas, Db2 for i rows as axioms | **Whole procedures.** This is the phase that reaches the target for real code; everything before it is preparation | 4 to 6 months |
| **C5 Idioms and the normaliser** (roadmap phases 2 and 5) | The semiring normaliser as the fallback gate; idiom passes as verified single-dialect rewrites with a user switch; the complete fragment as the `require` boundary; coverage ratchet; parser differential tests | Human-edited outputs and idiomatic outputs confirm; `require` by default per dialect pair | Ongoing |
| **C6 Db2 for i** (roadmap phase 6, decision 8) | C1 to C4 for SQL PL; W2 and W3 on rented IBM i | | 4 to 8 months |

Two things move earlier than in the roadmap: the typed target AST (C0,
Rust) and the error and transaction model (C4 before the normaliser),
because a procedure is never confirmable without C4 and the normaliser
contributes nothing to a mirror. One thing moves later: deep query
equivalence, because the converter does not need it to prove its own mirror
output.

## What the Rust side changes now (C0 in detail)

1. **Target AST.** `sqldiff-syntax` gains a typed abstract syntax for the
   target dialects (statements and expressions, not the lossless CST) and a
   printer. `equiv::ast` already parses tokens into `Stmt`/`Expr` for all
   dialects; it is the starting point, promoted from a private helper of
   the diff engine to the AST both tools share. The AST codegen to Lean the
   roadmap plans is written against it.
2. **Converter emits AST.** `Cx` methods return AST nodes instead of
   strings, migrated one construct at a time; `Out` becomes the printer.
   Until a construct is migrated its unit prints as today and is marked
   `proof: unconfirmed, reason: text-emitting rule`, so the migration is
   visible in the coverage counts.
3. **Complete trace.** `note` becomes mandatory for every rewrite: a rule id
   per arm in `func`, `ty` and the expression walk, with the node path. The
   trace is the certificate's `trace` field. `conversion-report.json` stops
   emptying `rules` for plainly converted units.
4. **Re-parse check** after printing, in `check`, as a gating finding
   (`print.unparsed`, differs) when the text does not parse back to the AST.
5. **`--proof` for `convert`** and the certificate file; the convert path
   sends cross-dialect requests (today `proof.rs:197` refuses them).
6. **Deployment profile**: `[deployment]` in `sqldiff.toml` or a pack: the
   `ASM-*` ids accepted, the observer switches, and catalog facts the user
   asserts (`collation = "SQL_Latin1_General_CP1_CI_AS"`) that discharge
   assumptions when the engine can check them against a catalog, and are
   listed as the user's own when it cannot.
7. **An assumption table** (`docs/assumptions.md`, generated): every
   `ASM-*` id, what discharges it (catalog fact, generated code, deployment
   declaration, nothing), and which rule emits it. Today the ids are
   scattered over 122 call sites with their text inline.
8. **One model.** Decide how `ir/`, `ErrorTxn.tla` and
   `profiles/semantics/*.toml` relate to the future core (which is the
   source, what is generated), before C4 writes the fourth copy.

## Decisions this needs

| # | Decision | Recommendation |
|---|---|---|
| 9 | Spec structure: one typed core whose semantics each dialect's lowering defines, or a semantics per dialect plus proven lowerings (decision 2 as written) | One core; the lowering rules are the reviewed spec. Fewer artifacts, one executable oracle, and `canon.rs` already has this shape |
| 10 | The converter's default output: provable (mirror with explicit scaffolding) or idiomatic | Provable by default; idioms as verified, switchable passes. The GUI shows both |
| 11 | A certificate (rule trace) for `convert`, beside the two-AST protocol decision 2 fixed for `diff` | Yes: the trace is untrusted hints over the same protocol, so decision 2 stands for `diff` |
| 12 | Target AST and printer in Rust, converter migrated from text to AST | Yes, in C0; it is also the only way the GUI's live conversion and the tree converter can share one checked path |
| 13 | Phase order: errors and transactions before the query normaliser | Yes, for the converter; the diff tool's phase 2 moves to C5 |
| 14 | Assumption discharge: a deployment profile that absorbs named assumptions, and `--proof require` as the release gate for converted trees | Yes; this is where "no re-review per routine" becomes a CI exit code |
