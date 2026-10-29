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
| `Sqldiff/Core/Syntax.lean` | The core language of phases C1, C2a, C3a and C4 ([verified-conversion.md](../../docs/verified-conversion.md), decision 9): parameters with modes and defaults, locals, assignment, `if`, `while`, `return`, sequences, calls, and the data statements `insert`, `update`, `delete` on a table definition (`TableDef`); (C3b) the read `query t c pick` (`Pick`: assign from the one kept row, count the kept rows, test for one); expressions are slice expressions (`SExpr`: variables, columns, 32-bit integer literals, NULL, `+ - * / %`, negation, comparisons, `AND OR NOT`, `IS [NOT] NULL`, `coalesce`, `CASE WHEN`) or uninterpreted operators; (C4) `tryCatch`, `raise`, `reraise`, `commit` and the error kind `user`; what a routine's statements can do (raise, violate a constraint, fail after a write, reach a handler) |
| `Sqldiff/Core/Semantics.lean` | Values (NULL, three-valued booleans, 32-bit integers, opaque values), errors (overflow, division by zero, constraint violations), the meaning of slice expressions, the database (tables as lists of rows, observed as bags), the data statements, the fuel-based, executable interpreter, what a caller observes (out parameters, returned value, world and database after a finished call; error kind and calls for a failed one; decision 1) and `Refines` |
| `Sqldiff/Core/Data.lean` | The data statements: congruence lemmas (`insertRows_map`, `updateRows_map`, `deleteRows_map`, and (C3b) `keptRows_map`, `pickRows_map`) and statement atomicity (`exec_error_atomic`) |
| `Sqldiff/Core/Rename.lean` | An injective renaming of a routine's variables is observationally the same routine (`refines_rename`) |
| `Sqldiff/Core/Fuel.lean` | Once a run finishes (with a result or an error), more fuel gives the same observation (`obs_mono`) |
| `Sqldiff/Core/OpFree.lean` | A routine without uninterpreted operators means the same whatever they mean (`Routine.obs_opFree`) |
| `Sqldiff/Core/Handlers.lean` | (C4) When T-SQL `TRY`/`CATCH` and a PL/pgSQL exception block mean the same: a body that cannot fail after a write (`Stmt.undoSafe`, `exec_undoSafe`), settling `undo` where it changes no outcome (`exec_settle`), and where it changes only the database of a failed call (`exec_settleTop`, `Routine.obs_settleTop`, `refines_settleTop_rename`) |
| `Sqldiff/Mirror/Lower.lean` | `lowerT` and `lowerP`: the T-SQL and PL/pgSQL spec of the C1 constructs, the C2a slice, the C3a data statements and the C3b queries (each rule with its vendor citation and probe status), as lowerings of the generated `Sqldiff.Ast.Routine` into the core; the trusted inputs (`Deployment`, `Catalog` with its tables); partial (`none` is "outside the spec"); `sliceT`, `sliceP`, `abortsT`: whether a routine is in the C2a slice |
| `Sqldiff/Mirror/Check.lean` | The mirror check with an untrusted certificate (the variable renaming), `check_sound`, `check_sound_c2a` and the residual assumptions of a C2a answer |
| `Sqldiff/Mirror/Request.lean` | The fields of a `sqldiff-check/3` mirror request and their decoding (C3a: table mappings in DEPLOYMENT, table entries in CATALOG); the extra answer fields (`scalars`, `residual`, `statements`, and (C4) `errors`) |
| `Sqldiff/Mirror/Examples.lean`, `Sqldiff/Mirror/ExamplesC2b.lean`, `Sqldiff/Mirror/ExamplesC3a.lean`, `Sqldiff/Mirror/ExamplesC3b.lean`, `Sqldiff/Mirror/ExamplesC4.lean` | Confirmed and refused pairs and interpreter runs (C1 and C2a; C2b strings; C3a with table states; C3b queries; C4 handlers, raised errors and transactions, with the runs that show why a refused pair differs), as `#guard`s checked on every build |
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

**Parameter and return types** (checker 0.8.0). The lowered routines
carry no types, so the check also compares the declared type of each
parameter, position by position, and the return types
(`Sqldiff.Mirror.sigCorr`, "Parameter and return types" in
`Sqldiff/Mirror/Lower.lean`): T-SQL `int`/`integer` with PostgreSQL
`integer`/`int`/`int4`, a C2b string type with a C2b string type (any
lengths: PostgreSQL drops a parameter's and a result's length), and on
the C1 path only `bigint` and `smallint` with themselves; any other pair
is answered "the parameter or return types do not correspond". A
procedure's trailing `INOUT return_code` must be an integer. A parameter
default must be NULL or a slice value that fits the declared type (T-SQL
truncates a default, PostgreSQL, having dropped the length, does not),
else the routine is outside the spec.

**String stores, on every path** (checker 0.9.0). A store into a target
that may hold a string (`SET`/`:=`, a `DECLARE` default, a function's
`RETURN`, an `OUTPUT` or `INOUT` copy-back) is in the spec only when no
conversion can change the value: the value is NULL; or the target is an
integer or a certainly non-character type; or the target is a string type
without a length that takes every value (PostgreSQL `text`/`varchar`,
T-SQL `nvarchar(max)`); or the value is a slice string whose bound fits
the target's length (and, into a T-SQL `varchar`, is not `wide`). This
holds on the C1 path too, where a stored string used to be lowered as it
is although T-SQL truncates and PostgreSQL raises. PostgreSQL parameters
and function results keep no length (probed on PostgreSQL 18.6), so they
count as unbounded targets there; locals keep theirs. A copy-back, whose
value comes from the callee, needs an unbounded or non-string target.
`char(n)`, `sysname`, `varchar(max)` from a value outside the slice, and
user or alias types take only NULL.

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
and T-SQL routines whose expressions can raise, or that call a
procedure (whose own errors the core does not model, so a call counts as
able to fail, checker 0.8.0), but that do not start with
`SET XACT_ABORT ON` (T-SQL goes on after a failed statement; PostgreSQL
stops). For those the core still evaluates the slice parts exactly, but
the answer also rests on the C1 caveat that runs raise no error.

### Phase C2b: strings

The core has string values (`Val.str`, a sequence of characters; the
empty string is not NULL) and five slice operators: `str`, `concat`,
`rtrim` (trailing U+0020 only), `streq` (exact, three-valued) and `len`
(characters). The rules, each with its vendor citation and probe status,
are in `Sqldiff/Mirror/Lower.lean`, "The C2b slice: strings". **Every
vendor quote there was written from memory** (the vendor sites were not
reachable when it was written) and needs checking against the page.

| Rule | Probed |
|---|---|
| Types: T-SQL `varchar(n)`/`nvarchar(n)`, `n ≤ 4000`; PostgreSQL `varchar(n)`, `varchar`, `text`. `max`, `char(n)`, no-length T-SQL `varchar` are outside | eval/casts (truncation by `CAST`) |
| PostgreSQL parameters carry no length (read unbounded; writes still held to the declared length) | not probed |
| Assignment, declaration defaults, parameter defaults, function results: only when the value provably fits (T-SQL truncates, PostgreSQL raises); never `nvarchar` into `varchar` | eval/casts/assign (T-SQL `abc`, PostgreSQL error) |
| Literals: printable ASCII, no backslash (`'..'` and `N'..'` are one `Str` in the AST) | eval/casts (literal types) |
| T-SQL `+` / PostgreSQL `||` on two strings, NULL-propagating; every T-SQL string bound ≤ 4000 | not probed |
| `=`/`<>` only with the CATALOG fact `!binary-collation`: T-SQL `a = b` is `rtrim(a) = rtrim(b)` exactly (padding), PostgreSQL `a = b` is exact | not probed |
| `RTRIM`/`rtrim`; T-SQL `LEN(x)` is PostgreSQL `length(rtrim(x))` | not probed |
| `COALESCE`, `CASE` over strings; T-SQL `ISNULL(x, y)` is `COALESCE` only when `x` is a variable and `y` fits its declared length | not probed |
| Strings passed to calls and `OUTPUT` arguments into string variables are outside the slice | n/a |

New residuals in a C2a answer: `ASM-CONCAT-NULL` (concatenation, unless
the T-SQL routine starts with `SET CONCAT_NULL_YIELDS_NULL ON`),
`ASM-BINARY-COLLATION` (a string comparison, under the fact the request
asserted), `ASM-TSQL-LEN-BMP` (`LEN`: no character outside the BMP, or an
SC collation), `ASM-TSQL-STRING-ARGS` (a T-SQL string parameter: callers
pass values that fit it). The answer format is unchanged (`"scalars":
"c2a"`, the same theorems); the checker reports `0.6.0`.

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
and the congruence lemmas, and (C2b) `mkRtrim_eval` (`Axioms.lean`).

### Phase C3b: single-table queries

Checker 0.10.0. The core has one more statement, `query t c pick`
(`Sqldiff.Core.Stmt.query`): the rows of table `t` where `c` is TRUE
(`keptRows`), then `pick` assigns variables from them (`pickRows`):
`row orNull sets` from the last kept row (nothing, or with `orNull` NULL,
when none is kept), `count x` their number (an overflow beyond 2^31-1),
`any x` whether there is one. It reads the database and writes only
variables. The lowerings build it from exactly the shapes
`sqldiff convert --to postgres` writes ("The C3b slice: queries" in
`Sqldiff/Mirror/Lower.lean`, each rule with its claim, vendor text and
probe):

| Rule | T-SQL | PL/pgSQL | Probed (eval/queries) |
|---|---|---|---|
| 1 | `SELECT @x = e, ... FROM t WHERE c` | `DECLARE r record; BEGIN FOR r IN SELECT e AS a, ... FROM t WHERE c LOOP x := r.a; ... END LOOP; END;` | Q1 (one row, NULL column, no row keeps the value, two targets, key NULL), Q2 (several rows: an unspecified one) |
| 2 | `SET @x = (SELECT e FROM t WHERE c)` | `x := (SELECT e FROM t WHERE c);` | Q5 (value, NULL on no row, 512 against 21000 on several) |
| 3 | `IF [NOT] EXISTS (SELECT 1 FROM t [WHERE c])` | the same | Q4 (TRUE/FALSE, never UNKNOWN), Q7 (the select list is not evaluated, both engines) |
| 4 | `SELECT @x = COUNT(*) FROM t [WHERE c]`, `SET @x = (SELECT COUNT(*) ...)` | `SELECT COUNT(*) INTO x FROM t [WHERE c];`, `x := (SELECT COUNT(*) ...)` | Q6 (0, 1, 3; `int` against `bigint`); overflow not probed |
| Row counts | none of these sends one | none | Q3 |
| Names | columns spelt as the catalog | a column that is also a variable (or the loop's record) is outside the spec | Q9 (42702 in a `FOR` query and in `EXISTS`) |
| Own writes | a read sees the routine's earlier writes | the same | Q8 |

The conditions that make rules 1 and 2 hold although neither vendor says
which row a multi-row result assigns (rule 1) and both raise on it (rule
2): the `WHERE` is a conjunction that fixes every primary-key column with
`=` against a value that reads no column (`keyFixed`), so at most one row
can match in any state an engine can be in (the key is in the trusted
catalog, `ASM-TABLES-AS-DECLARED`; the core's choice on a state that
breaks the key is not a claim about the engines); the values read no
variable and cannot raise (`pickValue`: T-SQL may evaluate them on rows
the `WHERE` drops, and reads variables per row); the targets are
distinct `int` variables the `WHERE` does not read. The PL/pgSQL block
is accepted only as the converter writes it: its one declaration a
`record` without default, named like no variable of the routine, the
loop its only statement, the loop's body exactly `xi := r.ai` in the
order of the select list's aliases. `EXISTS` is the whole condition of
an `IF`, its test held in a variable no dialect can name
(`Sqldiff.Mirror.existsVar`, `"%exists"`), a local of both lowered
routines that the certificate need not list. Queries stand only in
procedures; `WHILE EXISTS`, `EXISTS` inside a larger condition, joins,
aliases, hints, `TOP`, `ORDER BY`, `GROUP BY` and `DISTINCT` are outside
the spec, as is PL/pgSQL `SELECT col INTO x` (NULL on no row, probed
Q10).

**What it rests on**, beyond C3a: the probes above and the vendor texts
quoted from memory in `Lower.lean`; `ASM-TABLES-AS-DECLARED` for the read
tables too (the Rust side now sends a plain table a routine only reads,
`cert.rs`, `read_tables`); `ASM-TSQL-ANSI-NULLS` when a `WHERE` uses `=`;
`ASM-TSQL-ARITH-ERRORS` when the routine counts rows; decision 1's serial
execution (other sessions' writes are not modelled). No new residual id.
A confirmed answer whose routines read a table carries `"statements":
"c3b"`.

**Proofs.** `check_sound` and `check_sound_c2a` keep their statements;
the core lemmas gained the `query` case: the renaming simulation
(`pickRows_rename`, `keptRows_map`), operator independence
(`pickRows_map`), fuel monotonicity, statement atomicity of leaves, and
the C4 settling. One C4 fact needed a correction for reads:
`exec_setDb` (a statement that does not touch the database runs the
same from any database) now also asks for no query, and a handler with a
query is no longer a `failHandler` (it would read the database `undo`
restores). Standard axioms only (`Axioms.lean`).

### Phase C4: errors and transactions

The core has handled errors and transaction control
(`Sqldiff.Core.Stmt`): `tryCatch undo body handler` runs the handler
when the body fails, with the error bound to its `reraise`s, from the
state the error left, except that with `undo` the database is put back
as the body found it; `raise k` fails with `k` (`ErrKind.user` for the
routine's own errors, a new kind and class); `reraise`; and `commit`,
which does nothing a finished call shows. T-SQL `TRY`/`CATCH` lowers to
`tryCatch false`, PL/pgSQL `BEGIN ... EXCEPTION WHEN OTHERS THEN ... END`
to `tryCatch true`: the one thing they differ in is PostgreSQL's
rollback of the block (V18) against T-SQL keeping what the body wrote.

**The check settles that difference instead of assuming it away**
(`Sqldiff/Core/Handlers.lean`). It compares the PL/pgSQL routine with
`(lowerT a).settleTop`, which sets `undo` on a T-SQL handler

1. whose body cannot fail after it may have written (`undoSafe`: after a
   data statement or call, only statements that cannot fail in any run of
   the core): then the error leaves the database the body began with, and
   `exec_settle` proves the outcomes equal, state for state; or
2. that always fails and touches no database (`THROW;`, `THROW n, m, s`,
   possibly after statements that neither write nor return), outside
   every other handled body: then the error leaves the routine and the
   two runs differ only in the database of a failed call, which `Obs`
   does not carry (`exec_settleTop`, `ASM-STATE-AFTER-ERROR` in the
   residual).

A T-SQL `TRY` whose body can fail after a write and whose handler goes on
is left without `undo` and refused against an exception block: the
interpreter shows the two databases differ (`ExamplesC4`, "The rollback
difference"). `check_sound` and `check_sound_c2a` keep their statements;
their proofs go through `refines_settleTop_rename`.

The rules (`Sqldiff/Mirror/Lower.lean`, "The C4 rules"; V10, V17 and V18
are the checked vendor facts of `docs/vendor-facts-verification.md`,
everything else is quoted **from memory** and must be checked against the
page; **nothing is probed**: `eval/` has no run of any of it):

| Rule | Vendor text | Probed |
|---|---|---|
| `TRY`/`CATCH` against `EXCEPTION WHEN OTHERS` (one handler, condition `OTHERS`, block without `DECLARE`); the handler sees the variables as the error left them | "TRY...CATCH (Transact-SQL)" (from memory); PL/pgSQL "Trapping Errors" (V18) | no |
| Both catch every error of the core (severity 14/16 in T-SQL; `OTHERS` misses only `query_canceled`/`assert_failure`) | "TRY...CATCH" (from memory), V10, V18 | no |
| PostgreSQL rolls back the block's writes; T-SQL keeps them (settled by the check, above) | V18; "TRY...CATCH", "SET XACT_ABORT" (V10) | no |
| An error in a handler leaves it; handlers nest | both pages (from memory) | no |
| `THROW n, 'm', s` (`n` 50000..2147483647, `s` 0..255, literals) and `RAISE [EXCEPTION] 'f'` (no `%`) / `USING MESSAGE = 'm' [, ERRCODE = 'P0001']` (exactly that spelling: `'p0001'` raises 42704) raise `user` | "THROW (Transact-SQL)", PL/pgSQL "Errors and Messages" (from memory) | no |
| `THROW;` and `RAISE;` re-raise the handled error, only directly in a handler | same pages (from memory) | no |
| `RAISERROR`, `RAISE NOTICE`, other `ERRCODE`s, `ERROR_NUMBER()` and the other error functions, `XACT_STATE()`, `@@ERROR`, `SQLSTATE`, `SQLERRM`, condition-specific handlers: outside | V10 (RAISERROR ignores XACT_ABORT) | n/a |
| No call inside a handled body (a callee's errors are not in the core, its writes would differ) | n/a | n/a |
| T-SQL `BEGIN TRAN ... COMMIT` as a bracket in the routine's own list (or in the idiom's body), under `SET XACT_ABORT ON`, nothing between them that returns, calls, handles or controls transactions; lowers to nothing and `commit` | "BEGIN/COMMIT TRANSACTION" (from memory), V10 | no |
| The rollback-and-rethrow idiom (`IF @@TRANCOUNT > 0 ROLLBACK; THROW;`, or `ROLLBACK; THROW;` around a body that is exactly one bracket), outside other handlers and outside `IF`/`WHILE`/nested blocks, lowers to its body (0.8.0: with two brackets an error between them finds no transaction, the `ROLLBACK` raises 3903, probed) | "ROLLBACK TRANSACTION", "THROW" (from memory) | no |
| PL/pgSQL `COMMIT` only in a procedure without `SECURITY DEFINER`, outside exception blocks | V17 | no |
| `TRY`, `THROW`, transactions only in T-SQL procedures | "CREATE FUNCTION (Transact-SQL)" (from memory) | n/a |

**Residuals** added to a C2a answer: `ASM-CATCH-CORE-ERRORS` (the routine
has a handler: inside a handled block no error is raised that the core
does not model, or such an error is caught alike), `ASM-NO-CALLER-TXN`
(an error can reach a handler, or there is transaction control: no
transaction is open when the routine is called, `IMPLICIT_TRANSACTIONS
OFF`, and the PostgreSQL `CALL` is top-level outside a transaction
block), `ASM-THROW-ERRCODE` (the routine raises its own error: callers do
not tell user errors apart by number). `ASM-STATE-AFTER-ERROR` is now
decided on the settled routine, so a handled body that cannot fail after
a write no longer names it. A confirmed answer whose routines use C4
carries `"errors":"c4"` after `"statements"`; the checker reports
`0.7.0`. The abstract syntax gained `Node.Try`, `Throw`, `Raise`, `Txn`,
`Handler` and `RaiseOpt` (`crates/sqldiff-syntax/src/routine.rs`), so the
schema hash changed.

**Trust statement.** A confirmation of a routine with C4 constructs rests
on the C4 rules above (trusted spec, unprobed, mostly quoted from
memory), on the residuals, and on the earlier slices' facts. Not covered:
`RAISERROR`, savepoints, `ROLLBACK` outside the idiom, nested
transactions, `XACT_STATE()`/`@@TRANCOUNT` reads, calls in handled
blocks, condition-specific handlers, error messages and numbers, the
state after an error that leaves the routine, and explicit transactions
opened by the caller.
