# Verified conversion

Status: the priority is decided, the structure is proposed. Written
2026-10-28. On that date the project owner decided that **the converter
comes first in the verification project**: the proof effort is ordered to
reach converted code that needs no re-review, and the diff gate for
human-written variants follows. The design below (decisions A and B, and 9
to 14 at the end) follows from that priority and is proposed for the owner's
confirmation item by item. Companion to
[verification-roadmap.md](verification-roadmap.md) (the decided plan, whose
phase order this supersedes for the converter) and
[qed-lessons.md](qed-lessons.md) (what the QED prover contributes).

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
| The Lean gate confirms identical trees in one dialect only; cross-dialect pairs are answered "unconfirmed" in Rust before reaching it (`proof.rs:259`); the convert path never sets a proof mode. The syntax tree's kinds are generated into Lean (`formal/lean/Sqldiff/Syntax.lean`, with a schema hash in every request) and the checker decodes trees into Lean values, but that is the lossless CST, not an abstract syntax a semantics is given on | No conversion is ever confirmed today, and the design (decision 2) sends the checker only the two ASTs, no trace |
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

## Two decisions, not one

Prioritising the converter means two things that are independent of each
other and have different costs. They are kept apart so each can be judged
on its own.

### A. The converter gets its own proof path

Today a conversion is confirmed only when the `diff` gate would confirm the
pair, and Lean is given two ASTs with no record of how `b` came from `a`.
Decision A adds the certifying path above: a typed target AST, the
converter emitting AST, a complete rule trace, the re-parse check, the
certificate, `--proof` on `convert`. This is **work the roadmap does not
contain**, about 3 to 5 months of Rust (C0 below), and it is required for
the target in any phase order: without it there is nothing a proof can be
about, and non-literal rewrites (`SELECT @v = col` to `SELECT … INTO`,
`CROSS APPLY` to `LATERAL`, `TRY/CATCH` to `EXCEPTION`) confirm only when
Lean's search happens to rediscover the rule.

| Improves | Does not improve |
|---|---|
| Coverage of non-literal rewrites: the trace names the lemma, Lean checks instead of searching | The calendar: C0 runs before or beside the first Lean phase and adds months; the first confirmed procedure does not arrive sooner |
| Determinism: no search budget; a result does not depend on a timeout | The spec and review effort, which the roadmap rightly calls the critical path: every lemma still needs a cited spec rule and an approval |
| Diagnosis: an unconfirmed unit names the rule and node that failed, so the fix is a rule, not a search for why normal forms differ | Assumptions no code can discharge: what the callers read and under which privileges they run stay in the deployment profile |
| The printer gap is closed by the re-parse check | |
| The converter can generate for provability, because it controls the AST it emits | |

### B. Errors and transactions before the query normaliser

The roadmap orders phases by semantic depth (scalars, queries with the
normaliser, DML, routines), which is right for `diff`: a human rewrites a
query and the normaliser shows the forms equal, so phase 2 is where `diff`
first pays off. For the converter the same order spends the normaliser's 3
to 5 months before the one phase that lets a procedure confirm at all, and
a mirror conversion never needs it. Decision B does scalars, then queries
and DML **at mirror depth**, then errors and transactions, then the
normaliser.

"Mirror depth" means: for output with the same shape as the source, the
proof is one congruence lemma per operator (the same operator on related
inputs gives related outputs), by structural recursion, with no normal
forms. The hard semantic content is still all there and still has to be
written and reviewed: NULL ordering in `ORDER BY`, `TOP`/`LIMIT` without a
total order, `SUM` of no rows and `SUM(int)` overflowing, string equality
under collation, `decimal` scale rules. Only the normaliser itself
(sum-product normal form, bound-variable renaming, the one-point rule, key
cancellations) and its external benchmark move to C5.

| Improves | Costs |
|---|---|
| Whole procedures confirm after C4, without the normaliser in between: the same work as the roadmap's phases 0 to 4, minus one phase | **`diff` waits.** Human-written variants with rewritten queries (UC1 drift, UC2 version review) confirm only in C5. The GUI for human review, which design.md names as the product's primary face, is served later |
| The machinery is stressed by the converter's real output rather than by optimiser rewrites it never produces | **C3's non-literal lemmas are hand proofs.** `CROSS APPLY` to `LATERAL`, `ISNULL` to `COALESCE` with its typing, `IIF`, `STRING_AGG`: several are exactly what a normaliser discharges mechanically. Hence the hybrid below |
| | **Phase 4 is no easier for being earlier.** Porting `ErrorTxn.tla` and proving the T-SQL and PostgreSQL rows is the bottleneck under both orders |
| | **Less idiomatic output until C5.** Explicit casts, quoted aliases, per-statement exception blocks where the source's continue-on-error is observable; idioms come back as verified passes |
| | **A risk moves later.** The roadmap's phase 2 hardens the bag semantics early against 444 external pairs; here they are first stressed by congruences and engine tests, and a structural flaw in the core (decision 9) shows later |

**The hybrid.** A small normaliser is written in C3 as a Lean *proof
tactic*, used to prove the non-literal mirror lemmas, never run at check
time. The runtime normaliser, the complete-fragment staging and the Calcite
benchmark stay in C5. This recovers most of what a pure reorder loses and
keeps the trusted path free of search.

| | Roadmap order | A + B with the hybrid |
|---|---|---|
| First confirmed query rewrite in `diff` | After phase 2 | After C5 |
| First confirmed converted procedure | After phase 4, where normal forms coincide; non-literal rewrites depend on search | After C4, every unit the trace covers, deterministic |
| Confirmed conversion coverage | What the normaliser rediscovers | Rule coverage, measured per rule |
| Unconfirmed conversion says | "normal forms differ" | "rule X at node Y: lemma failed" |
| Spec and review effort | Same | Same |
| Rust work | AST export | AST export, target AST, printer, trace, re-parse, certificate: 3 to 5 months more |
| Time to the first confirmed procedure | Phases 0 to 4 | About the same, C0 against the normaliser |

Unchanged under both: decisions 1, 3, 5, 7 and 8 of the roadmap, the
lifecycle policy, and convergence.

## Phases

The roadmap's phases are kept and their contents reused; what changes is
the order and the exit criteria, because the converter needs *breadth over a
whole routine at mirror depth* before it needs *depth in queries*. Estimates
are elapsed time for one engineer with agents, wide error bars, and are in
addition to nothing: they re-cut the roadmap's 14 to 24 months, they do not
add to it, except for C0's Rust work.

| Phase | Scope | What reaches "no re-review" | Estimate |
|---|---|---|---|
| **C0 Shape** | A typed target AST and printer in `sqldiff-syntax` for PostgreSQL (and T-SQL, for the reverse direction later); the converter emits AST, rule by rule, text-emitting rules kept but their units can never confirm; a complete trace (every rewrite records its rule, including the ones that need no assumption); the re-parse check; `--proof` on `convert` with the certificate file; the deployment profile file; the Lean codegen extended from the syntax-tree kinds it already covers (`Syntax.lean`) to the abstract AST; the decision below on the core | Nothing yet; the gate confirms what phase 0 confirms | 3 to 5 months, mostly Rust |
| **C1 Skeleton** | Core CFG without data: `DECLARE`, `SET`/assignment of untyped placeholders, `IF`, `WHILE`, `RETURN`, blocks, routine headers and parameter modes, calls (`EXEC` to `CALL`); mirror lemmas for them; the error-free, transaction-free routine | Routines whose statements are all in C1 (rare, but the proof plumbing is complete end to end) | 2 to 3 months |
| **C2 Scalars** (roadmap phase 1) | Values, NULL, numeric types with widths and overflow, `decimal` precision and scale rules, strings and collation, casts and implicit conversion made explicit, `CASE`/`COALESCE`/`ISNULL`, dates and `DATEADD`/`DATEDIFF`, the built-in pairs of `builtins.toml` with their facts | Expression-level conversions; every `ASM-UNTYPED-*` and `ASM-ISNULL-TYPE` gone | 4 to 6 months |
| **C3 Statements** (roadmap phases 2 and 3, mirror depth) | Query operators by congruence (same operator, related inputs), `ORDER BY` and NULL ordering, `TOP`/`LIMIT` with the degenerate and tie-breaker lemmas, aggregates with their meaning, `INSERT`/`UPDATE`/`DELETE` including `UPDATE … FROM`, `MERGE` as a mirror where the target has it, generated keys, `RETURNING`; the converted DDL certified so Γ_B is output; the tactic-sized normaliser of the hybrid, for the non-literal lemmas | Set-based routines without error handling: most of the corpus's statements | 4 to 6 months |
| **C4 Errors and transactions** (roadmap phase 4) | `ErrorTxn.tla` ported as the core's CFG semantics; `XACT_ABORT`, `TRY/CATCH` to `EXCEPTION`, `@@ERROR` and return codes, `THROW`/`RAISERROR` to `RAISE` with the pack's code mapping, `BEGIN TRAN`/`COMMIT`/`ROLLBACK` and `@@TRANCOUNT`, savepoints, the per-statement exception scaffolding; T-SQL and PostgreSQL rows of the model as lemmas, Db2 for i rows as axioms | **Whole procedures.** This is the phase that reaches the target for real code; everything before it is preparation | 4 to 6 months |
| **C5 Idioms and the normaliser** (roadmap phases 2 and 5) | The semiring normaliser as the fallback gate; idiom passes as verified single-dialect rewrites with a user switch; the complete fragment as the `require` boundary; coverage ratchet; parser differential tests | Human-edited outputs and idiomatic outputs confirm; `require` by default per dialect pair | Ongoing |
| **C6 Db2 for i** (roadmap phase 6, decision 8) | C1 to C4 for SQL PL; W2 and W3 on rented IBM i | | 4 to 8 months |

Two things move earlier than in the roadmap: the typed target AST (C0,
Rust, decision A) and the error and transaction model (C4 before the
normaliser, decision B), because a procedure is never confirmable without
C4 and the normaliser contributes nothing to a mirror. One thing moves
later: the runtime normaliser and with it the `diff` gate for rewritten
queries, because the converter does not need it to prove its own mirror
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
   source, what is generated), before C4 writes the fourth copy. Working
   plan (2026-10-28):

   | Artifact | Role from C4 on |
   |---|---|
   | The Lean core and the dialect lowerings (`formal/lean/Sqldiff/Core`) | The semantics: the only definition of what a T-SQL or PostgreSQL error, rollback or handler does |
   | `profiles/semantics/*.toml` | Data with citations (error kinds, native codes, which settings exist). Generated into a Lean module the way `Syntax.lean` is generated from the Rust kinds, with a test that fails while it is stale, so the spec and the Rust code read one table |
   | `formal/profiles/error-txn.toml` cases | The shared test vectors: each scenario with its expected outcome runs on the Lean core's executable interpreter, on the Rust `ir/` interpreter and, until it is retired, on TLC; any disagreement fails CI |
   | `ErrorTxn.tla` | Ported to Lean in C4, then retired once the Lean core passes every case it passes |
   | `ir/` | The untrusted Rust twin: it finds witnesses for "different" and may stay faster than the Lean interpreter; it is held to the shared cases, never trusted for "equal" |

## Progress (branch `verification`)

**Status: experimental.** `sqldiff convert --proof report|require` and the
`sqldiff-check` checker are opt-in (`--proof` defaults to `off`) and change
no status when off. The checker covers a slice of T-SQL (the rows below);
on the WideWorldImporters sample (2026-10-28) it confirms 0 of 47 routines,
and 0 of the 26 corpus procedures, because every one uses something outside
the slice (queries, cursors, dynamic SQL, temporal tables). C3b
(2026-10-29) reads single-table queries on plain `int` tables; the corpus
count is unchanged (0 of 33 routines, each T-SQL file converted with the
corpus catalog), since its reads are in functions or over `varchar`
columns and tables with defaults and indexes. Treat a
confirmation as evidence within the slice, never as a release decision on
its own; the release gate (`--proof require`) is for trying the workflow.

| Item | State on 2026-10-28 |
|---|---|
| C0.1 Target AST and printer | Done for PostgreSQL. The abstract syntax moved to `sqldiff_syntax::abstract_syntax`, shared with the diff engine; `sqldiff_syntax::print` writes it as PostgreSQL (every non-atomic operand parenthesised, refusals instead of guesses); `sqldiff_syntax::routine` reads T-SQL and PL/pgSQL routines into one shape and prints PL/pgSQL. Round trips: 291 PostgreSQL statements and 20 PL/pgSQL routines in the repository print and read back as the same tree. The round trip found a false "equal" in the parser (PostgreSQL `\|\|` precedence, fixed) |
| C0 AST codegen | Done. `encode::SCHEMA` is a grammar; `formal/lean/Sqldiff/Ast.lean` (types and decoder) is generated from it with a schema hash in every request; every encoded tree of the repository is checked against the schema |
| C0.2 Converter emits AST | Not needed for the guarantee, so deferred: the certificate reads the output text on disk back with the target parser (item 4's re-parse check, applied to everything), so neither the rules nor a printer are trusted, and the proof is about what will run. Emitting AST stays useful for the trace's node paths and for idiom passes |
| C0.3 Complete trace | Done: every rewrite records a step (rule id, source span, instantiation), enforced by construction in the expression walk and by a corpus test; output byte-identical before and after |
| C0.4 Re-parse check | Subsumed by C0.2's decision above |
| C0.5 `--proof` on convert, certificate | Done: `sqldiff convert --proof report\|require` writes `certificates.json` (source and target AST hashes, renaming, residual assumptions, answer); unit statuses do not change yet. Every unit inside a certified routine also carries the answer as `proof` in `conversion-report.json` (`answer`, `theorem` or `reason` and `why` codes, `family:detail`, from the checker or `cert::reason_code`), and `totals.proof` (also `certificates.json`'s `summary`) counts routines, units and refusal codes; statuses and exit codes unchanged |
| C0.6 Deployment profile | Done: `[deployment] accept` and `facts`, validated against the registry; reported, never a verdict change |
| C0.7 Assumption table | Done: `profiles/assumptions.toml` (154 ids, each with what discharges it), `docs/assumptions.md` generated, completeness enforced by a source scan |
| C0.8 One model | Decided as a working plan (C0 item 8 above) |
| C0.10 Refusal diagnostics past the first unread statement | Done: `routine::read_collecting` leaves out each body statement the reader does not read, at any depth, with its message (a failure about the routine itself, its kind, header, options, parameters or table-valued result, stays a whole-routine error), and when a certificate's strict read stopped at such a statement the checker is asked about the trees without them; the certificate keeps its reason and is unconfirmed whatever the answer (`cert::never_confirmed`), its `why` the `read:*` code of every left-out statement plus the lowering's codes for the rest (`routine:`, `stmt:`, `node:`, `expr:`, `store:`, `type:`; `check:*` and `pair:*` dropped). WideWorldImporters, 47 routines, still 1 confirmed: 24 of the 30 routines stopped by an unread statement now name what the checker refuses after it (the other 6 have nothing it refuses), 18 distinct codes become 45; `proof blockers` before: `read:stmt:create (8), read:dynamic-sql (7), read:stmt:print (7), read:for-json (5), stmt:select:from:join (4), stmt:select:order (4), read:kind:view (3), read:routine-option (3)`; after: `read:dynamic-sql (13), read:stmt:print (13), stmt:select:table:unknown (12), stmt:select:order (11), read:stmt:drop (10), stmt:update:table:unknown (9), read:stmt:create (8), expr:var:global:fetch_status (7)` |
| C0.9 Rule proof registry | Done: `profiles/rule-proofs.toml` registers every converter rule id and rule-file head with what backs it (`lemma:` a section of `Lower.lean` whose rule reads the converter's output form, `axiom:V<n>` a vendor fact, or `engine`), `docs/proof-coverage.md` generated from it, completeness enforced by a source scan as for C0.7 |
| C1 Skeleton | Done: core language, fuel interpreter, `Refines` with divergence, the mirror check `Sqldiff.Mirror.check_sound` (standard axioms only), lowerings over the generated AST, `sqldiff-check/3`. End to end: a converted T-SQL procedure with IF, ELSE IF, WHILE and RETURN, which the diff engine can only call "not proven", is confirmed from the output on disk, under `ASM-C1-OPERATORS` (operators uninterpreted until C2) and `ASM-RETURN-CODE`. The converter now writes the T-SQL return status in its provable form (`return_code := 0` first, `COALESCE(e, 0)` on RETURN) |
| C2 Scalars | C2a done: 32-bit integers, NULL, three-valued logic, arithmetic errors (overflow, division by zero), COALESCE, ISNULL with int arguments, searched CASE, each rule with its vendor citation and probe status (`formal/lean/Sqldiff/Mirror/Lower.lean`); theorem `check_sound_c2a`. A confirmation in the slice no longer rests on `ASM-C1-OPERATORS`, only on what the slice names (`ASM-TSQL-ARITH-ERRORS`, `ASM-TSQL-ANSI-NULLS`); outside it, it also names `ASM-C1-NO-ERRORS`. With `SET XACT_ABORT ON`, or no expression that can raise, a converted procedure with IF, comparisons, ISNULL and CASE now passes the `--proof require` release gate once the deployment accepts what remains. C2b (strings) done on the Lean side, checker 0.6.0: `varchar(n)`/`nvarchar(n)` against `varchar`/`text`, ASCII literals, `+`/`||`, `RTRIM`, `LEN` against `length(rtrim(…))`, `COALESCE`/`ISNULL`/`CASE` over strings with every store proven not to truncate, and `=`/`<>` only under a trusted binary-collation fact (T-SQL `a = b` is `rtrim(a) = rtrim(b)`); residuals `ASM-CONCAT-NULL`, `ASM-BINARY-COLLATION`, `ASM-TSQL-LEN-BMP`, `ASM-TSQL-STRING-ARGS`. Rust sends the collation fact when the deployment accepts `ASM-BINARY-COLLATION` and records it on the certificate; a converted plain `=` on strings is then refused, as it should be (`'a' = 'a '` is TRUE in T-SQL only). A leading `SET CONCAT_NULL_YIELDS_NULL ON` discharges `ASM-CONCAT-NULL` in the release gate. Every C2b vendor quote is from memory (the vendor sites are not reachable from this environment) and no rule is probed yet. Converter consequence: PostgreSQL parameters carry no length, so a value read from a string parameter has no bound and cannot be stored into a `varchar(n)`; such routines stay on the C1 theorem |
| C3 Data statements | C3a done: single-table `INSERT ... VALUES`, `UPDATE` and `DELETE` on `int` tables in the core (tables as lists of rows, statement atomicity proven: `exec_error_atomic`), each rule with its citation and probe status; column against variable resolution refuses every name PL/pgSQL could read either way. The observation after an error no longer carries the database (`ASM-STATE-AFTER-ERROR`: T-SQL keeps earlier writes that PostgreSQL rolls back with the failed `CALL`). Rust sends a table only when the source and output trees each declare it once with a plain `CREATE TABLE` (`sqldiff_syntax::plain_table`: `int` columns, explicit nullability, a primary key, nothing else) and nothing else in either tree names it, under `ASM-TABLES-AS-DECLARED`. End to end, a converted procedure with INSERT, UPDATE and DELETE is confirmed with `statements: c3a`. **C3b** (checker 0.10.0, branch `c3-queries`): single-table reads of a plain table in the shapes the converter writes. The core gained one statement, `query t c pick` (assign from the kept row, count the kept rows, test for one), with every core lemma extended (renaming simulation, operator independence, fuel, leaf atomicity, the C4 settling; `exec_setDb` now also asks for no query, and a handler that reads is no `failHandler`), standard axioms only. The rules: T-SQL `SELECT @x = col, ... FROM t WHERE c` against the converter's PL/pgSQL block `DECLARE r record; BEGIN FOR r IN SELECT col AS a ... LOOP x := r.a; END LOOP; END;`, and `SET @x = (SELECT col FROM t WHERE c)` against `x := (...)`, both only when `c` fixes every primary-key column with `=` (at most one row; neither vendor says which row several would assign, and the scalar form raises 512 against 21000), with values that read no variable and cannot raise and distinct `int` targets `c` does not read; `IF [NOT] EXISTS (SELECT 1|* FROM t [WHERE c])` as the whole `IF` condition; `COUNT(*)` into an `int` variable (`SELECT @x = COUNT(*)` against `SELECT COUNT(*) INTO x`, or the scalar form), naming `ASM-TSQL-ARITH-ERRORS`. Procedures only; joins, aliases, hints, `TOP`, `ORDER BY`, `GROUP BY`, `DISTINCT`, `WHILE EXISTS`, `EXISTS` inside a larger condition and PL/pgSQL `SELECT col INTO x` stay outside. The abstract syntax gained `Node.ForQuery` (PL/pgSQL `FOR r IN SELECT ... LOOP`; schema hash changed); the Rust side now sends the plain tables a routine only reads (`cert.rs`, `read_tables`) under the same `ASM-TABLES-AS-DECLARED`, whose statement now says "writes or reads"; reads add no `ASM-OBSERVER-ROWCOUNT`; no new residual id. A confirmed answer whose routines read a table carries `statements: c3b`. **Probed** on SQL Server 2022 and PostgreSQL 18 (`eval/queries`, Q1 to Q10): no row keeps the variable, several rows give an unspecified one (both engines), 512 against 21000, `EXISTS` never UNKNOWN and its select list never evaluated (both engines, also a constant `1/0`), `COUNT` `int` against `bigint`, no rows-affected count from any of the read shapes, 42702 for a name that is a column and a variable, PL/pgSQL `SELECT INTO` setting NULL on no row; the vendor quotes in the rules are from memory. End to end, a converted procedure with all four shapes over a plain table is confirmed by `check_sound_c2a` (`crates/sqldiff-cli/tests/proof.rs`), and the same with a non-key `WHERE` is refused. Corpus coverage (each T-SQL corpus file converted with the corpus catalog, `--proof report`): 0 of 33 routines confirmed before and after; four routines that read a table by key (`p03`, `p05`, `m10`, `m19`) moved from "the output routine is outside the verified shape" to the checker, which refuses them for what C3b leaves out (functions, `varchar` columns, tables that are not plain). **C3c** (checker 0.11.0, branch `c3c-wider-tables`): wider plain tables, the core unchanged. A catalog table may have T-SQL `varchar(n)`/`nvarchar(n)` columns (`1 ≤ n ≤ 4000`) against PostgreSQL `varchar(n)` of the same `n`, read with the column's C2b bound (`wide` for `nvarchar`; `=` only under the collation fact, which now covers columns) and written by `INSERT`/`UPDATE` only with values that provably fit (a too-long value is 2628 or a silent truncation under `ANSI_WARNINGS OFF` in SQL Server, 22001 in PostgreSQL: outside the spec, not an error of equal class); a C3b pick may read a string column into a string variable it fits. String key columns stay outside (SQL Server pads key values). Constant defaults (an integer, or a slice string literal that fits), the same in both DDLs, are added by both lowerings to an `INSERT` that leaves the column out. A non-unique index without filter or options no longer keeps its table out when no entry can exceed the engines' size limits (SQL Server 1700/900 bytes, PostgreSQL btree about 2704 bytes with `INCLUDE` counted; checked from declared lengths in `plain_table::PlainIndex::fits`). Request: new SPEC types `v<n>`/`w<n>` and a fourth column field for the default (`i<int>`, `s<hex>`); old TABLE entries decode unchanged. New residual id `ASM-TSQL-ANSI-PADDING` (Rust-side: a routine writing a table with a T-SQL `varchar` column; a T-SQL tree that turns `ANSI_PADDING` off keeps such tables out); `ASM-TABLES-AS-DECLARED` and `ASM-BINARY-COLLATION` now also cover column types, defaults, indexes and column collations. **Probed** on SQL Server 2022 and PostgreSQL 18.6 (`eval/tables`, T1 to T9): storage and read-back with trailing spaces, 2628 against 22001 and the excess-spaces truncation on both, `ANSI_PADDING OFF` trimming `varchar` but not `nvarchar`, defaults (also: one too long fails the `INSERT`), `=` padding on a BIN2 column against exact PostgreSQL, a string key `'a'`/`'a '` duplicate in SQL Server only, a non-unique index changing nothing, the index entry limits (1946, 54000), a read into a shorter variable (truncation against 22001); the vendor quotes are from memory. End to end, a converted procedure over a table with a `varchar` and an `nvarchar` column, two defaults and a non-unique index (insert with defaults, update, read by key) is confirmed by `check_sound_c2a` with `statements: c3b`; a value that may not fit, and a `UNIQUE` index, are refused (`crates/sqldiff-cli/tests/proof.rs`). Coverage unchanged: corpus 0 of 33 routines (with the corpus catalog), WideWorldImporters 1 of 47 (a C1 answer, no table) before and after, and no table of either reaches the checker: the corpus tables arrive through `--catalog`, which the table reader does not see, and even in the tree each is named by a foreign key, has `char`, `decimal`, `datetime2` or identity columns, or a `UNIQUE` index; PostgreSQL string parameters still carry no length (C2b), so routines that insert a parameter into a `varchar` column stay outside. **C2c** (checker 0.14.0, branch `c2c-typed-values`): typed values beyond `int` and strings, part 1. The core gained integer widths (`arith`/`neg` carry 16, 32 or 64 bits and overflow at that width), bit/boolean literals, dates (days since 0001-01-01, proleptic Gregorian, `Sqldiff/Core/Calendar.lean`) and timestamps (microseconds), comparisons of two dates or timestamps, `DATEADD` by day/month/year with the month-end clamp, `DATEDIFF` by day, the date of a timestamp and a date at midnight; the renaming and "cannot fail" lemmas gained the cases, every theorem keeps its statement, standard axioms only. The rules (`Lower.lean`, "The C2c slice: typed values", each cited and probed on SQL Server 2022 and PostgreSQL 18, `eval/values` V1 to V8): T-SQL `smallint`/`int`/`bigint` against PostgreSQL `smallint`/`integer`/`bigint`, arithmetic at the wider width (two `smallint`s give a `smallint` on both, division of two only by a literal other than 0 and -1: `-32768 / -1` is 32768 on both without an error), narrowing stores only when the value provably fits (as string stores), `COUNT(*)` still into `int` only (into a `bigint`, T-SQL's `int` count overflows at 2^31 rows and PostgreSQL's `bigint` count does not: refused, contrary to the claim's plan), a `bit`, date or timestamp stored into an integer refused (T-SQL stores a bit as 0/1, PL/pgSQL raises 22P02; this was a false "equal" on the C1 path before C2c); a T-SQL constant beyond the `int` range is `numeric`, so it is in the slice only as a `bigint`'s stored or compared value; `bit` against `boolean` stored from `0`/`1` against `FALSE`/`TRUE` and read as `@b = 1`, `= 0`, `<> 1`, `<> 0` against `b = TRUE` etc., the converter's forms, three-valued alike; `date` and `datetime2(p)`, `p ≤ 6`, against `date` and `timestamp(p)` with the ISO 8601 literals only (`'YYYY-MM-DD'`, `'YYYY-MM-DDThh:mm:ss[.f]'`, independent of `DATEFORMAT`, `LANGUAGE` and `DateStyle`: probed, so no new assumption), a literal with more digits than kept outside (the engines round differently at a half: `.1234565` is `.123457` against `.123456`), comparisons, a date into a timestamp (midnight), `CAST(x AS date)`, `DATEADD(day|month|year, n, d)` against `(d + n)`, `(d + make_interval(months => n))::date`, `(t + make_interval(days => n))` with `ASM-DATEADD-RANGE` (517 against a value past 9999-12-31) and, for years, `ASM-DATEADD-YEAR-END`; `DATEDIFF(day, a, b)` against `((b)::date - (a)::date)`. Typed catalog columns with defaults in those forms (SPEC `j`, `h`, `b`, `d`, `t<p>`; `plain_table::ColType`, `cert.rs::pair_type`), date and timestamp keys; signatures pair `bigint`, `smallint`, `bit`/`boolean`, `date`, `datetime2(6)`/`timestamp(6)`. Outside: `datetime2(7)`, `datetime`, `time`, `datetimeoffset`, `tinyint`, `decimal` (part 2), clock reads, other dateparts, a timestamp stored into a date, `datetime2(p < 6)` parameters. Refusal codes `type:<name>`, `expr:date-literal`, `expr:bit-literal`, `expr:dateadd:part`, `expr:datediff:part`, `expr:div:smallint`. End to end, a converted procedure with a `bigint` and a `bit` variable, a date column written from a literal and compared, and `DATEADD` is confirmed by `check_sound_c2a` (`crates/sqldiff-cli/tests/proof.rs`); WideWorldImporters stays at 1 of 47 (its blockers are elsewhere: result sets, dynamic SQL, `PRINT`, `FOR JSON`, `ORDER BY`, joins). The review of C2c found five false "equal"s, all fixed with regressions: PostgreSQL `date + make_interval(...)` is a `timestamp`, so only its `::date` cast is a date; a call's arguments were not compared with the callee's parameter types (an `int` into a `smallint` or `boolean` parameter finds no procedure in PostgreSQL), so the request now carries each callee's parameter types and an argument must have exactly its parameter's type (this was open on the C1 path before C2c too); the bit comparisons dropped `ASM-TSQL-ANSI-NULLS`; a typed value stored into a variable of another type (a `datetime2(6)` through T-SQL `datetime` and PostgreSQL `timestamp` locals) and variables of differing declared types are now refused on every path; `ASM-TSQL-STRING-ARGS` now says every argument is a value of its parameter's type and is named for `smallint`, `bit`, `date` and `datetime2` parameters too. A claim that T-SQL types `-2147483648` as `numeric` was probed and not confirmed (SQL Server 2022: `int`, and the reviewer's expression raises 8115 against 22003). The re-review found two more, both fixed with regressions: a variable of a type outside the slice corresponded to one of the same name, and a string was stored into it, though the same name is not the same type (T-SQL `time` is `time(7)`, PostgreSQL's `time(6)`: `'12:00:00.1234567' < '12:00:00.1234568'` is 1 against 0, through the converter; `numeric`, `money` likewise), so no type outside the slice corresponds to any and only NULL or a string into a string type stores into one; and a string argument was not held to the callee's declared length (T-SQL truncates `abcdef` to a `varchar(3)` parameter's `abc`, PostgreSQL keeps it), so it must now provably fit that length and come from no `nvarchar`, and a parameter without a length takes none. `ASM-TSQL-STRING-ARGS` is now named for `int` and `bigint` parameters too (an outside caller's `1.5` is 1 in T-SQL). **C3d** (relational core, claimed 2026-10-30): queries over several catalog tables, then aggregates, ORDER BY with TOP, and writes from queries in later parts. **Part 1 done** (checker 0.16.0, branch `c3d-blocks`): query blocks in the C3b statement shapes. The core's read is now `query q pick` over a block (`Core.Block`: the catalog tables of a `FROM`, their product in nested-loop order kept where every `ON` and the `WHERE` are TRUE, column `c` of the source at position `i` read as `i.c`, so aliases need not correspond); a scalar subquery with more than one row fails with the new error kind `cardinality` (T-SQL 512, PostgreSQL 21000). The key closure (`Block.atMostOne`: every primary-key column of every table equated, with the core's one-kind `=`, to an expression over tables already determined, variables the pick does not assign and constants) is what `SELECT @x = ...` and its `FOR` block need; `Core/Fidelity.lean` proves `kept_length_le_one` (at most one tuple on a database whose keys are distinct and required columns non-NULL), and the lowering of `Pick.row false` cites it. Every core lemma gained the block (renaming simulation, operator independence, fuel, leaf atomicity, the C4 settling); `check_sound` and `check_sound_c2a` keep their statements; standard axioms only. One shared query reader (`querySpec`) now serves the lowering, the store check, the slice check and the diagnostics. The rules (`Lower.lean`, "The C3d blocks", vendor quotes from memory): catalog tables with or without aliases in comma lists, `[INNER] JOIN ... ON` and `CROSS JOIN`; an `ON` sees only its own join's tables; reference names ASCII and distinct (T-SQL up to case); `q.c` resolves to the one source named `q` (T-SQL spelt exactly; PostgreSQL folded and never a variable, the `FOR` record or the routine's name); an unqualified name to the one source that has the column (in PostgreSQL never also a variable or a reference name); `COUNT(*)` and `EXISTS` over any block; the scalar subquery over any block, counting as able to fail unless the block is closed. **Probed** on SQL Server 2022 (16.0.4305.5) and PostgreSQL 18.6 (`eval/queries/blocks-*`, Q11 to Q13): ON against WHERE pooling, products, NULL join keys, self-joins, picks by key across joins; 209/42702, 4104/42P01 (an aliased table's own name, an `ON` naming a later table or another comma item), 1013/42712, case in a case-sensitive SQL Server database (4104, 207), the PL/pgSQL qualifiers (a record 42702, the loop's record 55000, the routine's name 42702; an integer variable reads the column, refused all the same), a column that is a variable 42702; 512/21000 with the variable kept and the error ending an `XACT_ABORT ON` procedure. Rust: the certificate sends every table of a join (`cert.rs`, `from_tables`); no new residual id. New reason codes `stmt:select:many-rows` (in place of `stmt:select:where:not-key`), `stmt:select:from:join:<kind>`, `stmt:select:from:qualifier`, `stmt:select:from:ref`, `name:ambiguous`, `name:column-variable`. By design two C3b expectations change: a one-table query with an alias, and a scalar subquery whose `WHERE` does not fix the key, are in. End to end, a converted procedure reading two tables through a join by key (with a scalar subquery over an inner join, a count over a comma list and `EXISTS` over a cross join) is confirmed by `check_sound_c2a`, and the same joined on a column that is no key is refused (`crates/sqldiff-cli/tests/proof.rs`). WideWorldImporters stays at 1 of 47 routines (5 of 843 units): its six routines refused for a join or an alias move to deeper codes (`stmt:select:order` for four result sets, `stmt:select:from:join:left`, `stmt:select:table:unknown`), and `stmt:select:from:join` (4) and `stmt:select:from:alias` (2) leave the blockers. Review of C3d fixed two pre-existing holes, each with a regression: a table named in another case than its DDL (`dbo.ORD` for `dbo.Ord`, 208 in a case-sensitive SQL Server database, folded by PostgreSQL) was sent and confirmed, and is now sent only when spelt exactly as declared, whatever the collation; and a routine that can fail now names `ASM-NO-CALLER-TXN` with or without a write, since under `XACT_ABORT ON` its error dooms a caller's T-SQL transaction and loses the caller's earlier work where a PostgreSQL exception block keeps it (probed, `eval/queries/caller-txn-*`, Q15). Left outside rather than weaken a rule: read-only tables (a foreign key, `UNIQUE`, `CHECK`, identity, defaults the routine never reads), which keep WideWorldImporters' and the corpus's tables out of the catalog; outer joins; an `ON` or `WHERE` that can raise (C1 path only); a qualifier that is a PL/pgSQL integer variable (PostgreSQL reads the column, probed, but the rule refuses every variable). Next: multi-table forms; string parameter lengths. **Part 2 done** (checker 0.17.0, branch `c3d-tables`): catalog tables. A table both routines only read is a **read-only** catalog table (`CatTable.readOnly`): no `INSERT`, `UPDATE` or `DELETE` writes it (`writeTableOf`, theorem `writeTableOf_not_readOnly`; reason code `stmt:<kind>:table:read-only`), so it may carry foreign keys, `UNIQUE`, `CHECK`, identity, defaults of any kind, computed and generated columns, indexes, triggers and system versioning, and columns of types outside the slice, which reach the checker as **opaque** columns (`OpqCol`): names only, never read (`stmt:select:column:opaque`, on the C1 path too), but counted in name resolution (an unqualified name that is an opaque column of another source is ambiguous, 209/42702; a PL/pgSQL variable spelt as one is the column-variable conflict, 42702). The core is unchanged: it reads the table's projection on the slice columns, multiplicity kept, with the primary key only when it is wholly visible (`CatTable.keyVisible`, theorem `CatTable.keyVisible_of_hasKey`: a key with an opaque, string or `bit` part gives no key, so `SELECT @x = ...` cannot close the table, while `COUNT(*)`, `EXISTS` and the scalar subquery still read it); a call stays the world's effect, which may change any table. `check_sound` and `check_sound_c2a` keep their statements; standard axioms only (27 theorems in the gate). Request: a TABLE entry may end in `=r`, a column may have SPEC `o`; an older checker answers "the catalog does not decode". **Probed** on SQL Server 2022 (16.0.4305.5) and PostgreSQL 18.6 (`eval/queries/tables-*`, Q16 to Q19): reads of a table with all of the above give what its plain twin gives (`COUNT`, `EXISTS` with `SELECT 1` and `SELECT *`, a pick by key across a join, a scalar subquery), and a raising computed (8134) or virtual generated (22012) column raises only where a query names it; a system-versioned table's plain `FROM` gives its current rows (`FOR SYSTEM_TIME` reads as a table hint and is refused); a security policy's filter predicate and forced row-level security drop a `COUNT` from 3 to 1, which is why they keep a table out; 209/42702 with an opaque column, 42702 for a variable spelt as one; the vendor quotes are from memory. Rust: `plain_table::read_only` reads any permanent `CREATE TABLE` that cannot change a plain read (each column of the slice or opaque: another type, `COLLATE`, masking, encryption, computed, period and generated columns; the declared primary key, none when anything is `DEFERRABLE`); `plain_table::leaves_reads` lists the statements that name a table without changing its reads (indexes but `DROP_EXISTING`, `ALTER TABLE` adding one constraint or switching constraint checks and triggers, PostgreSQL triggers and views, `COMMENT ON`, extended properties; such an index or view, and another table's definition, count only by the name they define, which may hide a table on PostgreSQL's `search_path`, and a table that inherits or is a partition counts in full); anything else naming the table keeps it out (security policies, row-level security, rules, column changes, renames, drops, grants, data written at the top level, DDL in a routine body, the name in a string, dynamic SQL anywhere). `cert.rs` sends such a table read-only when each side defines it once and the two definitions pair by column name (a column whose types do not pair is opaque; `NOT NULL` only where both sides say so; key flags only when both declare the same key). The trees now include the base catalogs' DDL (`--catalog`; `--catalog-b`, or without it the base catalog as converted, leaving out suspect objects). `ASM-TABLES-AS-DECLARED` now names base catalogs and states what it asserts of a read-only table; no new id, no new answer field. End to end, a converted procedure reading such tables (by key across a join, `COUNT(*)`, `EXISTS`, a scalar subquery), declared in the tree or through `--catalog` with and without `--catalog-b`, is confirmed by `check_sound_c2a`; a write, a read of a `decimal` column, a table under a security policy and a `--catalog-b` table of another shape are refused (`crates/sqldiff-cli/tests/proof.rs`). Measured (each T-SQL corpus file with the corpus catalog, `--proof report`; WideWorldImporters as one tree): corpus 0 of 33 routines confirmed before and after; routines refused for `stmt:*:table:unknown` 21 before, 4 after (three read a temporary table or table variable, one deletes through an alias); 13 now stop at a write to a read-only table (`stmt:insert:table:read-only` 10, `stmt:update:table:read-only` 7: the corpus routines write tables with foreign keys or identity, outside this part), the rest at functions, views, `MERGE`, transactions and lookups by a `UNIQUE` column (`stmt:select:many-rows`). WideWorldImporters unchanged at 1 of 47 routines (5 of 843 units), `stmt:select:table:unknown` 13 and `stmt:update:table:unknown` 9: 13 of its procedures run dynamic SQL, which keeps every table of the tree out (an existing rule, kept). A PostgreSQL `CREATE [OR REPLACE] [CONSTRAINT] TRIGGER ... EXECUTE FUNCTION f()` (the converter's temporal and DML triggers) no longer counts as dynamic SQL (`cert.rs`, `dynamic_sql`): it names the function it runs, whose body is read as a routine of its own; `EXECUTE` anywhere else in PostgreSQL still does (in PL/pgSQL `EXECUTE function` runs a variable's text), and so does an event trigger. Left outside: writes to tables with constraints, identity or expression defaults; `UNIQUE` as a closure step; string keys; `ALTER TABLE ... SET (SYSTEM_VERSIONING ...)` (not probed); aggregates, `ORDER BY` with `TOP` and writes from queries (later parts). **Proof coverage** (branch `proof-ledger`, claimed 2026-10-30): a registry of every converter rule id with its proof status, a fixed taxonomy of the checker's refusal reasons with a per-corpus histogram, and a per-unit `proof` field in the conversion report, so that a unit `converted` by the engine and one `confirmed` by the checker are told apart; statuses and exit codes unchanged **Caller's `SET ROWCOUNT`** (checker 0.15.0): a caller's `SET ROWCOUNT n` is inherited by the T-SQL routine and limits the rows its `INSERT`, `UPDATE`, `DELETE` and `SELECT` statements touch (probed, `eval/session`: one row of three updated, deleted or inserted, where PostgreSQL, which has no such setting, touches all three), so every confirmation with `statements: c3a` or `c3b` now names `ASM-TSQL-ROWCOUNT-0` (the checker's residual; the Rust side adds it on the C1 path and for an older checker), a deployment fact. |
| C4 Errors and transactions | C4a done on the Lean side, checker 0.7.0. The abstract syntax reads T-SQL `TRY`/`CATCH`, `THROW`, `BEGIN TRAN`/`COMMIT`/`ROLLBACK` and PL/pgSQL nested `BEGIN ... EXCEPTION WHEN ... END` blocks, `RAISE`, `COMMIT`/`ROLLBACK` (`Node::Try`, `Throw`, `Raise`, `Txn`; schema hash changed). The core gained `tryCatch undo body handler`, `raise`, `reraise`, `commit` and the error kind `user`; T-SQL `TRY` lowers to `tryCatch false`, an `EXCEPTION WHEN OTHERS` block to `tryCatch true` (PostgreSQL rolls the block back, V18). The check does not assume the rollback away: it settles `undo` where it is proven to change no outcome (a body that cannot fail after a write, `exec_settle`) or only the database of a failed call (a handler that always fails without touching the database, outside other handled bodies, `exec_settleTop`), and refuses the rest; `check_sound`/`check_sound_c2a` keep their statements. `THROW n, 'm', s` against `RAISE EXCEPTION` (P0001) and `THROW;` against `RAISE;` are in; so are `BEGIN TRAN ... COMMIT` as a bracket under `XACT_ABORT ON`, the rollback-and-rethrow idiom (lowered to its body, as the converter writes it) and PL/pgSQL `COMMIT` in an invoker-rights procedure (V17). Outside: `RAISERROR`, the error functions and `SQLSTATE`/`SQLERRM`, condition-specific handlers, calls inside handled blocks, savepoints, `ROLLBACK` outside the idiom, nesting. New residuals `ASM-CATCH-CORE-ERRORS`, `ASM-NO-CALLER-TXN`, `ASM-THROW-ERRCODE`; a confirmed answer carries `"errors":"c4"`. End to end, the converter's output for a handled division, a duplicate key re-raised with `THROW`, `CATCH THROW;` after two writes, the rollback-and-rethrow idiom and `BEGIN TRAN ... COMMIT` is confirmed by `check_sound_c2a`; a `TRY` that writes, can then fail and carries on is refused. Rust registers the residuals and records `errors: c4`; a c4 confirmation discharges the converter's `ASM-TRY-SUBTRANSACTION`. **Probed** on SQL Server 2022 and PostgreSQL 18.6 (`eval/errors-txn`, P1 to P13): every behaviour the C4 rules claim was observed, and P12/P13 confirm two C3a claims (statement atomicity, `UPDATE` reading old values). One behaviour the rules did not state: a T-SQL caller that catches an error raised inside a routine's own `BEGIN TRAN` gets the doomed transaction back (`XACT_STATE() = -1`, writes fail with 3930 until it rolls back), where PostgreSQL leaves none; a confirmation of such a routine now names `ASM-TSQL-DOOMED-TXN`. The quotes themselves are still from memory outside V10/V17/V18 Checker 0.8.0, review fixes (all conservative, no new residual id): parameter and return types are compared between the sides (`sigCorr`: `int` with `integer`/`int`/`int4`, a C2b string type with a C2b string type, and on the C1 path only `bigint`/`smallint`; anything else is refused); a parameter default must be NULL or a slice value that fits its declared type (T-SQL truncates `'abcdef'` into `varchar(3)`, PostgreSQL drops the length and keeps it); a call counts as able to fail (a callee's error), so a routine with one is a `c2a` answer only under `SET XACT_ABORT ON` and names `ASM-STATE-AFTER-ERROR`; `ERRCODE` must be `'P0001'` exactly (`'p0001'` raises 42704); the unguarded `ROLLBACK; THROW;` needs a body that is exactly one bracket (with two, an error between them finds no transaction and the `ROLLBACK` raises 3903, probed), and the idiom is outside the spec inside `IF`, `WHILE` or a nested block Checker 0.9.0: every string store (assignment, `DECLARE` default, function `RETURN`, `OUTPUT`/`INOUT` copy-back) must provably keep its value on the C1 path too (the slice's `fitsTy`, or an unbounded target: PostgreSQL `text`/`varchar`, T-SQL `nvarchar(max)`), else the routine is outside the spec; before, T-SQL truncation against PostgreSQL's error or keep was confirmed under `ASM-C1-OPERATORS` |

Findings on the way, fixed: the PostgreSQL `||` precedence (a false "equal"); `convert` reported units as "converted" while the engine's diff named pair-level assumptions (rows-affected counts, untyped stores, binding without a catalog); the C1 lowering's review found three holes before merge (PostgreSQL name folding, undeclared names read as NULL, `EXEC @rc =` status position). The C3a review found that the routine reader dropped a T-SQL `OUTPUT` clause, a `SET` target's qualifier, `INSERT` table hints and `FOR JSON` options from the tree the checker sees; it now refuses them (the diff engine was not affected: it compares the tokens too). The C2b wiring found that `ASM-CI-COLLATION` asked only for consistent letter case; T-SQL also ignores trailing spaces in comparisons and PostgreSQL does not, so accepting it accepted conversions that differ on `'a' = 'a '`. Its statement and the converter's note now name both conditions.

## Decisions this needs

Decided 2026-10-28 by the project owner: the converter is the priority of
the verification project. The following follow from it. On the same day
the owner asked for the work to be reprioritised around this document and
carried out as fast as possible, so the recommendations below are the
working plan on the `verification` branch from 2026-10-28; each can still
be overruled, and the table says which are in progress.

| # | Decision | Recommendation |
|---|---|---|
| A | The converter gets its own proof path (target AST, trace, certificate, re-parse check; C0) | Yes; required for the target in any order |
| B | Errors and transactions before the runtime normaliser, with the tactic-sized normaliser in C3 | Yes, with the hybrid; the `diff` gate for rewritten queries moves to C5 |
| 9 | Spec structure: one typed core whose semantics each dialect's lowering defines, or a semantics per dialect plus proven lowerings (decision 2 as written) | One core; the lowering rules are the reviewed spec. Fewer artifacts, one executable oracle, and `canon.rs` already has this shape |
| 10 | The converter's default output: provable (mirror with explicit scaffolding) or idiomatic | Provable by default; idioms as verified, switchable passes. The GUI shows both |
| 11 | A certificate (rule trace) for `convert`, beside the two-AST protocol decision 2 fixed for `diff` | Yes: the trace is untrusted hints over the same protocol, so decision 2 stands for `diff` |
| 12 | Target AST and printer in Rust, converter migrated from text to AST | Yes, in C0; it is also the only way the GUI's live conversion and the tree converter can share one checked path |
| 13 | Phase order as in "Phases" above (C0 to C6), the detail of decision B | Yes; the diff tool's phase 2 moves to C5 |
| 14 | Assumption discharge: a deployment profile that absorbs named assumptions, and `--proof require` as the release gate for converted trees | Yes; this is where "no re-review per routine" becomes a CI exit code |
