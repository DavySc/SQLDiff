# Formal model of error and transaction semantics

TLA+ model of how T-SQL (SQL Server 2022), PL/pgSQL (PostgreSQL 18) and SQL PL
(Db2 for i) behave when a statement fails inside a routine, checked by TLC
against the section 7 profile tables of [the design](../docs/design.md)
(section 12, "Error/transaction model"; M4 gate). Facts come from
[the vendor facts check](../docs/vendor-facts-verification.md), with the
corrections applied.

| File | What it is |
|---|---|
| `profiles/error-txn.toml` | The profile tables as data: one `[[cell]]` per table row and dialect, one `[[case]]` per scenario with its expected outcome |
| `gen_cases.py` | Turns the TOML into `ProfileCases.tla`; checks every modelled cell has a case and every program fits its dialect |
| `ProfileCases.tla` | Generated. Do not edit |
| `ErrorTxn.tla` | The semantics: an interpreter over abstract statements, per dialect |
| `Cases.cfg` | TLC: each case ends in its expected outcome |
| `Explore.cfg` | TLC: each case's program under every fault plan; safety invariants, durability never shrinks, termination |
| `run-tlc.sh` | What CI runs (`.github/workflows/formal.yml`) |

Run it with `formal/run-tlc.sh` (needs Java 11+ and Python 3.11+; downloads
the pinned `tla2tools.jar` unless `TLA2TOOLS` points at one). Both
configurations finish in a few seconds.

## What the model covers

A routine is a program over abstract statements: writes (tags, optionally two
rows), `BEGIN`/`COMMIT`/`ROLLBACK`, savepoints, raising (`THROW`, `RAISERROR`,
`RAISE`, `SIGNAL`, notices), status checks (`@@ERROR`, `@@ROWCOUNT`/`FOUND`/
SQLSTATE 02000), `RETURN`, and structured handlers (TRY/CATCH, `BEGIN …
EXCEPTION WHEN OTHERS`, compound statements with CONTINUE/EXIT/UNDO handlers,
`ATOMIC`). Settings: `XACT_ABORT`, `IMPLICIT_TRANSACTIONS`, the Db2 commit level
(`*NONE` or under commitment control), and whether the caller already holds a
transaction with pending work of its own. A fault plan says which writes fail
and how: ordinary error, T-SQL batch-aborting error, uncatchable error, or not
found.

The outcome is what the caller is left with: committed tags, pending tags,
whether an error reached the caller, whether execution stopped early, the
return code, `@@TRANCOUNT`, and whether the caller's transaction is doomed.

Rules that rest on the corrected facts:

- **V2**: under `*NONE` a multi-row statement that fails keeps the rows done before the error; `NOT ATOMIC` MERGE keeps them under commitment control too.
- **V4** (unverified): under `*NONE`, ATOMIC and UNDO undo nothing. Cases relying on it carry `verified = false`.
- **V6**: Db2 `COMMIT` in a procedure commits the caller's pending work too (ACTGRP(*CALLER)); a routine leaves pending work to the caller (COMMIT ON RETURN NO).
- **V10**: which T-SQL errors abort the batch with `XACT_ABORT OFF` is not published, so it is a fault kind (`batch`) the pack list decides, not a rule in the model.
- **V14**: Db2 savepoints need commitment control.
- **V17**: PostgreSQL `COMMIT`/`ROLLBACK` only in a top-level CALL and not inside an exception block.
- **V18**: `WHEN OTHERS` does not catch `QUERY_CANCELED`/`ASSERT_FAILURE` (fault kind `uncatchable`).
- T-SQL: `THROW` ends the batch even with `XACT_ABORT OFF` (transaction left open), `RAISERROR` never honours `XACT_ABORT`, an error in TRY with `XACT_ABORT ON` leaves the transaction uncommittable (error 3930 on the next write), bare `ROLLBACK` ends the outermost transaction, a trancount mismatch at exit raises error 266.

## Not modelled

Cells marked `not-modelled` in the TOML, each with a reason: the unterminated
`THROW`/`ROLLBACK` row (lexical, handled by the parser and early checks),
journaling (catalog fact), non-transactional state in T-SQL and PostgreSQL
(table variables, identity, sequences; belongs with the WP3 state model), and
DDL. Also outside the model: deferred constraints surfacing at `COMMIT` (V22),
triggers, nested routine calls, `RESIGNAL`, named condition handlers, and
batch-end behaviour beyond the routine.

## Adding a case

Add a `[[case]]` to `profiles/error-txn.toml` (syntax at the top of the file),
run `python3 formal/gen_cases.py`, then `formal/run-tlc.sh`. A failing case
prints the outcome TLC computed next to the expected one. When a fact is
confirmed or refuted on a real system, change the rule in `ErrorTxn.tla` and
the affected expectations together, and flip `verified`.

## For the IR and lowering rules (WP3)

The cell ids (`error.after-runtime-error.db2i`, …) name the rows of the
section 7 tables, and the cases are the executable form of those rows. The IR's
transaction model can load the same TOML and replay each case through its own
lowering as a unit test, so the Rust model and this spec are checked against
the same table.
