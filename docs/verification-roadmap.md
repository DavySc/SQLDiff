# Verification roadmap

Status: plan, nothing below is implemented yet. Written 2026-10-28.

How SQLDiff gets from tested to machine-checked: a proof that an "equal"
verdict is right, for a stated fragment of SQL, against a stated semantics of
each supported RDBMS version. The first rule of the design (never a false
"equal") is the property being proven; everything else (witnesses,
explanations, conversion, GUI) stays outside the proof.

## Assurance tiers

| Tier | What it means | Status |
|---|---|---|
| A | Tests, fuzzing, mutation gate, execution oracle, TLA+ model of errors and transactions | In place ([validation.md](validation.md), `formal/`) |
| B | Machine-checked soundness of a small kernel in Rust | Not planned on its own: its refactors become phase 0 below |
| C | A proof, in Lean 4, that a confirmed "equal" holds under a formal semantics of each dialect version, for a growing fragment | This plan |
| D | seL4 level: every component verified, parser and engine semantics included | Out of reach: no formal semantics of T-SQL, PostgreSQL or Db2 for i exists to verify against |

## Trust boundary today

`derive_verdict` (`crates/sqldiff-core/src/session.rs`) returns "equal" when no
finding is `different` or `unknown` and no unknown reason is recorded. "Equal"
is therefore the absence of an objection, and every component whose silence
counts is trusted:

| Trusted for "never a false equal" | Why |
|---|---|
| `equiv/` lowering and comparison: `canon.rs`, `ast.rs`, `lower.rs`, `compare.rs`, `term.rs`, `literal.rs`, `options.rs`, `schema.rs`, `table.rs`, `ddl.rs`, `call.rs`, `trigger.rs`, `script.rs`, `mod.rs` | Decide statement equality (`compare_pair`), table, index and call equality |
| `treediff.rs`, `normalize.rs`, `classify.rs`, `tokens.rs`, `equiv::refine_modified` | Drop changes as cosmetic or structural |
| `ir/behaviour.rs`, `interp.rs`, `cfg.rs`, `profile.rs` | Never say "equal", but are the only detectors of transaction and error differences |
| `bind/`, `catalog/`, `ledger.rs`, `composition.rs`, `engine::pair_writes`, `sqldiff-syntax` | Detectors and the parser |

Not trusted for it: `equiv/eval.rs`, `edit.rs`, `xwit.rs` (they only turn
"unknown" into "different"), lints, `rules/analysis.rs`, and the crates that
only consume verdicts. Roughly 35 to 40 thousand lines are trusted, too many to
verify as they are.

## Target: a verified gate

The statement to prove, for dialect versions `vA` and `vB`, artifacts `a` and
`b`, catalog Γ and assumptions S:

> If SQLDiff reports equal under S, then for every pair of related database
> states satisfying Γ and every pair of related inputs, if S holds, running `a`
> on `vA` and `b` on `vB` gives related observations.

"Related" and "observations" are decision 1 below.

Design: translation validation.

1. Rust parses both sides and exports the AST, the catalog facts and the
   dialect versions as JSON.
2. `sqldiff-check`, a Lean 4 program compiled to a native binary, lowers the
   AST with a lowering proven to preserve the dialect semantics and compares
   the terms.
3. The report says "equal" only when the checker confirms it. Rust "equal"
   that the checker does not confirm becomes "unknown" (in strict mode) or
   carries `proof: unconfirmed` (during rollout).

The Rust canonicaliser, witness search and explanations are unchanged and
untrusted. Lean is chosen for executable semantics (usable as a test oracle),
proof automation, `#print axioms`, and prior use of the same pattern (AWS
Cedar: a Lean model with proofs, plus differential testing of the Rust
implementation).

Trusted at the end: the Lean kernel and compiler, the dialect specification
(reviewed, and tested against the engines), the named axioms (unverified
vendor facts), the Rust parser and the AST export.

## Open decisions

| # | Decision | Recommendation | Status |
|---|---|---|---|
| 1 | What "related observations" means (below) | Contextual equivalence over a stated set of observers | In progress |
| 2 | Gate (translation validation) or proving the Rust | Gate, then converge per fragment (see "Drift management") | Proposed |
| 3 | Rollout | `proof:` field first, strict mode once coverage is high | Proposed |
| 4 | Access to IBM i systems | One per supported release, for probes | Open |
| 5 | Who reviews spec, statements and axioms | A named human; the one role AI does not fill | Open |
| 6 | Version support policy | Follow each vendor's lifecycle (see "RDBMS versions") | Proposed |
| 7 | Azure SQL Database and Managed Instance | Out of scope until decided: versionless, compatibility level only | Open |

### Decision 1: observational equivalence

Three parts: what a caller observes, which contexts are quantified over, and
how values and states correspond across dialects; plus how nondeterminism is
treated. Facts already fixed by the current tool, checked with
`sqldiff diff` on 2026-10-28:

- An output column alias rename (`AS OrderId` to `AS Id`) is reported
  unknown, so result column names are treated as observable.
- Adding `SET NOCOUNT ON` is reported different, so "rows affected" counts
  are treated as observable. If decision 1 leaves them unobserved, this
  becomes a false "different".
- Adding `ORDER BY` to an unordered result is reported unknown, consistent
  with observing a result as a bag unless both sides order it totally.
- The execution oracle observes the error that reached the client, returned
  rows, and durable state read in a fresh session; the TLA+ model observes
  committed and pending work, the error, early stop, return code,
  `@@TRANCOUNT` and a doomed caller transaction.

The remaining choices (error granularity, messages, row counts, generated
keys, nondeterminism, environment faults, value correspondence) are worked
out with the project owner and recorded here.

## AI operating model

AI writes proofs; it never writes trusted statements unreviewed.

| Artifact | Written by | Checked by |
|---|---|---|
| Proof bodies | AI | The Lean kernel |
| Theorem statements (`Statements/*.lean`) | AI drafts, human approves | `theorems.lock` (statement hashes); CI fails on a change not in the lock |
| Dialect specification (`Spec/*.lean`) | AI drafts with a citation per rule, human approves | Engine differential tests, the lock |
| Axioms | Human only | `#print axioms` checked against an allowlist; each maps to a `V*` fact |
| `sorry` | Nobody | CI rejects it |

Agents (in `.claude/agents/`):

- `prover`: edits proof bodies only and runs `lake build`; a PreToolUse hook
  blocks edits to `Spec/`, `Statements/` and `theorems.lock`. After repeated
  failure it looks for a counterexample (Lean `plausible`, or the SQL oracle)
  instead of weakening the statement; a counterexample is a finding.
- `spec-drafter`: drafts spec rules and lowering rules with citations.
- `spec-breaker`: generates programs, runs them through the Lean semantics
  and the real engines, and reports every disagreement.
- `sync`: follows Rust changes into the Lean lowering (see "Drift
  management").
- `audit` gains `lake build`, the lock, axiom and `sorry` checks, and the
  Rust/Lean differential suite.

AI shortens proof work most; drafting the specification less; deciding and
reviewing the semantics hardly at all. Review is the critical path.

## Phases

Estimates are elapsed time for one engineer with AI agents, with wide error
bars; total about 16 to 28 months including multi-version support.

| Phase | Scope | Exit criterion | Estimate |
|---|---|---|---|
| 0 Groundwork | Private `Report.verdict` (assigned today in core, source, review and convert); a coverage invariant (every unit proven or covered by a finding); tests for the leads below; typed dialect version (see "RDBMS versions"); `formal/lean` Lake project with `Term`; AST JSON export with schema hash; the gate on a trivial fragment; CI and agents | Gate confirms the trivial fragment on the corpus | 2 to 3 months |
| 1 Scalar expressions | Three-valued logic, NULL, numeric types, strings and collation, casts and literals, bit and boolean, CASE/COALESCE/ISNULL; `compare.rs` hooks proven under their assumptions; `eval/casts`, `eval/bit-boolean`, `eval/literal` as Lean regression tests | Scalar-only "equal" on corpus and real-code confirmed; mutation gate at zero false equals | 3 to 4 months |
| 2 Queries | Bags, joins, aggregates, ordering and NULL ordering, paging, set operations, scalar subquery contracts, catalog keys and nullability | As phase 1, for queries | 3 to 5 months |
| 3 DML | INSERT, UPDATE (including `UPDATE … FROM`), DELETE, MERGE, upserts, generated keys and `RETURNING`; execution oracle as a Lean differential test | DML "equal" confirmed; oracle agrees with the Lean semantics | 2 to 4 months |
| 4 Routines | The `formal/ErrorTxn.tla` semantics ported to Lean over the CFG; routine equivalence by simulation; V4, V9, V14, V15 as axioms | Behavioural "equal" confirmed; axiom allowlist matches vendor-facts-verification.md | 3 to 5 months |
| 5 Hardening | Coverage, checker performance, strict mode as default, parser differential tests against `libpg_query` and ScriptDom | Most real-code "equal" verdicts confirmed | Ongoing |

Concurrency stays out of scope: anything that depends on it remains an
assumption.

## Drift management

The Rust implementation and the Lean gate change at different speeds. What
each kind of drift can do:

| What drifts | Effect | False "equal"? |
|---|---|---|
| Rust lowering changes, Lean not | Rust "equal" is not confirmed: unknown | No; coverage drops, measurably |
| Lean stricter than Rust | Unknown, logged as a possible Rust bug | No |
| Lean "equal", Rust has a "different" witness | Contradiction; the witness is run on the engine to decide which side is wrong | No, but one side has a bug |
| Statements and proofs | The kernel rechecks proofs; `theorems.lock` catches statement edits | No |
| **The meaning of an AST node** (the parser changes what a field carries) | Lean proves facts about a meaning the parser no longer produces | **Yes: the one silent path** |
| Spec and a newer engine release | The proof holds for an outdated semantics | Yes, across versions (see "RDBMS versions") |

### The AST boundary (soundness)

1. The Rust AST types are the single source. `cargo xtask gen-lean-ast`
   generates the Lean inductive types and the JSON decoder; CI fails when the
   generated file is stale.
2. Lean pattern matches are exhaustive, so a new Rust variant is a Lean build
   error until it is handled; handling it as `unsupported` gives unknown.
3. Both binaries embed the schema hash. A mismatch, an unknown field or an
   unknown variant makes the checker refuse, and the verdict is unknown.
4. End-to-end differential tests: SQL text through the Rust parser into the
   Lean semantics, against the same text on the engine. One test covers the
   parser, the AST meaning and the spec. Db2 for i goes through probes.
5. Parser differential tests against `libpg_query` and ScriptDom for the
   verified constructs.

### Coverage (usefulness)

1. Every gated verdict is classified: confirmed; Lean unsupported; Lean
   not-equal; Lean equal against a Rust "different". Counts are logged per
   run and reported by CI on the corpus, real-code and mutation sets.
2. Ratchet: the confirmed rate on the corpus never falls below `main`;
   contradictions are empty or triaged before merge.
3. Stale twins: each Lean lowering function names the Rust function it
   mirrors (`-- mirrors canon.rs: Canon::binary`). `formal/lean/SYNC.toml`
   keeps the Rust body hash at the last sync; CI lists the functions changed
   since, and the `sync` agent works through the list.
4. Term diff: for the verified fragment, Lean and Rust produce the same
   `Term`; CI diffs them on every corpus file, which names the construct that
   drifted.

### Process

- A change to a construct inside the verified fragment updates Lean in the
  same PR. Outside it, Lean may lag; the lag shows as unconfirmed verdicts and
  is tracked as coverage debt.
- `sync` drafts the Lean change, `prover` repairs proofs, a human reviews spec
  and statement changes only.
- Pinned: `lean-toolchain`, Lean dependencies, engine container images (by
  version and cumulative update, not `2022-latest`). Both binaries ship
  together with one version.

### Convergence

Two implementations need the controls above. Once a fragment is verified and
stable, Rust calls the compiled Lean lowering (Lean compiles to C and links as
a static library) and retires its own code for that fragment, so drift inside
the verified part is impossible. The Lean lowering mirrors `canon.rs`
structurally from the start, so retiring Rust code is a swap. Cost: a Lean
runtime in the Rust binaries and some Lean knowledge among contributors.

## RDBMS versions

### Today

- The version is a free-form string per artifact, defaulting to SQL Server
  2022, PostgreSQL 18 and IBM i 7.6 (`Dialect::default_version`,
  `crates/sqldiff-syntax/src/dialect.rs`). It is set by `--dialect-version`,
  `[dialect_versions]` in `sqldiff.toml` or a pack, or a `dialect_map` entry.
- The only behaviour that depends on it is built-in availability
  (`[*.since]` in `profiles/builtins.toml`, compared by `version_lt` in
  `catalog/builtins.rs`). Semantic profiles, rules, the converter and the
  witness search do not use it.
- Version strings are not validated; an unparsable part compares as 0.
- The vendor facts, eval runs and CI containers cover SQL Server 2022 and
  PostgreSQL 18 only; Db2 for i facts were read for 7.4 to 7.6 and probed on
  whatever release a user has.

### Policy: follow the vendor lifecycle

A version's status in SQLDiff follows its vendor's lifecycle, read from data,
not from code:

| SQLDiff status | Vendor state | What SQLDiff does |
|---|---|---|
| `preview` | Beta, release candidate, announced | Parses and analyses; the gate never confirms "equal" (the spec is not validated against a release) |
| `supported` | Under vendor support: PostgreSQL community support (5 years from the first release); SQL Server mainstream or extended support; IBM i standard support | Full: spec, proofs, engine tests in CI, probes for Db2 for i |
| `legacy` | Only a paid extension (SQL Server Extended Security Updates, IBM i Service Extension), or within 12 months after vendor support ended | Results as before; spec and proofs frozen; no new behaviour work; every report states the status |
| `retired` | After `legacy` | Rejected with a message naming the end date, except as side A of a migration review (see below); removed from CI and the spec at the next release |

Per vendor:

- **PostgreSQL**: one version key per major. Minor releases are bug fixes;
  CI tracks the latest minor of each supported major. A new major enters as
  `preview` at its first beta.
- **SQL Server**: the key is the engine version plus the database
  compatibility level, because many T-SQL behaviours follow the compatibility
  level, not the engine. Supported pairs are the compatibility levels each
  supported engine offers. Cumulative updates are pinned in CI.
- **Db2 for i**: the key is the IBM i release plus the Db2 PTF group and
  Technology Refresh level, because SQL features arrive by Technology
  Refresh. `[db2i.since]` entries name a release and level. Probes run on
  every supported release; a fact not probed on a release is an axiom for
  that release only.

Migration review: comparing a routine on a retired or legacy version against
its upgraded form is the most common reason to need an old version, so the
semantics of the last retired version per dialect stay available as side A
(old side) only, frozen, until the next version retires.

### Lifecycle data

`profiles/lifecycle.toml`, one entry per version:

```toml
[[postgres]]
version = "14"
released = 2021-09-30
support_ends = 2026-11-12
source = "https://www.postgresql.org/support/versioning/"
checked = 2026-10-28
status = "unverified"        # verified once read from the vendor page by a person
```

- Dates are read from the vendor's own lifecycle page (postgresql.org
  versioning policy, Microsoft lifecycle pages, IBM i lifecycle pages), like
  vendor facts: AI may draft, a person verifies, `status` records which.
- The CLI warns when a report uses a version that is `legacy`, `retired`, or
  less than 90 days from a status change.
- A monthly CI job opens an issue 90 days before a version changes status. A
  status change itself needs a person's PR (CI matrix, spec, docs).

Snapshot on 2026-10-28, from third-party trackers and Microsoft's
year-level table, **not yet verified against vendor pages**
(postgresql.org was not reachable from the session that wrote this):

| Dialect | Version | Vendor support ends | Status it implies |
|---|---|---|---|
| PostgreSQL | 14 | 2026-11-12 | supported, `legacy` from then |
| PostgreSQL | 15, 16, 17, 18 | November 2027, 2028, 2029, 2030 | supported |
| PostgreSQL | 19 | not released (beta 4 on 2026-09-24) | preview |
| SQL Server | 2016 | extended support ended 2026-07-14; ESU end date unconfirmed | legacy |
| SQL Server | 2017, 2019, 2022, 2025 | extended 2027-10-12, 2030-01-08, 2033-01-11, 2036-01-06 | supported |
| IBM i | 7.4 | standard support ended 2026-09-30; Service Extension to 2029-09-30 (one source) | legacy |
| IBM i | 7.5, 7.6 | not announced | supported |

### Versions in the proofs

- The Lean spec takes the version as a parameter: one base semantics with
  version predicates (`mergeAvailable v := v ≥ 15`), not a copy per version.
  Theorems quantify over the supported range, so the proof effort grows with
  the number of behaviour differences, not with the number of versions.
- A new release goes through a delta review: release notes and the vendor's
  behaviour-change list, AI-drafted candidate deltas with citations, a
  person's review, then the whole engine test suite on the new release to
  catch undocumented changes. Most proofs are unaffected and rerun as-is.
- CI: every PR runs the oldest and newest supported version of each dialect;
  nightly runs every supported and preview version.
- Same dialect, two versions (a routine on PostgreSQL 14 against the same
  routine on 18) becomes an ordinary comparison, useful for upgrade reviews
  before an end of support.

Expected ongoing cost per release: two to four weeks for a PostgreSQL major
(yearly), more for a SQL Server release (every few years) or compatibility
level, and a probe run plus a delta review for each IBM i Technology Refresh.

## Leads found while planning

Not yet confirmed as bugs; each needs a test first.

- Raw terms for statements that fail to parse fold every word to upper case
  (`equiv/mod.rs`, around lines 94 to 113) and can compare equal without
  `ASM-TSQL-CI-COLLATION`, which matters under a case-sensitive collation.
- Collection roll-ups default to "equal" when empty
  (`.unwrap_or(Verdict::Equal)` in `sqldiff-cli` `main.rs`, `source_cmd.rs`
  and `queue_cmd.rs`, and in `sqldiff-review/src/branch.rs`).
- `GENERATE_UUID` needs IBM i 7.6 or a Technology Refresh on 7.4 and 7.5
  (converter note in `sqldiff-convert/src/tsql.rs`), but has no
  `[db2i.since]` entry, so the binder never gates it.
- PostgreSQL `MERGE` (15+) and its 17+ clauses are not version-gated in code.
- `version_lt` treats an unparsable version part as 0.
- CI uses `mcr.microsoft.com/mssql/server:2022-latest`, a floating tag.

## Metrics

- Share of "equal" verdicts confirmed by the gate, per dialect version, on
  corpus, real-code and mutation sets.
- `sorry` count (zero) and axiom count (each mapped to a `V*` fact and a
  version).
- Spec/engine disagreements and Rust/Lean contradictions found per week.
- Versions per status, and days until the next status change.
- Review hours per phase.
