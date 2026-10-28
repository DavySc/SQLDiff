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

> If SQLDiff reports equal under S, then for every pair of related
> environments satisfying Γ, if S holds, every observation of `b` on `vB` is
> related to some observation `a` on `vA` can produce.

"Related", "environment" and "observation" are fixed by decision 1 below.

Design: translation validation.

1. Rust parses both sides and exports the AST, the catalog facts and the
   dialect versions as JSON, with its statement and path alignment as an
   untrusted hint.
2. `sqldiff-check`, a Lean 4 program compiled to a native binary, lowers the
   AST with a lowering proven to preserve the dialect semantics and checks
   that B's terms refine A's (`tB ⊑ tA`), along the hinted alignment.
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

### Decision 2: a gate, then convergence

Decided by the project owner on 2026-10-28.

| Choice | Decided | Rejected, and why |
|---|---|---|
| What is verified | A Lean checker gates every "equal"; once a fragment is verified and stable, Rust calls the compiled Lean lowering for it and deletes its own code (see "Convergence") | Proving the Rust (Aeneas, Verus, Creusot): `canon.rs` would first need rewriting into the verifiable subset, every refactor breaks proofs, and it needs the same Lean SQL semantics anyway |
| Checker input | The parsed AST; Lean lowers it itself, so the Rust lowering is not trusted | Rust's terms plus a lowering certificate: a certificate format for every rule in `canon.rs`, and two lowerings forever |
| Alignment | Rust sends its statement and path alignment as an untrusted hint; Lean checks refinement along it, which keeps the check about linear in routine size. A wrong hint makes the verdict unconfirmed, never wrong | Lean searching for the alignment itself: the same budget problems the Rust side has |
| Integration | A separate `sqldiff-check` process first, one per CLI invocation for all its pairs and one per GUI or MCP session, over the chunked JSON stdio framing of [adapter-protocol.md](adapter-protocol.md); a linked library at convergence | Linked from the start: a Lean stack overflow would take down the GUI or MCP server, and the musl and macOS x64 builds must work before anything ships |

Rules that follow, part of the trusted base:

- **Failure means unconfirmed.** A missing checker, a crash, a timeout or a
  schema mismatch gives "unconfirmed", "unknown" in strict mode; a Rust
  "equal" never stands alone.
- **No default reads as confirmed.** Lean's `panic!` prints and returns the
  default value of the result type by default, in both integration modes.
  The result type is designed so that its default, and every error path,
  means "not confirmed".
- **Only candidate equals are checked.** The checker runs only when Rust is
  about to report "equal" or "equal under assumptions"; "different" and
  "unknown" do not call it.
- **The executable code imports only `Init` and `Std`.** Mathlib and the
  `Lean` package stay in proof modules, so the binary carries neither; the
  AST codegen emits its own decoder instead of using `Lean.Json`.

Runtime model: Lean compiles to C and links a small runtime (`libleanrt`)
plus the compiled modules it imports; proofs and types are erased. Memory is
reference counted (no collection pauses; values with one owner are updated in
place); every value is boxed and objects shared between threads use atomic
counts, so per operation it is expected to be a small factor slower than
Rust. In linked mode each Rust thread that calls Lean registers with its
runtime, and depth limits or a large-stack thread guard against stack
overflow.

Baseline, release build, 2026-10-28 (this machine): a `sqldiff diff` of a
corpus routine takes 4 to 6 ms including process start, a synthetic
400-statement T-SQL to PostgreSQL routine 584 ms, and a process start 1.6 ms.
Phase 0 measures the checker against these budgets:

| Measure | Budget |
|---|---|
| Checker start, cold | < 20 ms |
| Check of a corpus routine (about 20 lines) | < 5 ms |
| Check of the 400-statement routine, edited to be equal | < 25% of the Rust diff time |
| `diff` time on the corpus and the WWI benchmark, gate on against off | < 10% slower |
| Binary size added (separate process) | < 20 MB |
| Memory | Below the Rust process for the same pair |
| Builds | All four release targets: Windows x64, macOS arm64 and x64 (built on arm64), static musl Linux x64 |

Spike results, 2026-10-28 (`.github/workflows/formal-lean.yml`, Lean
4.35.0, the phase 0 checker; release targets as in `release.yml`):

| Target | Start-up, median | Per request | Size | Links against |
|---|---|---|---|---|
| Linux x64 | 3.2 ms | 2.1 µs | 8.2 MB (unstripped) | glibc 2.14 or newer, nothing else |
| macOS arm64 | 6.5 ms | 2.1 µs | 2.3 MB | system libraries only |
| macOS x64 (built and run under Rosetta on arm64) | 45 ms | 12.8 µs | 2.8 MB | system libraries only |
| Windows x64 | 18.9 ms | 2.9 µs | 7.9 MB | Windows DLLs only |

All within budget; the macOS x64 figures include Rosetta's translation and
are not measured on Intel hardware. A static glibc link crashed on start,
so Linux ships a glibc-linked checker beside the musl CLI. Lean 4.19's own
linker produced macOS binaries that dyld refuses ("__DATA_CONST segment
missing SG_READ_ONLY flag"); Apple's linker did not help; Lean 4.35.0 does
not have the problem and is pinned.

When the spike misses a budget: run the gate asynchronously in the GUI
("equal, confirming…"), cache checks by the hash of both ASTs, the versions,
the observer profile and the checker version, or keep strict mode for CI
while interactive runs show unconfirmed results first. If the musl build
fails, a glibc checker ships beside the musl CLI.

## Open decisions

| # | Decision | Recommendation | Status |
|---|---|---|---|
| 1 | What "related observations" means (below) | Refinement of the reference side, over a stated observer profile | Decided 2026-10-28 (four choices by the project owner, the rest proposed) |
| 2 | Gate (translation validation) or proving the Rust | Gate, then converge per fragment, with a performance gate (below) | Decided 2026-10-28 |
| 3 | Rollout | A `proof` field and modes off/report/require; `require` becomes the default per dialect pair (below) | Decided 2026-10-28 |
| 4 | Access to IBM i systems | Manual probes now, rented IBM i decided at phase 3, no `require` without an engine differential (below) | Decided 2026-10-28; first access pending |
| 5 | Who reviews spec, statements and axioms | The project owner, with external dialect experts at phase ends; approval by signed lock files (below) | Decided 2026-10-28 |
| 6 | Version support policy | Follow each vendor's lifecycle (see "RDBMS versions") | Proposed |
| 7 | Azure SQL Database and Managed Instance | All Azure targets in scope as platform profiles; T-SQL targets keyed by platform, engine and compatibility level (below) | Decided 2026-10-28 |
| 8 | When Db2 for i is proven | Deferred as long as possible: phases 1 to 4 cover T-SQL and PostgreSQL only (below) | Decided 2026-10-28 |

### Decision 1: observational equivalence

"Equal" is a statement about what a caller can observe. Decided by the
project owner on 2026-10-28: refinement for nondeterminism, error kinds plus
user codes, an observer profile for side channels, and session state
observed. The other items are proposals that stand unless changed.

#### Refinement, and the side it refines

`A` is the reference and `B` the candidate: the original and its conversion
(UC3), the old version and the new one (UC2). "Equal" means **B refines A**:
for every environment, every observation B can produce is one A could
produce. B may be more deterministic than A (a conversion may add an
`ORDER BY`), never less.

- The order of `sqldiff diff a b` matters: swapping the sides can change the
  verdict. Reports state which side is the reference; the GUI's swap action
  re-runs the comparison.
- **Equivalent** is refinement both ways. Reports carry it as a separate
  field. Drift between two variants where neither is the original (UC1, and
  UC4 collections without a declared reference tree) uses equivalence.
- A refinement proof is one direction; equivalence needs a second proof.

#### Environment against unspecified behaviour

Refinement is only sound when the nondeterminism it ranges over is the
engine's own unspecified behaviour. Everything else is an input that the
environment chooses and both sides share:

| Kind | Examples | Treatment |
|---|---|---|
| Environment (inputs, for all) | Database state, arguments, session settings, caller transaction, privileges, clock readings, `RAND`/`NEWID`/`uuidv4()` draws, environmental faults (lock timeout, cancel, a failing write) | Chosen once and related between the sides by the correspondence. Faults are related through a correspondence of writes (same kind, related target), as `ir/behaviour.rs` aligns them today |
| Unspecified behaviour (refinable) | Order of an unordered result; the rows `TOP`/`LIMIT`/`FETCH` return without a total `ORDER BY`; the source row `UPDATE … FROM` or `SELECT @v = col` uses; evaluation order the vendor does not guarantee, including whether an error is raised; which of several violations is reported | A's set of allowed outcomes. B may pick any subset |

Why the line matters: if clock or random draws were refinable, a B that
always returns one fixed UUID, or freezes the clock, would refine A; if
faults were refinable, a B that always fails with a lock timeout would
refine any A that could time out.

Two obligations follow:

- `Run` is total and never empty (non-termination and errors are
  observations), proven once in Lean; otherwise an empty B would refine
  anything.
- Marking behaviour as unspecified widens A's set, so a wrong "unspecified"
  can produce a false "equal". Every such spec rule cites the vendor text
  that says the behaviour is not guaranteed and is reviewed like an axiom.
  Engine tests cannot confirm it (an engine shows one choice); they can only
  refute it.

#### Observations

| Channel | Observed as |
|---|---|
| Result sets | Values through the value correspondence; a list when A orders it totally, else a bag; column types through the correspondence (`ASM-RESULT-TYPES` when unknown) |
| Result column names | Same dialect: exact. Cross-dialect: case-insensitive, under `ASM-RESULT-NAMES-CI` (PostgreSQL returns an unquoted `AS OrderId` as `orderid`) |
| Return value, OUT parameters, return code | Through the value correspondence |
| Durable state (read in a fresh session) and pending state (the caller's own view) | Tables as bags, through the state correspondence |
| Caller transaction afterwards | Open, committed, rolled back, doomed; trancount |
| Errors | See below |
| Session state left behind | Temporary tables that still exist and their contents, settings changed by `SET` that persist, open cursors. Observed because callers (connection pools) keep using the session |
| Generated keys | Values written or returned, from related generator states. Generator positions afterwards (gaps after a rollback) are not observed, under `ASM-GENERATOR-GAPS` |
| Rows-affected counts, messages | Set by the observer profile (below) |
| Non-termination | An observation of its own |
| Not observed | Performance, plans, locks held, statistics, log volume |

#### Errors

- **Cross-dialect:** the error kind from the semantic profiles
  (`[error_kinds]`: constraint, conversion, arithmetic, cardinality,
  not_found, lock, user_signal, name_resolution, cancel, other) must match.
- **Same dialect:** the exact error number or SQLSTATE.
- **Errors the code raises itself** (`THROW`, `RAISERROR`, `RAISE`,
  `SIGNAL`): the user code, matched through a mapping a pack declares
  (`THROW 50001` against `RAISE … USING ERRCODE = 'U0001'`), and the
  message text as rendered. Text that formats a value (number, date, NULL)
  is not proven unless the value is a literal, as for `PRINT` in scripts.
- Engine message text is never compared.

#### Observer profile

Side channels that only some callers read are switches in an observer
profile, set in `sqldiff.toml` or a pack (`[observer]`). Every report names
the profile; when an unobserved channel could differ, the report lists its
assumption (`ASM-OBSERVER-<CHANNEL>`).

| Channel | Default | Why |
|---|---|---|
| Rows-affected counts (`SET NOCOUNT`, read by `ExecuteNonQuery`) | Observed in the same dialect, not across dialects | PostgreSQL procedures return no counts, so cross-dialect pairs could never be equal |
| Messages (`PRINT`, `RAISE NOTICE`, warnings) | Observed | Scripts compare them today |

#### Environments

All database states satisfying the catalog; all arguments, across dialects
those valid for both declared types (`ASM-PARAM-DOMAIN` when the domains
differ); session settings fixed by the source or an assumption; caller
privileges (a permission error is an error). Excluded by assumption:
concurrency (serial, one session), resource limits and timeouts (class
`Limit`).

#### Correspondence

Three relations, data with assumptions, all the identity within one dialect
version: values (`bit` 1 to `true`, `datetime2(p)` to `timestamp(p)`;
`eval/casts` and `eval/bit-boolean` are the ground truth), names
(`ASM-IDENT-FOLD`, `ASM-DEFAULT-SCHEMA`) and states (tables related row by
row).

#### In Lean

```lean
structure Env where            -- chosen by the world; related between sides
  db : DbState
  args : List Value
  settings : Settings
  callerTxn : Option TxnState
  privileges : Privileges
  clock : Clock
  draws : Draws
  faults : FaultPlan

-- every observation one run can produce; nonempty (theorem run_nonempty)
def Run (v : DialectVersion) (p : Prog) (e : Env) : Set Obs

def Refines (prof : Profile) (R : Corr) (vA : DialectVersion) (a : Prog)
    (vB : DialectVersion) (b : Prog) (S : Assumptions) : Prop :=
  ∀ eA eB, R.env eA eB → S.holds eA eB →
    ∀ ob ∈ Run vB b eB, ∃ oa ∈ Run vA a eA, R.obs prof oa ob

def Equivalent prof R vA a vB b S :=
  Refines prof R vA a vB b S ∧ Refines prof R.symm vB b vA a S
```

#### What changes in today's verdicts

Checked with `sqldiff diff` on 2026-10-28, before the change:

| Case | Today | After decision 1 |
|---|---|---|
| B adds `ORDER BY` to an unordered result | unknown | equal (B refines A); not equivalent |
| B drops the `ORDER BY` A had | unknown | different: B can return an order A never does |
| B renames an output column alias | unknown | different in the same dialect |
| B adds `SET NOCOUNT ON` (same dialect) | different | different (counts observed by default) |
| A routine whose only cross-dialect difference is rows-affected counts | not separately reported | equal under `ASM-OBSERVER-ROWCOUNT` |
| Silent row picks on both sides with no `ORDER BY` | equal under `ASM-SINGLE-ROW` | possibly equal without it, when A's pick is unspecified; NULL against unchanged on zero rows still differs |

Work this creates outside the proofs: a reference side in the CLI, MCP,
GUI, review store and `pairs`/`tree`/`queue` configuration; an
`equivalent` field; the `[observer]` section; the error-code mapping in
packs; witness texts that say which side can produce what. It ships in
Rust before the checker (decision 3).

### Decision 8: Db2 for i deferred

Decided by the project owner on 2026-10-28: Db2 for i proofs come as late as
possible.

- Phases 1 to 4 cover T-SQL and PostgreSQL only. The Lean AST and spec stay
  parameterised by dialect, so Db2 for i is added later without rework.
- Db2 for i "equal" verdicts stay `unconfirmed`, reason "dialect not yet in
  the verified scope", and never reach `require`.
- The manual fact probes (W1, decision 4) continue: the Rust tool's
  assumptions need them regardless.
- Db2 for i gets its own phase 6, after phase 5. It starts when T-SQL and
  PostgreSQL are at stage 2 of decision 3 for every supported version, or
  earlier if the owner decides. Rented IBM i for W2 and W3 is decided when
  phase 6 starts.

### Decision 7: Azure SQL

Decided by the project owner on 2026-10-28. Until now SQLDiff ignored the
database compatibility level, also for boxed SQL Server, although many T-SQL
behaviours follow it and an upgraded database keeps its old level.

The three axes of a T-SQL target, as of 2026-10-28:

| | Boxed SQL Server | Azure SQL Managed Instance | Azure SQL Database, Fabric SQL database |
|---|---|---|---|
| Engine | Version (2016 to 2025) | Update policy: SQL Server 2022 (default), SQL Server 2025, or Always-up-to-date (evergreen, one-way) | Evergreen |
| Compatibility level | Per database; new databases get the native level (2022: 160) | Per database | Per database; 170 by default for new databases, existing ones are not upgraded |
| Platform defaults | The server's configuration | Close to boxed | `READ_COMMITTED_SNAPSHOT` on (off on boxed); UTC clock; contained database (no cross-database names, no `USE`) |
| Lifecycle | The SQL Server release's | A 2022 or 2025 policy upgrades itself at the end of that release's mainstream support | None |

Decided:

- **A T-SQL target is (platform, engine version or update policy,
  compatibility level).** The level defaults to the engine's native level
  (Azure SQL Database: 170) and is set per database in `dialect_map` or
  `sqldiff.toml`. The spec takes the level as a parameter, like versions.
  This is part of phase 0's typed version key.
- **All four platforms are in scope for the Rust tool early**, as platform
  profiles: their defaults, unavailable features, and the differences
  between them reported as findings or assumptions.
- **Platform defaults are declared settings**, overridable in configuration
  like `XACT_ABORT` today. Comparing across platforms reports a binding
  finding where a construct is unavailable (a three-part name on Azure SQL
  Database) and lists a differing setting as an assumption. Read-committed
  snapshot only matters under concurrency, which decision 1 assumes away;
  the clock's time zone is an environment input both sides share.
- **Fixed update policies are their SQL Server release.** Managed Instance on
  the 2022 or 2025 policy uses that release's spec plus the platform
  settings, and can be proven and reach `require` like boxed SQL Server. Its
  lifecycle follows the policy, including the automatic upgrade.
- **Evergreen targets alias the newest SQL Server release.** Azure SQL
  Database, Fabric SQL database and Always-up-to-date instances are treated
  as the newest `supported` SQL Server release in `profiles/lifecycle.toml`
  at the database's compatibility level, under the assumption
  `ASM-AZURE-AS-LATEST`. No Azure subscription is needed. With no engine
  differential on the real platform (decision 4), they never reach
  `require`: their confirmations always carry that assumption.

### Decision 5: review of trusted content

Decided by the project owner on 2026-10-28. On that date the project has one
human contributor, commits (Claude's included) go straight to `main`, and
there is no CODEOWNERS file, so review has to be cheap for one person and
enforced by something an agent cannot produce.

Trusted content, which only a human approves:

| Item | What the review checks |
|---|---|
| Theorem statements (`Statements/`) | They say what decision 1 means, no weaker |
| Spec rules (`Spec/`) | Each matches its cited vendor text and its engine test results |
| Rules that mark behaviour unspecified | The vendor text really says the behaviour is not guaranteed (they widen what "equal" allows) |
| Axioms | Each matches its `V*` fact in vendor-facts-verification.md and the probe status |
| The AST meaning contract | What the parser produces is what the Lean side assumes |

Decided:

- **Who.** The project owner approves every item. At the end of each phase
  an external expert per dialect (T-SQL, PostgreSQL, IBM i) reviews the
  riskiest items: every unspecified-behaviour rule and axiom, and a sample of
  spec rules chosen by risk. Expert time is budgeted per phase; their
  findings are recorded like probe runs.
- **Enforcement: signed locks.** `theorems.lock` and `SPEC.lock` hold one
  hash per approved item (statement, rule, axiom, AST contract version) and
  are signed with the owner's SSH or GPG key. CI checks the signature
  against `formal/allowed_signers` and fails when trusted content has no
  signed hash. No agent holds the key, so no agent can approve, however a
  change reaches the repository; direct pushes keep working for everything
  else. Changing `allowed_signers` itself needs a signature by a key already
  in it.
- **Adversarial review of every trusted change.** A `spec-critic` agent,
  started fresh, sees the changed item and its vendor citation but not the
  drafter's reasoning, and tries to break it (a counterexample, a misread
  citation, a weaker statement than decision 1). Its report goes with the
  review; its findings are leads, never approvals.
- **What is approved is the Lean text.** Each item carries machine-checked
  examples (Lean `example`s it must accept or reject, from engine runs where
  they exist) and an AI paraphrase as a reading aid. The owner learns to
  read, not write, Lean during phase 0. A generated table lists every item
  with its citation, engine evidence, critic report and approval.

### Decision 4: IBM i access

Decided by the project owner on 2026-10-28. As of that date no probe in
`probes/db2i` has run on an IBM i: all 16 facts in `status.json` are
`unverified`.

What the plan needs from IBM i:

| Workload | Scale | Access |
|---|---|---|
| W1 vendor-fact probes (`probes/db2i`) | One script, 139 probes, per supported release and Technology Refresh | Manual, as the probe README describes |
| W2 spec differential (`spec-breaker`, phases 1 to 4) | Thousands of generated programs | Programmatic: JTOpen JDBC, ODBC or Mapepire |
| W3 execution oracle and corpus Db2 fixtures | Tens of scenarios per run | Programmatic |

Every system used must let a profile create a schema (journaled), run under
commitment control and create SQL procedures, and must not be a production
system or hold customer data: the probes write, and SQLDiff never writes to
a source system.

Decided:

- **Now (phases 0 and 1): W1 by hand.** First access is PUB400 (free
  community system, IBM i 7.5 per April 2026 reports, non-commercial use, one
  profile per person), run by the project owner in Access Client Solutions.
  It cannot run the V25 authority check, which needs a second profile, and
  may refuse `CREATE SCHEMA SQLDFPROBE`, in which case the probe build needs
  a configurable schema name. Running from a cloud session needs the host
  allowed in its network policy and a non-HTTPS connection (SSH or the
  database host servers), which may not be possible there.
- **Rented IBM i for W2 and W3: decided when phase 6 starts** (decision 8;
  originally phase 3), priced then (for
  example IBM Power Virtual Server, started and stopped by CI under a budget
  cap, credentials in CI secrets).
- **No engine differential, no `require`.** A dialect version reaches stage
  2 of decision 3 only after its spec has passed a differential test against
  that engine version: PostgreSQL and SQL Server in CI containers, Db2 for i
  through W2 on that release. Until then its confirmations rest on axioms,
  listed as assumptions, and its default stays `report`.
- **All supported releases are probed:** at least one W1 run per supported
  release (7.5 and 7.6) at its latest Technology Refresh, repeated for each
  new Technology Refresh. The `legacy` 7.4 gets one run before its spec is
  frozen if access exists; otherwise its facts stay axioms for 7.4.

### Decision 3: rollout

Decided by the project owner on 2026-10-28.

#### How a report shows the proof

The verdict values do not change. Reports gain:

- `proof`: `confirmed`, `unconfirmed` or `n/a` (nothing to confirm: the
  verdict is not "equal" or "equal under assumptions");
- `proof_reason` when unconfirmed: fragment not covered, version in
  `preview`, checker missing, timed out or crashed, schema mismatch, or a
  contradiction with a Rust "different";
- `verdict_before_proof` in `require` mode, recorded the way
  `verdict_before_suppression` is (`Report::settle_suppression`), so text,
  JSON, SARIF, the GitHub Action and exit codes agree without new flags.

Modes, set by `[proof] mode` in `sqldiff.toml` or a pack, or `--proof`:

| Mode | Effect |
|---|---|
| `off` | The checker is not called; no `proof` field |
| `report` | The checker runs on candidate equals; the verdict is unchanged |
| `require` | An unconfirmed "equal" or "equal under assumptions" becomes "unknown" |

A confirmed verdict lists the axioms the confirming theorem used (from
`#print axioms`), mapped to their `ASM-FACT-*` assumptions, so a proven
"equal" names exactly what it rests on. Until `require` is the default, no
document or screen says "proven" without "confirmed by `sqldiff-check`
<version> against spec <version>"; the GUI shows a badge on confirmed
verdicts only.

#### Stages

| Stage | When | Default mode |
|---|---|---|
| 0 | Phase 0: the gate covers only the trivial fragment | `off` for users; the project's own CI runs `report` and tracks coverage |
| 1 | The gate ships in releases and confirms at least half of the corpus "equal" verdicts | `report` everywhere; nothing fails because of it |
| 2 | Per dialect pair and version (T-SQL to PostgreSQL 18, …): at least 95% of "equal" verdicts on corpus, real-code and the WWI benchmark confirmed, no contradiction open for 8 weeks, and both versions' specs passed an engine differential (decision 4) | `require` for that pair, including the GitHub Action's default |
| 3 | Every supported pair is at stage 2 | `require` everywhere; `report` stays as an opt-out |

Early users of `require`, from phase 0: the formatter's self-check
(`format::engine_check`, which needs "equal" and whose edits are the trivial
fragment) and the mutation gate's meaning-preserving edits (whitespace,
keyword case, comments).

With the version lifecycle: `preview` versions never confirm, so under
`require` they are always "unknown"; `legacy` versions keep their frozen
proofs and still confirm.

#### Semantics first

The decision 1 changes to Rust's verdicts (refinement with a reference
side, error matching, the observer profile, session state, the
`equivalent` field) ship in Rust before the checker, as one deliberate
behaviour change with release notes, so the checker checks the relation
Rust already reports. Side A, the reference, stays the first argument
everywhere: the base in `changes`, the source in `convert`. The release
notes list the verdicts that change (table above) and the rule ids that
change; reviews accepted under an old rule id no longer match their
finding, and the release says so and how to re-accept.

## AI operating model

AI writes proofs; it never writes trusted statements unreviewed.

| Artifact | Written by | Checked by |
|---|---|---|
| Proof bodies | AI | The Lean kernel |
| Theorem statements (`Statements/*.lean`) | AI drafts, human approves | `theorems.lock`, signed by the owner (decision 5); CI fails on a change not in it |
| Dialect specification (`Spec/*.lean`) | AI drafts with a citation per rule, human approves | Engine differential tests, `SPEC.lock` signed by the owner |
| Axioms | Human only | `#print axioms` checked against an allowlist in `SPEC.lock`; each maps to a `V*` fact |
| Spec rules that mark behaviour unspecified | AI drafts with the vendor text that says it is not guaranteed, human approves like an axiom | Engine tests can only refute them (decision 1); external expert review at phase ends (decision 5) |
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
- `spec-critic`: started fresh on every change to trusted content, sees the
  item and its citation but not the drafter's reasoning, and tries to break
  it (decision 5). Reports leads only.
- `audit` gains `lake build`, the lock signature, axiom and `sorry` checks,
  and the Rust/Lean differential suite.

AI shortens proof work most; drafting the specification less; deciding and
reviewing the semantics hardly at all. Review is the critical path.

## Phases

Estimates are elapsed time for one engineer with AI agents, with wide error
bars; total about 14 to 24 months for T-SQL and PostgreSQL including
multi-version support, and an estimated 4 to 8 months more for Db2 for i in
phase 6 (decision 8). Phases 1 to 5 cover T-SQL and PostgreSQL only.

| Phase | Scope | Exit criterion | Estimate |
|---|---|---|---|
| 0 Groundwork | Private `Report.verdict` (assigned today in core, source, review and convert); the decision 1 semantics in Rust and the `proof` field and modes (decision 3); a coverage invariant (every unit proven or covered by a finding); tests for the leads below; typed dialect version (see "RDBMS versions"); `formal/lean` Lake project with `Term`; AST JSON export with schema hash; the gate on a trivial fragment; the packaging and performance spike (decision 2); signed locks, `allowed_signers` and the `spec-critic` agent (decision 5); the owner learning to read Lean; CI and agents | Gate confirms the trivial fragment on the corpus, on all four release targets, within the decision 2 budgets | 2 to 3 months |
| 1 Scalar expressions | Three-valued logic, NULL, numeric types, strings and collation, casts and literals, bit and boolean, CASE/COALESCE/ISNULL; `compare.rs` hooks proven under their assumptions; `eval/casts`, `eval/bit-boolean`, `eval/literal` as Lean regression tests | Scalar-only "equal" on corpus and real-code confirmed; mutation gate at zero false equals | 3 to 4 months |
| 2 Queries | Bags, joins, aggregates, ordering and NULL ordering, paging, set operations, scalar subquery contracts, catalog keys and nullability | As phase 1, for queries | 3 to 5 months |
| 3 DML | INSERT, UPDATE (including `UPDATE … FROM`), DELETE, MERGE, upserts, generated keys and `RETURNING`; execution oracle as a Lean differential test | DML "equal" confirmed; oracle agrees with the Lean semantics | 2 to 4 months |
| 4 Routines | The `formal/ErrorTxn.tla` semantics ported to Lean over the CFG, T-SQL and PostgreSQL rows; routine equivalence by simulation; their unverified facts as axioms | Behavioural "equal" confirmed; axiom allowlist matches vendor-facts-verification.md | 3 to 5 months |
| 5 Hardening | Coverage, checker performance, the decision 3 stages up to `require` everywhere, parser differential tests against `libpg_query` and ScriptDom | Most real-code "equal" verdicts confirmed | Ongoing |
| 6 Db2 for i | Phases 1 to 4 for SQL PL and the Db2 for i rows of the TLA+ model; V4, V9, V14, V15 as axioms unless probes settled them; W2 and W3 on rented IBM i (decisions 4 and 8) | Db2 for i at stage 2 for every supported release | 4 to 8 months (estimate) |

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

Convergence puts Lean on the path of every diff, not only of candidate
equals: the witness search and explanations use the lowered terms of every
statement pair. A fragment therefore converges only when its Lean lowering
stays within an agreed factor of the Rust lowering on the corpus and WWI
benchmarks (the factor is set when the first fragment is ready, from the
phase 0 measurements). A fragment that misses it stays with two
implementations and the drift controls.

## RDBMS versions

### Today

- The version is a string per artifact, defaulting to SQL Server 2022,
  PostgreSQL 18 and IBM i 7.6 (`Dialect::default_version`,
  `crates/sqldiff-syntax/src/dialect.rs`). It is set by `--dialect-version`,
  `[dialect_versions]` in `sqldiff.toml` or a pack, or a `dialect_map` entry.
  On the `verification` branch it is validated against
  `crates/sqldiff-core/profiles/lifecycle.toml` (`sqldiff_core::target`): a
  T-SQL target is `2019`, `2019/130`, `mi-2022`, `mi-evergreen`, `azure` or
  `fabric` with an optional compatibility level, an unknown version is an
  error, and the CLI prints a note for a `preview`, `legacy` or `retired`
  version or one within 90 days of a change.
- The only behaviour that depends on it is built-in availability
  (`[*.since]` in `profiles/builtins.toml`, compared by `version_lt` in
  `catalog/builtins.rs`). Semantic profiles, rules, the converter and the
  witness search do not use it.
- Version strings were not validated, and an unparsable part compared as 0;
  on the `verification` branch they are validated at every entry point.
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
- **SQL Server**: the key is (platform, engine version or update policy,
  compatibility level), because many T-SQL behaviours follow the
  compatibility level, not the engine (decision 7). Supported pairs are the
  compatibility levels each supported engine offers. Cumulative updates are
  pinned in CI. Managed Instance on a fixed update policy follows its SQL
  Server release; evergreen platforms alias the newest supported release.
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

## Phase 0 progress (branch `verification`)

| Item | State |
|---|---|
| Private `Report.verdict`, changed only by `Report::settle` | Done |
| Typed dialect targets, `profiles/lifecycle.toml`, lifecycle notes | Done (dates unverified) |
| Packaging and performance spike | Done: all four targets within budget (decision 2) |
| Gate on the trivial fragment: `formal/lean` (`check_sound`, no axioms), `sqldiff_core::proof`, `--proof off|report|require` | Done; the CLI tests use a stand-in for the checker, CI tests the real one |
| Decision 1 semantics in Rust | Enforced where a verdict could be wrong: result column names, reference side with `equivalent` and `refinements`, ORDER BY added, observer profile for row counts. Checked 2026-10-28 and already conservative (never equal): raises across dialects (not covered by statement equivalence), temporary tables and `SET` in a body. Coverage still to add, turning unknown into proven: error kinds and pack-declared user error codes across dialects, PostgreSQL raise message changes (unknown, T-SQL already different), session state, ORDER BY removed (different when the result can hold two rows) |
| Coverage invariant | Enforced by construction within one dialect (`crates/sqldiff-core/src/coverage.rs`). Text outside statements is compared token by token (`GO` against `GO 5` is unknown). Inside statements every comparison step leaves a receipt for the text it decided: the structural diff (same tokens, same shell, or a named structural judgment), statement equivalence and the observer profile. A final check re-verifies the "same tokens" and "same shell" receipts and requires every significant token on both sides to be under a receipt or a finding; a token under none makes the verdict unknown ("not compared"). A step that forgets a unit therefore yields unknown, not a false equal. Across dialects the lowering lists what statement equivalence does not cover (`equiv::lower`); moving that onto receipts is still to do |
| AST export with generated Lean types | Not started (phase 0 compares syntax trees as text) |
| Signed locks, `allowed_signers`, `spec-critic` | Not started |
| Shipping `sqldiff-check` in releases | Not started |

The trivial fragment: same dialect, same canonical target, and syntax trees
equal once whitespace and comments are left out. It covers reformatting
and comment changes. Keyword case is outside it, because a T-SQL word such
as `date` can be a keyword or an identifier and identifiers are
case-sensitive under a case-sensitive collation; bringing it in needs a
vendor fact as an axiom (decision 5).

### Verdict changes so far (for the release notes, decision 3)

| Change | Before | Now |
|---|---|---|
| A T-SQL result column alias renamed (`AS OrderId` to `AS Id`) | unknown | different (`equiv.result-column-name`) |
| A T-SQL result column alias whose only change is letter case (`AS OrderId` to `AS orderid` or `[ORDERID]`) | equal under `ASM-TSQL-CI-COLLATION` | different, or unknown inside a compound statement |
| Result columns whose names differ only in letter case across dialects (T-SQL `Id`, PostgreSQL unquoted `id`, Db2 for i `ID`) | equal | equal under `ASM-RESULT-NAMES-CI` |
| A rewritten same-dialect T-SQL statement that also changes the case of names | equal under assumptions without `ASM-TSQL-CI-COLLATION` | with it (also on `main`) |
| Instances whose converted PostgreSQL returns a column under a lower-case name | converted | under assumptions (`ASM-RESULT-NAMES-CI`) |
| B adds `ORDER BY` to a top-level result A leaves unordered (no TOP or LIMIT) | unknown | equal, `equivalent: false`, with the reason in `refinements` |
| Every "equal" or "equal under assumptions" | (no field) | `equivalent: true` unless a one-way step was used |
| A T-SQL procedure that sends rows-affected counts (no `SET NOCOUNT ON` first) against a PostgreSQL or Db2 for i routine, which sends none | equal under assumptions, without it | under `ASM-OBSERVER-ROWCOUNT`; converted procedures and instances are "under assumptions" instead of "converted". Decided 2026-10-28: no special handling; a team whose callers do not read the counts accepts `ASM-OBSERVER-ROWCOUNT` in its pack |
| `SET NOCOUNT` changed within one dialect, `[observer] row_counts = "never"` | different | equal under `ASM-OBSERVER-ROWCOUNT` |

## Leads found while planning

Each needs a test before it counts as a bug; outcomes are recorded here.

- Fixed (on `main`): a same-dialect T-SQL statement that
  changed both the case of its names and something statement equivalence
  proves (operand order, `DELETE` without `FROM`) was "equal under
  assumptions" without `ASM-TSQL-CI-COLLATION`, although the lowering folds
  the names (`canon::lc`) and `dbo.Orders` and `dbo.orders` are different
  tables under a case-sensitive collation. `name_assumptions` now adds the
  assumption when a name or variable is spelled in more than one case
  across the two statements. The raw terms of statements that fail to lower
  fold the same way and are covered by the same check.
- Checked, not a bug: collection roll-ups that default to "equal" when
  empty (`.unwrap_or(Verdict::Equal)`). Two empty trees are equal (nothing
  is observable on either side); a file without a dialect makes `tree`
  unknown, an unreadable file or a missing folder is an error (exit 2).
- The T-SQL to PostgreSQL converter writes `SELECT Id` as `SELECT id`, so
  the result column comes back as `id` and the conversion needs
  `ASM-RESULT-NAMES-CI` (decision 1). Writing `id AS "Id"` where the case
  differs would keep the name and drop the assumption.
- One assumption id, two settings: `ASM-TSQL-CI-COLLATION` stands both for
  names and variables comparing case-insensitively and for string
  comparisons using a case-insensitive collation (`canon::asm::TSQL_CI`).
  T-SQL takes these from different collations, so confirming one in a
  review also confirms the other. To be split, with care for the review
  store, which keys on assumption ids.
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
