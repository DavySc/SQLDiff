# Semantic profiles

The error and transaction behaviour of each dialect (design §4, §7) lives as
data in `crates/sqldiff-core/profiles/semantics/<dialect>.toml`. The code in
`crates/sqldiff-core/src/ir/` (CFG lowering, the transaction interpreter and
the behavioural comparison) reads these tables. It does not hard-code
vendor rules.

Every row has an `id`, a `status` (`confirmed`, `unverified` or `assumed`) and,
where it rests on a checked vendor fact, `fact = "Vn"` from
[vendor-facts-verification.md](vendor-facts-verification.md). A finding that
depends on a row that is not `confirmed` names it as an assumption
(`ASM-FACT-Vn` or `ASM-ROW-<id>`).

## Sections

| Section | Meaning |
|---|---|
| `[error_kinds.<kind>]` | `class` (`error`/`warning`) and vendor `codes`. `xx*` is a code class, `*` the catch-all. The kinds are constraint, conversion, arithmetic, cardinality, not_found, lock, user_signal, name_resolution, cancel and other. |
| `[conditions]` | Handler condition names (`OTHERS`, `SQLEXCEPTION`, `CATCH`, …) mapped to the kinds they catch. |
| `[raises]` | The kinds each statement category can raise. Fault scenarios are drawn from this. |
| `[[unhandled]]` | What an uncaught error does. The first row whose `when` matches wins. |
| `[[caught]]` | What a caught error does, by `construct` (`try_catch`, `exception_block`, `continue_handler`, `exit_handler`, `undo_handler`). The first match wins. |
| `[transactions]` | Scalar rules: nesting, savepoints, commit inside functions/exception blocks/atomic blocks/caller transactions, doomed commits, deferred constraints. |
| `[transactions.routine_mode]` | Who owns the transaction for each routine kind (`caller`, `call`, `autocommit`, `commit_level`). |
| `[atomicity]` | Statement, routine-body, compound and MERGE atomicity. |
| `[[temp]]` | Temporary objects: `on_rollback` (`keep`, `revert`, `empty`), `on_commit` and lifetime. |
| `[[cardinality]]` | Many-row / no-row behaviour per form (SELECT INTO, scalar subquery, UPDATE FROM, MERGE, DML). |

## Rows for errors

`when` may test `xact_abort`, `no_commit`, `in_txn`, `context` (`routine` or
`script`) and `caller_txn`. `kinds` limits the row to some error kinds, and
`form` (`throw`, `raiserror`) limits it to one raising statement. The effect
fields are:

- `statement`: `rollback` or `partial` (the failing statement's own work).
- `transaction`: `none`, `rollback_all`, `doom` or `rollback_scope`.
- `control`: `next` (go on with the next statement) or `exit`.

## Coverage

`crates/sqldiff-core/tests/semantics.rs` holds the M4 gate. The test
`every_row_has_a_test` fails if any row id in these files has no behavioural
case or value test. `tests/formal_cases.rs` replays the cases of
`formal/profiles/error-txn.toml` against the interpreter when that file exists.
