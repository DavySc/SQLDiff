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
whose draft did not change. The result key already contains the draft text
and the rule options (`store::CheckInput::key`), so a result stays valid when
the draft is byte-identical and the check code is the same. The aim:

| stage | stored under | fingerprint covers |
|---|---|---|
| draft (pass 1: drafts, tree state) | tree key | the draft crates only |
| result (pass 2: check of a draft) | result key (draft text inside) | everything pass 2 runs, **not** the code that wrote the draft |

A stale reuse is the worst outcome, so the cut must be enforced by Cargo
dependencies, not by a list: the draft stage becomes a crate whose
dependency closure is the draft fingerprint, and a test asserts that closure
equals the fingerprinted set (as `engine_fingerprint.rs` does today). The
check stage's set is the closure of `sqldiff-convert` minus the draft crate,
asserted the same way, and the draft crate's public API stays limited to
producing drafts; anything the check stage reads from a draft (its types and
accessors) lives in a crate both fingerprints cover.

Next PRs, each with no change in behaviour until the last:

1. Move the draft types (`Draft`, `Out`, `Note`, `Status`, `Step`,
   `Written`, `RuleOptions`) to a small crate both stages cover.
2. Move the producing code (`tsql`, `tok`, `table_vars`, `impossible`,
   `rewrite_*`) into `sqldiff-draft`.
3. Two fingerprints and their closure tests; key the tree state with the
   draft fingerprint and the results with the check fingerprint. This is the
   behaviour change (what stored work survives an edit), covered by tests
   that an edit in each crate invalidates exactly the stages it should.
