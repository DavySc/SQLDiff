# Crate split (performance plan W4.1)

Purpose: a change to the conversion code should not rebuild, and should not
change the engine fingerprint of, code it cannot affect. Correctness comes
first: every step is a mechanical move with no change in behaviour, checked by
byte-identical conversion output.

## Decision

Split `sqldiff-core` into layers, one PR per layer, starting at the bottom
(the part with the fewest dependencies on the rest of the engine):

1. `sqldiff-catalog`: `catalog` (tables, columns, DDL replay, type and
   built-in tables) and `tokens`, with `profiles/builtins.toml`. This PR.
2. Later layers, only where the dependency graph below allows a clean cut.

`sqldiff-core` re-exports the moved modules (`pub use sqldiff_catalog as
catalog; pub use sqldiff_catalog::tokens;`), so every path
`sqldiff_core::catalog::...` and `sqldiff_core::tokens::...` keeps working and
no caller changes.

## What the first move changes

- Source files move with `git mv`; only the two `crate::catalog::` paths in
  `tokens.rs` and `ddl.rs` and the `include_str!` path in `builtins.rs`
  change.
- `build.rs` of `sqldiff-convert` lists the crate, so the engine fingerprint
  still covers the moved sources. The fingerprint changes once, at this PR.
- The purity guard (`check_purity`) scans the new crate and its allowances
  moved with the files; the network-client check in CI lists it.

## Dependency finding

`catalog` and `tokens` depend only on `sqldiff-syntax`, which is why they are
the first cut. The rest of `sqldiff-core` has cycles (`model`, `ir`, `ledger`,
`engine` and `equiv` reach into each other). Pass 1 of the conversion
(`sqldiff-convert/src/tsql.rs`, the drafts) uses `equiv::canon`, `literal`,
`dynamic` and `cursor`, so drafts depend on most of `equiv`. A cut at
`equiv` alone therefore buys little for the fingerprint until the cycles
between `model`, `ir`, `ledger` and `engine` are broken. Those cycles are the
next piece of work; each break is its own PR.

## Checks for each step

- `cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`,
  full `cargo test --workspace`.
- Conversion output of a fixed subset is identical before and after
  (`diff -r`, excluding `.sqldiff`), at `-j1` and `-j4`.

## Step 2: `sqldiff-model` (breaking the model/ir/ledger/engine cycles)

The cycles had one hub, `model` (the report types). `model` used six things
that sat in modules which themselves use `model`:

| `model` used | was in | now |
|---|---|---|
| `assumptions::lookup` (a debug check) | `assumptions` | moved into `sqldiff-model` |
| `session::derive_verdict` | `session` | moved |
| `proof::Proof` | `proof` (with `artifact`) | moved with `artifact` and `target` |
| `ir::witness::Witness` | `ir/witness.rs` (with `ir/profile.rs`) | moved to `sqldiff-model::witness` and `::profile` |
| `ledger::Discharge` | `ledger` | the struct moved into `model`; `ledger` re-exports it |

`sqldiff-model` holds `model`, `assumptions`, `session`, `proof`, `artifact`,
`target`, `profile` and `witness`, plus their data (`profiles/assumptions.toml`,
`lifecycle.toml`, `semantics/*.toml`). It depends only on `sqldiff-syntax`.
`sqldiff-core` re-exports the modules under their old paths
(`sqldiff_core::model`, `ir::profile` and so on), so no caller changes.

What stays in `sqldiff-core` and is still tangled: `ir` (cfg, interp,
behaviour), `ledger`, `engine`, `facts`, `classify`, `equiv`. `ledger` uses
`ir::cfg` and `ir::behaviour`, and `ir` uses `ledger`/`facts`/`equiv`: that
is the remaining cycle. `ledger` is the smallest piece to break next.

Two visibility changes the move forced, with no change in what can be done:
`Report.verdict` stays private (reads use `Report::verdict()`; reports are
built with `Report::draft` and settled by `finish`, as before) and
`proof::apply` is `pub`.

## Step 3: `sqldiff-equiv`

Measured module graph of `sqldiff-core` after step 2 (edges between its
top-level modules): `classify`, `normalize`, `hooks`, `phrase`, `render` are
leaves; `coverage` and `treediff` use each other and `normalize`; `equiv`
(47k lines) uses only `classify`, `normalize`, `coverage`, `treediff`, the
catalog and the model. Nothing among them uses `ir`, `facts`, `ledger` or
`engine`. They move together into `sqldiff-equiv` (data: `pg18_*.txt`).

`ledger` cannot be a lower layer: it calls `engine::diff`, `facts` and
`ir::cfg`, so it sits on top. The one edge from below into it is
`ir::behaviour` calling `ledger::declared_commit_level`, a pure text helper
that can move to `sqldiff-model` later.

What this buys: pass 1 of a conversion (`sqldiff-convert/src/tsql.rs`) uses
`equiv::canon`, `literal`, `dynamic` and `cursor`. With this step it no
longer needs `ir`, `engine` or `ledger` to be the same code to be the same
engine, once `sqldiff-convert` is split to depend on `sqldiff-equiv` alone.

Widened for the move: `equiv::script::routine_key` and `equiv::lower::is_do`
are `pub` (the CFG code in `ir` calls them).

## Step 4: `rules` moves into `sqldiff-equiv`; the plan for stage fingerprints

`rules` (the idiom rule engine and `profiles/rules/*.rules`) uses only the
equivalence layer and the model, and it is what pass 1 of a conversion
(`sqldiff-convert/src/tsql.rs`) calls. It moves into `sqldiff-equiv` with its
data, so the code that writes a draft depends on `sqldiff-syntax`,
`-catalog`, `-model` and `-equiv` and on nothing above them.

### Why stage fingerprints, and what they must never do

Today one fingerprint (`ENGINE_FINGERPRINT`) keys everything, so any edit,
also one in `ir` or `engine` that cannot change a draft, discards the stored
drafts and tree state, and any edit to the draft code discards results
whose draft did not change. The aim is that an edit invalidates only the
stored work it can change. A stale reuse is the worst outcome (a resumed run
would differ from a fresh one), so the cut is only allowed where it can be
proved, and two facts of the current code limit it (found in the review of
the first version of this section):

**1. Pass 2 runs draft code.** `check_with` takes a `rebuild` callback; in a
tree run (`tree.rs`, the `rebuild` closure) it calls
`rewrite_in_tree_plain` and so `tsql::with_plain_routines`, and checks a
*mixed* draft: routines that do best without exception blocks written plain,
the others with blocks. The result key holds `draft.canonical()`, which
covers the whole-file blocks draft and the whole-file plain draft, **not**
the mix. A check fingerprint that left out the draft code would therefore
reuse a stale mixed result after an edit that changes only the mix. "Pass 2
runs nothing of pass 1" is false today.

**2. The tree state is built by more than the draft producers.**
`build_tree_state` builds the source catalog fold (`tree.rs`), base catalogs
(`base::over`), the claims, the ledger's hand-written output (`review.rs`),
the target catalog with its stand-in tables and `refused::StandIns::build`,
which itself calls `rewrite_in` and `relax`. A tree-state fingerprint that
covered only `tsql`, `tok`, `table_vars`, `impossible` and `rewrite_*`
would reuse a stale tree state after an edit to `refused.rs` or the catalog
fold, and nothing would flag it (the per-file result keys stay consistent
through the target and stand-in digests, and the sampled re-check compares
drafts only).

### Decisions

- **The draft stage is all of pass 1**: `build_tree_state` and everything it
  calls. Until pass 1 is a crate of its own, split out of `tree.rs`, the tree
  state and the stored drafts stay under the full fingerprint. No gain is
  claimed for them before that.
- **Results get the first gain.** The result key keeps the full-engine
  closure *except* the draft producers, and the mix is made part of the
  result: the result stores the mixed routines (`plain_at`) and the mixed
  draft's text; on reuse the mix is built again (pass-1 cost only, no check)
  and compared, and a mismatch is a miss. That keeps the expensive part (the
  check) reusable after a rule tweak that leaves the output alone, and stays
  sound for an edit that changes only the mix. (Rejected: putting the draft
  fingerprint in the key whenever a rebuild can run, which loses the reuse
  exactly for the files with exception blocks; moving the mix choice out of
  pass 2.)
- **Cargo must enforce it.** The producers are called by pass 2 only through
  the `rebuild` closure that the orchestrator hands in, so the check code
  can live in a crate that does not depend on the producers, and the closure
  test (as in `engine_fingerprint.rs`) asserts the stage's set equals the
  Cargo closure. That needs the check code (`check_one`, `check_with`,
  reports, store) separated from `tree.rs`, which is the real work of PR 2.

### The cross-file claims

The mix rebuild reads `Claims::of(all inputs)`: the temporary table names
that table variables claim across the tree. It is not in the per-file result
key. It cannot change the mix alone: a clash makes the declaration of a
routine refuse (`tsql.rs`, `claims.clash`), the refusal depends on the table
name and the routine, not on whether that routine is written with or without
exception blocks, so the same refusal is in both whole-file drafts and in
`draft.canonical()`, and the key changes. This argument is a claim, not a
proof: PR 3 adds a test (a second file whose table variable takes the same
name, with and without exception blocks) and any case where the whole-file
drafts stay equal but the mix differs is a hole in the existing keys to fix
before anything else.

### `Debug` of `Draft` is the key

`canonical()` is built from `Debug`. Every type reachable from `Draft`
derives `Debug` today and `Shared` forwards to its `str`, so it is faithful.
When the draft types move, they move together, and a guard test asserts that
no type reachable from `Draft` has a hand-written lossy `Debug` (or
`canonical()` switches to the serde bytes).

### Next PRs

1. Move the draft types (`Draft`, `Out`, `Note`, `Status`, `Step`,
   `Written`, `RuleOptions`, `Shared`) to a small crate both stages cover, with
   the `Debug` guard. No change in behaviour.
2. Separate the check code from `tree.rs` (a `sqldiff-check` crate) and the
   producers (`tsql`, `tok`, `table_vars`) into `sqldiff-draft`. No change in
   behaviour.
3. Store the mix with the result, two fingerprints, closure tests, and the
   tests that an edit in each place invalidates exactly what it should: an
   edit to `tsql` that changes only the mix, an edit to a pass-1-only file
   outside `tsql` (`refused.rs`), an edit to `ir` (drafts kept, results not),
   and the claims case above. This is the behaviour change.

## Step 5: `sqldiff-units` (PR 1 of the stage-fingerprint plan)

The data both stages read moves into `sqldiff-units`, which depends on
`sqldiff-syntax` only: `Draft` (and `Out`, `RawUnit`), `Note`, `Step`,
`Status`, `Written`, `By`, `RuleOptions`, `Shared`/`Pool`, and the unit-id
hash (`ids_of`). `sqldiff-convert` re-exports them under their old paths.

What the move forced, none of it changes what is computed:

- `Draft`'s fields are `pub` (the construction and the splitting in
  `lib.rs` need them across the crate boundary) and its `pub(crate)` helpers
  (`well_formed`, `canonical`, `worst_note`) are `pub`.
- `Pool::share_notes(&Conversion)` becomes a loop in `tree.rs` (`Conversion`
  is not part of the units).
- `impl From<rules::Status> for Status` would be an orphan impl; it is the
  function `tsql::unit_status`.
- `tests/debug_is_complete.rs` guards the key: only `Shared` has a
  hand-written `Debug` (and it prints the string it holds), and a draft that
  differs in any one field prints differently, so `Draft::canonical()` (the
  `Debug` text) separates every pair of drafts.
