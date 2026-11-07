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
