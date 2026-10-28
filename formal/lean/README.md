# Lean checker (`sqldiff-check`)

The verified gate of [the verification roadmap](../../docs/verification-roadmap.md)
(decision 2): SQLDiff reports "equal" only when this checker confirms it, in
`require` mode (decision 3).

| File | What it is |
|---|---|
| `Sqldiff/Syntax.lean` | Generated from the Rust syntax kinds (`sqldiff_core::proof::lean_syntax`), with the schema hash. Do not edit: `SQLDIFF_BLESS=1 cargo test -p sqldiff-core --test lean_syntax` writes it, and that test fails while it is stale |
| `Sqldiff/Ast.lean` | Generated from the abstract syntax schema (`sqldiff_syntax::encode::SCHEMA`, `sqldiff_core::proof::lean_ast`): routines, statements and expressions as Lean types, and the decoder for their one-line encoding. Do not edit; the same bless command writes it |
| `Sqldiff/Tree.lean` | The syntax tree as Lean types and its decoder: an unknown kind, a token where a node belongs or the reverse, or any other text fails to decode |
| `Sqldiff/Gate.lean` | Programs, refinement (decision 1), the check, and `check_sound`: a confirmed answer means B refines A, for every semantics |
| `Sqldiff/Core/Syntax.lean` | The core language of phases C1, C2a and C3a ([verified-conversion.md](../../docs/verified-conversion.md), decision 9): parameters with modes and defaults, locals, assignment, `if`, `while`, `return`, sequences, calls, and the data statements `insert`, `update`, `delete` on a table definition (`TableDef`); expressions are slice expressions (`SExpr`: variables, columns, 32-bit integer literals, NULL, `+ - * / %`, negation, comparisons, `AND OR NOT`, `IS [NOT] NULL`, `coalesce`, `CASE WHEN`) or uninterpreted operators; what a routine's statements can do (raise, violate a constraint, fail after a write) |
| `Sqldiff/Core/Semantics.lean` | Values (NULL, three-valued booleans, 32-bit integers, opaque values), errors (overflow, division by zero, constraint violations), the meaning of slice expressions, the database (tables as lists of rows, observed as bags), the data statements, the fuel-based, executable interpreter, what a caller observes (out parameters, returned value, world and database after a finished call; error kind and calls for a failed one; decision 1) and `Refines` |
| `Sqldiff/Core/Data.lean` | The data statements: congruence lemmas (`insertRows_map`, `updateRows_map`, `deleteRows_map`) and statement atomicity (`exec_error_atomic`) |
| `Sqldiff/Core/Rename.lean` | An injective renaming of a routine's variables is observationally the same routine (`refines_rename`) |
| `Sqldiff/Core/Fuel.lean` | Once a run finishes (with a result or an error), more fuel gives the same observation (`obs_mono`) |
| `Sqldiff/Core/OpFree.lean` | A routine without uninterpreted operators means the same whatever they mean (`Routine.obs_opFree`) |
| `Sqldiff/Mirror/Lower.lean` | `lowerT` and `lowerP`: the T-SQL and PL/pgSQL spec of the C1 constructs, the C2a slice and the C3a data statements (each rule with its vendor citation and probe status), as lowerings of the generated `Sqldiff.Ast.Routine` into the core; the trusted inputs (`Deployment`, `Catalog` with its tables); partial (`none` is "outside the spec"); `sliceT`, `sliceP`, `abortsT`: whether a routine is in the C2a slice |
| `Sqldiff/Mirror/Check.lean` | The mirror check with an untrusted certificate (the variable renaming), `check_sound`, `check_sound_c2a` and the residual assumptions of a C2a answer |
| `Sqldiff/Mirror/Request.lean` | The fields of a `sqldiff-check/3` mirror request and their decoding (C3a: table mappings in DEPLOYMENT, table entries in CATALOG); the extra answer fields (`scalars`, `residual`, `statements`) |
| `Sqldiff/Mirror/Examples.lean`, `Sqldiff/Mirror/ExamplesC3a.lean` | Confirmed and refused pairs and interpreter runs (C1 and C2a; C3a with table states), as `#guard`s checked on every build |
| `Main.lean` | The executable: one tab-separated request per line on stdin (`sqldiff-check/2`, with the schema hash; `sqldiff-check/3` for the mirror check, with the abstract syntax schema hash), one JSON answer per line on stdout (the answer format is documented there). A request whose schema hash is not the checker's, or whose trees or routines do not decode, is unconfirmed |
| `Axioms.lean`, `check_axioms.py` | `#print axioms` for every theorem the gate relies on, checked against the allowlist (Lean's three standard axioms; vendor facts only through a signed `SPEC.lock`, decision 5) |
| `test_gate.py` | Protocol tests against the built binary |
| `measure.py` | Start-up, throughput, size and linked libraries (decision 2 budgets) |

Phase 0 covers the trivial fragment only: both sides in the same dialect,
analysed for the same target, with identical syntax trees once whitespace
and comments are left out. The proof is reflexivity, so it needs no
specification and no axioms. What it trusts: SQLDiff's parser,
`sqldiff_core::proof::tree_line`, which writes a tree injectively, and
`Tree.decode`, which reads it back. A syntax kind added, removed or renamed
in Rust changes the schema hash, so an old checker refuses (unconfirmed)
until `Syntax.lean` is generated again and the checker rebuilt.

The toolchain is pinned in `lean-toolchain`. Executable code imports only
`Init`. `.github/workflows/formal-lean.yml` builds and tests it on every
release target, and on Linux runs the Rust CLI's proof tests against it
(`SQLDIFF_REAL_CHECK`).

### Phase C1: the mirror check (`sqldiff-check/3`)

`Sqldiff.Mirror.check dep Γ cert a b`, served as the `sqldiff-check/3`
request kind (`Main.lean`; `/2` is unchanged), confirms a T-SQL routine `a` and a
PL/pgSQL routine `b` when `lowerP Γ b` is `lowerT dep a` with its variables
renamed by `cert`, the renaming injective on `a`'s variables.
`check_sound` says a confirmed answer means `b` refines `a` for every
meaning of the operators and every world. Trusted, beyond the above: the
lowering (it is the spec), the deployment facts `dep` (whether row counts
are observed, which T-SQL routine name reaches which target routine) and
the target catalog `Γ` (callee parameter modes). Not trusted: the
certificate.

What a C1 confirmation does not cover: operators are uninterpreted, and
the same operator name is taken to mean the same function in both
dialects (`ASM-C1-OPERATORS` on the Rust side), so it is about the control
structure, variables, parameters, calls and the T-SQL return status, not
about real T-SQL against real PostgreSQL expressions. There is no data
(C3) and no error handling (C4).

### Phase C2a: the first scalar slice

The core now has values (NULL, three-valued booleans, 32-bit integers,
and opaque values for everything else), an error outcome (overflow,
division by zero) and a fixed meaning for the expressions of one slice
(`Sqldiff.Core.SExpr.eval`). An error ends the routine, and the caller
observes its decision-1 kind (`arithmetic`) and the calls made before it.
(Until C3a the failed observation also carried the world; it no longer
does, see C3a below: what a caller sees of the state after a failed call
differs between the dialects.) Each dialect's lowering types its expressions and lowers the
slice to `SExpr`; everything else stays on the C1 path, an uninterpreted
operator.

The slice (`Sqldiff/Mirror/Lower.lean`, one comment per rule with its
Microsoft Learn page, PostgreSQL manual section and probe status):

- variables declared T-SQL `int`/`integer`, PostgreSQL
  `integer`/`int`/`int4`; integer literals in the 32-bit range; NULL in a
  value position;
- `+ - * /` on two integers (overflow raises, division truncates toward
  zero, a zero divisor raises), `%` with a literal divisor other than 0
  and -1, unary minus;
- `= <> < > <= >=` on two integers, three-valued; `AND`, `OR`, `NOT` with
  the three-valued tables (`AND`/`OR` only when neither operand can
  raise); `IS [NOT] NULL`;
- `COALESCE` of integers, T-SQL `ISNULL(x, y)` of integers when `y`
  cannot raise (the same core term as `COALESCE(x, y)`), searched
  `CASE WHEN ... THEN ... [ELSE ...] END` of integers;
- `IF` and `WHILE` take the branch only on TRUE; assignment of an integer
  or NULL to an integer variable; `SET XACT_ABORT ON`.

An expression without variables that fails (`1/0`) is outside the slice:
PostgreSQL may raise it while planning, even in a branch not taken.

A confirmed `/3` answer says which case applies. When every expression
of both routines is in the slice, neither lowering has an uninterpreted
operator, and the T-SQL routine either starts with `SET XACT_ABORT ON` or
has no expression that can raise, the answer names
`Sqldiff.Mirror.check_sound_c2a` and carries `"scalars":"c2a"` and
`"residual":[...]`; otherwise it names `Sqldiff.Mirror.check_sound` and
carries `"scalars":"uninterpreted"`.

Trust statement. `check_sound_c2a`: both routines are in the slice, the
PL/pgSQL routine refines the T-SQL one for every world, and both mean the
same whatever the uninterpreted operators mean (`Routine.obs_opFree`), so
the confirmation does not rest on `ASM-C1-OPERATORS`. It rests instead on
the slice rules being right (trusted spec: each cites the vendor text and
says whether `eval/` probes cover it; most are not probed yet), and on the
T-SQL session facts in `residual`: `ASM-TSQL-ARITH-ERRORS` when an
expression can raise (`ANSI_WARNINGS`/`ARITHABORT` not both `OFF`, so
overflow and division by zero raise rather than give NULL) and
`ASM-TSQL-ANSI-NULLS` when it uses `=` or `<>` (the routine was created
with `ANSI_NULLS ON`). The other trusted inputs are unchanged (the
lowering, `dep`, `Γ`, the parsers).

What stays under `ASM-C1-OPERATORS` (a `check_sound` answer): every
other type (`bigint`, `decimal`, strings, dates, `bit`, ...), casts and
implicit conversions, function calls other than `COALESCE`/`ISNULL`,
`%` with a variable divisor, simple `CASE`, `IN`, `BETWEEN`, `LIKE`,
comparisons with a NULL literal, `AND`/`OR` over operands that can raise,
and T-SQL routines whose expressions can raise but that do not start with
`SET XACT_ABORT ON` (T-SQL goes on after a failed statement; PostgreSQL
stops). For those the core still evaluates the slice parts exactly, but
the answer also rests on the C1 caveat that runs raise no error.

### Phase C3a: single-table data statements

The core has a database: each table a list of rows, a row one value per
column of the table's definition, which the target catalog gives
(`Sqldiff.Core.TableDef`). Three statements write it: `insert`,
`update` and `delete`, whose expressions may read the current row's
columns (`SExpr.col`). The observation of a finished call now includes
the database it leaves (decision 1: durable state); `Refines` quantifies
over the database a call starts from, as over the arguments and the
world. A callee may read and write the database (`WorldModel.call`).

**Lists, observed as bags.** The engines promise no row order. The core
keeps a list, and every statement treats it as a bag would: `insert`
appends, `update` maps each row on its own, `delete` filters, and
whether a statement fails never depends on the order (within the slice,
a statement cannot fail with two error kinds, see below). So equal lists
from one start mean equal bags, and a confirmation claims nothing about
order.

**What C3a covers** (`Sqldiff/Mirror/Lower.lean`, "The C3a slice: data
statements", one comment per rule):

| Rule | Vendor text | Probed |
|---|---|---|
| `INSERT INTO t (c1..cn) VALUES (...), ...`: listed columns get the values, the others NULL; at least one row, each of `n` values; no query source, `DEFAULT VALUES`, `RETURNING`, `ON CONFLICT`, or omitted column list | "INSERT (Transact-SQL)" (a column not in column_list that is nullable gets NULL); PostgreSQL "INSERT" ("filled with ... null if there is none") | SQL Server incidentally (eval/casts/assign: an unlisted column is NULL); PostgreSQL not probed |
| `VALUES` sees no columns: a PL/pgSQL name there is a variable even when the table has that column | PostgreSQL column resolution ("cannot be referenced from this part of the query"); T-SQL refuses column names in `VALUES` | not probed |
| `UPDATE t SET c = e, ... [WHERE p]`: every `e` reads the row's old values; key columns are never set; no alias, no `FROM` (T-SQL `UPDATE ... FROM` is outside C3a), no `OUTPUT`/`RETURNING` | PostgreSQL "UPDATE" ("can use the old values of this and other columns"); "UPDATE (Transact-SQL)" (`SET @v = column, column = expression` "sets the variable to the pre-update value"; stated for variables, reviewers check) | not probed |
| Setting a key column is outside the spec | PostgreSQL "CREATE TABLE", "Non-Deferred Uniqueness Constraints" (checked row by row, so `SET k = k + 1` can fail by row order) | not probed |
| `DELETE FROM t [WHERE p]`; no alias, second `FROM`, `USING`, `OUTPUT`/`RETURNING` | PostgreSQL "DELETE"; "DELETE (Transact-SQL)" | not probed |
| `WHERE` keeps rows where `p` is TRUE (FALSE and UNKNOWN skip) | PostgreSQL "UPDATE"/"DELETE" ("Only rows for which this expression returns true"); "UPDATE (Transact-SQL)", "Search condition (Transact-SQL)" | SQL Server incidentally (eval/bit-boolean: `UPDATE ... WHERE id = 2` changes one row) |
| A NULL in a `NOT NULL` or key column, or a duplicate key, is a `constraint` error (515/23502, 2627/23505); a key column is `NOT NULL` | both vendors' error lists; PostgreSQL "CREATE TABLE" | not probed |
| Statement atomicity: a failing data statement has no effect, then the routine ends as for any error (T-SQL only under leading `SET XACT_ABORT ON`) | "SET XACT_ABORT (Transact-SQL)"; PostgreSQL "Transactions" (every statement runs in a transaction) | not probed |
| Values written to a column are `int` expressions of the slice or NULL, even on the C1 path (another type converts on the write, differently per dialect) | "Data type conversion" / PostgreSQL "Value Storage" | the difference is probed (eval/casts/assign: T-SQL truncates, PostgreSQL rounds) |
| A `WHERE` that can raise is outside the slice (access paths decide which rows are tested); `UPDATE` values are evaluated only on the kept rows (PostgreSQL's logical order; one order T-SQL may take, and T-SQL is the reference side) | PostgreSQL "The WHERE Clause" | not probed |
| A data statement that could fail with both an arithmetic and a constraint error is outside the slice (which is reported depends on row order) | decision 1 ("which of several violations is reported" is unspecified) | n/a |
| A part of an `UPDATE` value that reads no column and can raise is outside the slice (PL/pgSQL may plan with the variables' values and fold it) | PL/pgSQL "Plan Caching"; "PREPARE" (custom plans); 9.18.1 "CASE" | not probed |
| Names: T-SQL `Name` in a data statement is a column, matched exactly to the catalog's T-SQL spelling (ASCII, distinct up to case, so collation-independent); PL/pgSQL names fold, and a name that is both a column of the target and a variable (parameters, locals, `found`) is outside the spec, as is the table's own name (a whole-row reference) | "Database identifiers" (collation); PL/pgSQL "Variable Substitution" ("will report an error if a name ... could refer to either a variable or a table column"), "Obtaining the Result Status" (`FOUND`) | not probed |
| Tables: T-SQL names through the deployment's table map (`dbo.t` to `t`, `ASM-DBO-SCHEMA`); PostgreSQL names folded, looked up in the catalog | as for routines | n/a (trusted input) |
| Row counts: with `rowCounts = true` every T-SQL data statement is outside the spec (T-SQL sends a count unless `NOCOUNT`, PostgreSQL none; C1 already keeps `SET NOCOUNT` outside then); otherwise not observed | "SET NOCOUNT (Transact-SQL)" | not probed |
| Data statements only in procedures | "CREATE FUNCTION (Transact-SQL)" (functions cannot modify the database state) | n/a |

**The failed observation.** A failed call now shows its error kind and
the calls made before it, not the world or the database. After an error,
T-SQL under `XACT_ABORT ON` rolls back an open transaction and, with none
open, keeps the statements before the failing one committed; PostgreSQL
rolls back the whole `CALL`. So the state after a failed call differs
between the dialects whenever something was written before the error,
and C4 models it. A C2a answer whose routine can fail after a write (a
data statement or a call) carries `ASM-STATE-AFTER-ERROR` in `residual`:
the caller does not rely on the state after a failed call. (This also
corrects C2a, which observed the world after a failed call.)

**Assumptions** a C3a confirmation rests on, beyond C1 and C2a:

- the catalog (trusted, from the request's CATALOG): each table has
  exactly the listed `int` columns, `NOT NULL` and primary key, in both
  databases, and nothing else that changes a write: no other constraint,
  default, identity, sequence, computed or generated column, trigger, rule
  or row-level security; the key is not `DEFERRABLE INITIALLY DEFERRED`
  and not `IGNORE_DUP_KEY`; no inheritance children. The Rust side sends a
  table only when the catalogs show this (`Sqldiff.Mirror.CatTable`);
- the deployment's table map (`ASM-DBO-SCHEMA`, `ASM-DEFAULT-SCHEMA`) and
  the column spellings (the state correspondence of decision 1);
- `ASM-OBSERVER-ROWCOUNT` when the deployment says row counts are not
  read (the deployment's choice, as before);
- the residuals of the answer: `ASM-TSQL-ARITH-ERRORS`,
  `ASM-TSQL-ANSI-NULLS` (it also decides which rows a `WHERE` with `=`
  matches), and `ASM-STATE-AFTER-ERROR`;
- the parser: `sqldiff_syntax` reads routines with the extended parser,
  which drops a T-SQL `OUTPUT` clause on `INSERT`/`UPDATE` and the
  qualifier of `SET t.c = e`; the Lean side cannot see either, so the Rust
  side must refuse routines that have them before sending C3a requests.

`ASM-PG-VARIABLE-CONFLICT-ERROR` is not needed: every name that the
setting would decide is refused, so the lowering means the same under
`error`, `use_variable` and `use_column`.

**Trust statement.** `check_sound` and `check_sound_c2a` now cover data
statements: a confirmed answer means the lowered PL/pgSQL procedure
refines the lowered T-SQL one for every argument list, world and starting
database, including the database a finished call leaves; with
`check_sound_c2a`, whatever the uninterpreted operators mean. The answer's
`"statements":"c3a"` says the routines have data statements. The
confirmation rests on the C3a rules above being right (trusted spec,
mostly not probed yet), on the catalog and deployment facts, and on the
residuals; it does not cover the state after a failed call (C4), `MERGE`,
`UPDATE ... FROM`, queries, `RETURNING`/`OUTPUT`, or any type but `int`.
Theorems with only the standard axioms: `check_sound`, `check_sound_c2a`,
`refines_rename`, `obs_mono`, `Routine.obs_opFree`, `exec_error_atomic`
and the congruence lemmas (`Axioms.lean`).
