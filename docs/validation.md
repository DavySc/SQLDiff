# Validation

How the design §12 checks run, and how a work package proves its §14 gate.

| Check | Where | Runs in |
|---|---|---|
| Corpus golden runner | `crates/sqldiff-corpus` (`tests/golden.rs`) | `cargo test`, `validation / golden` |
| Soundness: never "equal" where the corpus says otherwise | `tests/golden.rs` | same |
| Parser round trip and error budget (< 1% of statements, M2) | `crates/sqldiff-corpus/tests/fuzz.rs`, `fuzz/` | `cargo test`, `validation / fuzz` |
| Fault-injection execution oracle | `corpus/tools/fault_oracle.py`, `corpus/oracle/*.toml` | `validation / oracle` |
| Core passes with no pack loaded | empty `HOME`/XDG dirs, no config in the tree | `validation / golden` |
| AI separation (placeholder) | no network client in engine crates; byte-identical runs | `validation / separation` |
| Fixture format, creation on PG 18 / SQL Server 2022 | `corpus/tools/check_corpus.py` | `corpus` workflow |
| Db2 for i vendor-fact probes (run by hand on IBM i) | `probes/db2i`, `crates/sqldiff-corpus/tests/db2i_probes.rs` | `cargo test`, `CI / test` |

## Corpus golden runner

Every `[[expect]]` in `corpus/pairs` and `corpus/mutations` runs through
`sqldiff_core::diff` and is judged:

- the verdict must match;
- for "different", every expected class must have a finding of that class whose own
  verdict is "different", so a case only counts when it is caught for the right
  reason and not by an unrelated text edit;
- for "equal-under-assumptions", at least as many assumptions as the case lists
  (wording is not compared).

Expectations the engine cannot meet yet are listed in
`crates/sqldiff-corpus/known-failing.toml`, each with the work package expected to fix
it. The test fails when an unlisted expectation fails (a regression) and when a
listed one passes, so the list only shrinks. To prove a gate, make the change, run
the tests, and delete the entries it fixes.

```
cargo run -p sqldiff-corpus --bin corpus-report            # table and gate status
cargo run -p sqldiff-corpus --bin corpus-report -- m13 p0  # filter by id prefix
cargo run -p sqldiff-corpus --bin corpus-report -- --json
cargo run -p sqldiff-corpus --bin corpus-report -- --write-known-failing
```

Milestones M5a/M5b (behavioural diff) are proved by
`crates/sqldiff-core/tests/witnesses.rs` together with the oracle run; see
`docs/behavioural-diff.md`.

## Fuzzing

`sqldiff_corpus::fuzz` generates well-formed routines and scripts for each dialect
(T-SQL procedures with TRY/CATCH, PL/pgSQL with exception blocks, Db2 SQL PL with
handlers, labels and ATOMIC compounds, plus nested DML and expressions). The test
checks round trip, that compounds are recognised as compounds, and that error nodes
stay under 1% of statements. It also mutates every corpus file (deletions,
duplications, swaps) and checks round trip. `SQLDIFF_FUZZ_ITERS` raises the count.

The same generator drives the cargo-fuzz targets in `fuzz/` (nightly):

```
cargo +nightly fuzz run parse_bytes     # arbitrary text: no panic, no lost bytes
cargo +nightly fuzz run parse_grammar   # generated programs: no error nodes
```

## Execution oracle

See "Execution oracle" in `corpus/README.md` for the spec format. Each scenario runs
on both sides without a fault and once per fault point; a fault point makes writes to
one table fail with a constraint violation (CHECK for INSERT/UPDATE/MERGE, a foreign
key from a shadow table for DELETE). These are ordinary statement errors on both
engines, not trigger errors, which abort the batch in SQL Server. Outcomes compared:
error reached the client (plus error codes when both sides run on one engine),
returned rows, and durable state read in a fresh session.

Limits: one error kind (constraint violation), one session (no concurrency, so m03,
m04 and the lock parts of p02/p13 are not exercised), autocommit callers only, and
time-dependent columns are left out of the observed state. Db2 for i has no container
and is not covered; its vendor facts are probed by hand instead (`probes/db2i`).
