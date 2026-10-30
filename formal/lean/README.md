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
| `Sqldiff/Core/Syntax.lean` | The core language of phases C1, C2a, C3a and C4 ([verified-conversion.md](../../docs/verified-conversion.md), decision 9): parameters with modes and defaults, locals, assignment, `if`, `while`, `return`, sequences, calls, and the data statements `insert`, `update`, `delete` on a table definition (`TableDef`); (C3b) the read `query t c pick` (`Pick`: assign from the one kept row, count the kept rows, test for one); expressions are slice expressions (`SExpr`: variables, columns, integer literals, NULL, `+ - * / %` and negation at a width (C2c: 16, 32 or 64 bits), comparisons, `AND OR NOT`, `IS [NOT] NULL`, `coalesce`, `CASE WHEN`; (C2c) bit, date and timestamp literals, `DATEADD` by day, month or year, `DATEDIFF` by day, the date of a timestamp, a date at midnight) or uninterpreted operators; (C4) `tryCatch`, `raise`, `reraise`, `commit` and the error kind `user`; what a routine's statements can do (raise, violate a constraint, fail after a write, reach a handler) |
| `Sqldiff/Core/Calendar.lean` | (C2c) The proleptic Gregorian calendar of the core's dates (days since 0001-01-01) and timestamps (microseconds): day numbers from and to civil dates, leap years, the month and year addition with the month-end clamp, checked against the vendors' own counts |
| `Sqldiff/Core/Semantics.lean` | Values (NULL, three-valued booleans, 32-bit integers, opaque values), errors (overflow, division by zero, constraint violations), the meaning of slice expressions, the database (tables as lists of rows, observed as bags), the data statements, the fuel-based, executable interpreter, what a caller observes (out parameters, returned value, world and database after a finished call; error kind and calls for a failed one; decision 1) and `Refines` |
| `Sqldiff/Core/Data.lean` | The data statements: congruence lemmas (`insertRows_map`, `updateRows_map`, `deleteRows_map`, and (C3b) `keptRows_map`, `pickRows_map`) and statement atomicity (`exec_error_atomic`) |
| `Sqldiff/Core/Rename.lean` | An injective renaming of a routine's variables is observationally the same routine (`refines_rename`) |
| `Sqldiff/Core/Fuel.lean` | Once a run finishes (with a result or an error), more fuel gives the same observation (`obs_mono`) |
| `Sqldiff/Core/OpFree.lean` | A routine without uninterpreted operators means the same whatever they mean (`Routine.obs_opFree`) |
| `Sqldiff/Core/Handlers.lean` | (C4) When T-SQL `TRY`/`CATCH` and a PL/pgSQL exception block mean the same: a body that cannot fail after a write (`Stmt.undoSafe`, `exec_undoSafe`), settling `undo` where it changes no outcome (`exec_settle`), and where it changes only the database of a failed call (`exec_settleTop`, `Routine.obs_settleTop`, `refines_settleTop_rename`) |
| `Sqldiff/Mirror/Lower.lean` | `lowerT` and `lowerP`: the T-SQL and PL/pgSQL spec of the C1 constructs, the C2a slice, the C2b strings, the C2c typed values (integer widths, `bit` against `boolean`, `date` and `datetime2(p)` against `date` and `timestamp(p)`), the C3a data statements, the C3b queries and the C3c tables (each rule with its vendor citation and probe status), as lowerings of the generated `Sqldiff.Ast.Routine` into the core; the trusted inputs (`Deployment`, `Catalog` with its tables); partial (`none` is "outside the spec"); `sliceT`, `sliceP`, `abortsT`: whether a routine is in the C2a slice |
| `Sqldiff/Mirror/Check.lean` | The mirror check with an untrusted certificate (the variable renaming), `check_sound`, `check_sound_c2a` and the residual assumptions of a C2a answer; (0.13.0) `why`, the reason codes of the routine the check refuses |
| `Sqldiff/Mirror/Diagnose.lean` | (0.13.0) Untrusted refusal diagnostics: `whyT` and `whyP` name the constructs `lowerT`/`lowerP` refuse, as the codes of "Refusal diagnostics" below, using the lowering functions as oracles node by node; no theorem depends on it |
| `Sqldiff/Mirror/Request.lean` | The fields of a `sqldiff-check/3` mirror request and their decoding (C3a: table mappings in DEPLOYMENT, table entries in CATALOG; C3c: string column types and defaults in a table entry; C2c: the typed-value column types `j`, `h`, `b`, `d`, `t<p>` and their defaults); the extra answer fields (`scalars`, `residual`, `statements`, and (C4) `errors`) |
| `Sqldiff/Mirror/Examples.lean`, `Sqldiff/Mirror/ExamplesC2b.lean`, `Sqldiff/Mirror/ExamplesC2c.lean`, `Sqldiff/Mirror/ExamplesC3a.lean`, `Sqldiff/Mirror/ExamplesC3b.lean`, `Sqldiff/Mirror/ExamplesC3c.lean`, `Sqldiff/Mirror/ExamplesC4.lean`, `Sqldiff/Mirror/ExamplesDiag.lean` | Confirmed and refused pairs and interpreter runs (C1 and C2a; C2b strings; C2c typed values, with the runs of the date arithmetic and the width overflows; C3a with table states; C3b queries; C3c string columns, defaults; C4 handlers, raised errors and transactions, with the runs that show why a refused pair differs; the reason codes of refused routines, one or more per family), as `#guard`s checked on every build |
| `Main.lean` | The executable: one tab-separated request per line on stdin (`sqldiff-check/2`, with the schema hash; `sqldiff-check/3` for the mirror check, with the abstract syntax schema hash), one JSON answer per line on stdout (the answer format is documented there; an unconfirmed `/3` answer whose routine is outside the spec carries `why`, the reason codes). A request whose schema hash is not the checker's, or whose trees or routines do not decode, is unconfirmed. The checker reports version 0.15.0 |
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
lengths: PostgreSQL drops a parameter's and a result's length), and
(C2c) `bigint` and `smallint` with themselves, `bit` with `boolean`,
`date`, and `datetime2(6)` with `timestamp(6)` or `timestamp`; any
other pair is answered "the parameter or return types do not
correspond". A
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
other type (`decimal`, `datetime`, `time`, `tinyint`, ...; strings are
C2b, integer widths, `bit`, `date` and `datetime2` are C2c), casts and
implicit conversions other than C2c's `CAST(... AS date)`, function
calls other than `COALESCE`/`ISNULL` and C2c's `DATEADD`/`DATEDIFF`,
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

### Phase C2c: typed values beyond `int` and strings

Checker 0.14.0. The core has integer widths (`Sqldiff.Core.IW`: 16, 32
and 64 bits; `arith` and `neg` carry the width of their result and
overflow at it), bit/boolean literals (`SExpr.bool`, the same value a
comparison gives), dates and timestamps (`Val.date`: days since
0001-01-01 of the proleptic Gregorian calendar, `Val.ts`: microseconds
since 0001-01-01T00:00:00; `Sqldiff/Core/Calendar.lean`), comparisons
of two dates or two timestamps, `dateAdd` by day, month or year (the
month-end clamp, the time of day kept), `dateDiff` by day (day boundaries
crossed), `toDate` and `toTs`. The rules, each with its vendor citation
(**all quoted from memory**) and its probe, are in "The C2c slice: typed
values" of `Sqldiff/Mirror/Lower.lean`; `eval/values/{mssql,postgres}.sql`
with their `.out` files (V1 to V8) is the probe, run on SQL Server 2022
and PostgreSQL 18 on 2026-10-30. The forms are exactly the ones `sqldiff
convert --to postgres` writes (`tsql.bit.compare`, `tsql.bit.store`,
`tsql.dateadd`, `tsql.datediff`).

| Rule | Claim | Probed (eval/values) |
|---|---|---|
| Integer widths | T-SQL `smallint`/`int`/`bigint` against `smallint`/`int2`, `integer`/`int`/`int4`, `bigint`/`int8`; `+ - * /` at the wider operand's width, overflow raises (8115 against 22003); two `smallint`s give a `smallint` on both; `smallint / smallint` is a `smallint` division and raises on `-32768 / -1` on both (8115 against 22003); a literal divisor is an `int`, so `@s / -1` gives 32768 on both; `%` keeps its literal-divisor rule (`-32768 % -1` on `smallint`s is 8115 against 0); comparisons across widths | V1 |
| Integer literals | PostgreSQL types a constant beyond the `int` range `bigint`; T-SQL types it `numeric(p,0)`, so it is in the slice only as the whole value stored into a `bigint` or compared with one (an exact conversion), never as an arithmetic operand | V2 |
| Stores between widths | widening keeps the value; a narrowing store is in the spec only when the value provably fits (a literal in range, or a value of the target's width or narrower), on every path (`int` 40000 into `smallint` is 220 against 22003); `COUNT(*)` into an `int` variable only: into a `bigint` T-SQL still raises 8115 beyond 2^31-1 rows (its `COUNT` is `int`) and PostgreSQL's `bigint` count does not; a `bit`, date or timestamp stored into an integer is outside (T-SQL stores a bit as 0/1, PL/pgSQL raises 22P02) | V2, V3 |
| `bit` against `boolean` | stores from the literals `0`/`1` against `FALSE`/`TRUE`, NULL or another bit; the comparisons `@b = 1`, `= 0`, `<> 1`, `<> 0` against `b = TRUE`, `= FALSE`, `<> TRUE`, `<> FALSE` (either operand order) lower to the variable or its `NOT`, three-valued alike on both; `IS [NOT] NULL`; any other store (`2`, an int, a string) or comparison is outside; never a key column | V3 |
| Literals | `'YYYY-MM-DD'` where a date is read, `'YYYY-MM-DDThh:mm:ss[.f]'` with at most `p` digits where a `datetime2(p)`/`timestamp(p)` is read, in a store, a comparison, a parameter default, a column default, or under `CAST(... AS date \| datetime2(p) \| timestamp(p))`: the ISO 8601 forms, which neither `DATEFORMAT`/`LANGUAGE` nor `DateStyle` affect; more fractional digits than kept are outside (both engines round, not alike at a half: `.1234565` is `.123457` against `.123456`); every other form is outside, as a store too | V4 |
| Comparisons and stores | `= <> < <= > >=` of two dates or two timestamps; a date against a timestamp at midnight (`toTs`); a date into a `date`, a timestamp of at most `p` digits into a `datetime2(p)`, a date into a timestamp (midnight); a timestamp into a date (both truncate) and into a narrower precision (both round) are outside; `CAST(x AS date)` of a timestamp drops the time | V4, V5 |
| `DATEADD` | `day \| month \| year` (and their abbreviations) with an `int` number on a date or `datetime2(p)` against the converter's `(d + n)`, `(d - m)`, `(d + make_interval(months \| years => n))::date` and `(t + make_interval(days \| months \| years => n))`: days added, months and years moved with the day clamped to the result month's last day, the time kept; the result is not held to a range (T-SQL raises 517 past 9999-12-31 where PostgreSQL returns the value: `ASM-DATEADD-RANGE`); years name `ASM-DATEADD-YEAR-END` (Microsoft's page states the clamp for months); other dateparts, a `bigint` number, a string date are outside | V6 |
| `DATEDIFF` | `day` only, against `((b)::date - (a)::date)`: the day number of `b`'s date minus `a`'s, an `int`; other dateparts outside | V7 |
| Columns | `bigint`, `smallint`, `bit`/`boolean`, `date`, `datetime2(p)`/`timestamp(p)` columns with constant defaults in the forms above (SPEC `j`, `h`, `b`, `d`, `t<p>`; defaults `b0`/`b1`, `d<hex>`, `t<hex>`); a date or timestamp key column; the Rust side sends them when both DDLs show them (`plain_table::ColType`, `cert.rs::pair_type`) | V8 |
| Signatures | `bigint` and `smallint` with themselves, `bit` with `boolean`, `date`, `datetime2(6)` with `timestamp(6)` or `timestamp` (PostgreSQL drops a parameter's and a result's precision, so a `datetime2(p < 6)` parameter is outside); a string type with a PostgreSQL string type without a length or of the same length (re-review of C2c: a caller's argument is held to the length in the PostgreSQL DDL, which must then be the one T-SQL truncates to) | not probed |
| Calls | every argument has exactly its callee's parameter type, which the request names (ROUTINE `NAME:MODES:TYPES`, `Catalog.routineTys`, from the PostgreSQL DDL): a value of that slice type (or NULL; the T-SQL literals a `bit` or date parameter reads), an `OUTPUT`/`INOUT` variable declared with it; a callee whose types the request does not give takes no argument. A string argument must provably fit the parameter's declared length in T-SQL (a literal's length, a variable's declared length) and come from no `nvarchar`; a parameter without a length (`text`), or an argument without a known bound, is outside. T-SQL truncates a longer argument on entry and PostgreSQL, having dropped the length, keeps it: `dbo.q @s varchar(3)` given a `varchar(10)` holding `abcdef` sees `abc` against `abcdef` (re-review of C2c, through the converter; this was a false "equal"). Before C2c an argument was not compared with the callee, which was sound only while confirmed callees were `int` routines: an `int` into a `smallint` or `boolean` parameter finds no procedure in PostgreSQL (42883), a `smallint` variable in an `INOUT integer` position is not writable, where T-SQL converts | review of C2c, on PostgreSQL 18 |
| Variable types | the declared types of the variables the certificate pairs correspond (`localTyCorr`: the same slice type, any string lengths), and nothing but NULL, or a string into a string type such as `varchar(max)`, is stored into a variable of a type outside the slice: a `datetime2(6)` value through T-SQL `datetime` and PostgreSQL `timestamp` locals is `.123333` against `.123456` (review of C2c). A type outside the slice corresponds to nothing, not even the type of the same name: T-SQL `time` is `time(7)` and PostgreSQL's `time(6)`, so `'12:00:00.1234567' < '12:00:00.1234568'` is 1 against 0, and at one precision they round differently at a half (`time(6)`); T-SQL `numeric` is `numeric(18,0)` and rounds `1.5` to 2; `money` keeps four decimals (re-review of C2c, the first through the converter). The list of non-slice pairs that correspond is empty: a pair joins it only with a probe showing every store the lowering admits identical. Refused on the C1 path too | review and re-review of C2c |

Outside, by decision: `datetime2(7)` and `datetime2` without a precision
(no PostgreSQL mirror), `datetime`, `smalldatetime`, `datetimeoffset`,
`time`, `tinyint`, `decimal` (a later part); reads of the clock
(`SYSDATETIME()`, `GETDATE()`, `now()`, `clock_timestamp()`), `DATEPART`,
`EOMONTH`, formatting and string conversions; `COUNT(*)` into a
`bigint` (see the table); `bit` in arithmetic,
`COALESCE`/`ISNULL` with a number, or against another bit; `@s + 1` stored
back into a `smallint` (the literal is an `int`, so the sum is, and the
store narrows). A variable of one of these types is outside on every
path, the C1 path included (re-review of C2c; see "Variable types").

**What it rests on**, beyond C2a and C2b: the probes above and the vendor
texts quoted from memory in `Lower.lean`; `ASM-DATEADD-RANGE` and
`ASM-DATEADD-YEAR-END` in `residual` (`Check.lean`); values a caller
passes that both engines can hold (a T-SQL caller's dates are within
0001-01-01 and 9999-12-31, a PostgreSQL caller's timestamps have at most
six digits), which is the argument correspondence of decision 1, and
`ASM-TSQL-STRING-ARGS`, whose statement now says that every argument is
a value of its parameter's declared type and which a routine with a
parameter of any slice type names (a `datetime2(7)` value is rounded on
entry, an `int` narrowed or made 0/1, a `1.5` passed to an `int` or
`bigint` truncated to 1 where PostgreSQL finds no procedure); for columns, `ASM-TABLES-AS-DECLARED` as in C3c. A pair whose
variables' declared types differ is answered "the declared types of the
variables do not correspond". No new residual id and no new answer
field; the request's ROUTINE entries gain the callees' parameter types; `check_sound` and `check_sound_c2a` keep their
statements, the core lemmas (the renaming simulation, "cannot fail")
gained the new cases, and `Axioms.lean` is unchanged (standard axioms
only). `ExamplesC2c` has the confirmed pair (eval/values' procedure as
the converter writes it), the interpreter runs of the date arithmetic and
the width overflows, and the refused forms; `test_gate.py` cases 104 to
112 exercise the protocol.

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
  matches), `ASM-STATE-AFTER-ERROR`, and (checker 0.15.0)
  `ASM-TSQL-ROWCOUNT-0`: callers run the routine with `SET ROWCOUNT 0`,
  the default. A caller's `SET ROWCOUNT n` is inherited by the routine
  and limits the rows its `INSERT`, `UPDATE`, `DELETE` and `SELECT`
  statements touch or read, and PostgreSQL has no such setting
  (`eval/session`, R1 to R4, SQL Server 2022 and PostgreSQL 18.6: under a
  caller's `SET ROWCOUNT 1` an `UPDATE` and a `DELETE` of three rows
  touch one and a three-row `INSERT ... VALUES` inserts one; the
  converted procedure touches all three). Every routine with a data
  statement or a query names it (`Sqldiff.Mirror.residual`), and the
  Rust side adds it to every confirmation whose `statements` is `c3a` or
  `c3b` (also on the C1 path and from an older checker). It is not
  discharged by the routine's text: the reader keeps only `ON`/`OFF`
  options as `Setting`s, so a routine that begins with `SET ROWCOUNT 0`
  is outside the spec;
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
an `IF`, its test held in a hidden variable (`Sqldiff.Mirror.existsVar`,
`%exists`), a local of both lowered routines that the certificate need
not list; a routine that names it too is refused (duplicate local, or a
renaming that is not injective). Queries stand only in
procedures; `WHILE EXISTS`, `EXISTS` inside a larger condition, joins,
aliases, hints, `TOP`, `ORDER BY`, `GROUP BY` and `DISTINCT` are outside
the spec, as is PL/pgSQL `SELECT col INTO x` (NULL on no row, probed
Q10).

**What it rests on**, beyond C3a: the probes above and the vendor texts
quoted from memory in `Lower.lean`; `ASM-TABLES-AS-DECLARED` for the read
tables too (the Rust side now sends a plain table a routine only reads,
`cert.rs`, `read_tables`); `ASM-TSQL-ANSI-NULLS` when a `WHERE` uses `=`;
`ASM-TSQL-ARITH-ERRORS` when the routine counts rows; decision 1's serial
execution (other sessions' writes are not modelled). No new residual id
in C3b; since checker 0.15.0 a routine that reads names
`ASM-TSQL-ROWCOUNT-0` (see C3a). The read shapes return at most one row
each and showed no change under a caller's `SET ROWCOUNT 1`
(`eval/session` R3), where a pick over several rows did; they name it all
the same, since the setting applies to every `SELECT`.
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

### Phase C3c: wider plain tables

Checker 0.11.0. The core is unchanged: a catalog table (`CatTable`) may
now have string columns and constant defaults, and the Rust side lets a
non-unique index through. "The C3c tables" in `Sqldiff/Mirror/Lower.lean`
has every rule with its claim, vendor text (**all quoted from memory**)
and probe; `eval/tables/{mssql,postgres}.sql` with their `.out` files
(T1 to T9) is the probe, run on SQL Server 2022 and PostgreSQL 18.6.

| Rule | Claim | Probed (eval/tables) |
|---|---|---|
| String columns | T-SQL `varchar(n)`/`nvarchar(n)`, `1 ≤ n ≤ 4000`, against PostgreSQL `varchar(n)` with the same `n` (SPEC `v<n>`/`w<n>`); a value that fits is stored and read back as it is, trailing spaces included | T1 |
| Reads | a column read has the column's bound (`wide` for `nvarchar`); every C2b rule applies to it as to a string variable (`=`/`<>` only under the collation fact, which now covers columns: a column without `COLLATE` has the database's collation) | T1, T5 |
| Writes | `INSERT`/`UPDATE` store into a string column only NULL or a string of the slice that fits (`colFits`); SQL Server raises 2628 under `ANSI_WARNINGS ON` and truncates under `OFF`, PostgreSQL raises 22001, both truncate excess spaces | T2 |
| Picks | C3b `SELECT @x = col` and `SET @x = (SELECT col ...)` may read a string column into a string variable it fits (`pickStoresOk`, `forStoresOk`); SQL Server truncates, PostgreSQL raises 22001 | T9 |
| `ANSI_PADDING` | a `varchar` column created under `OFF` trims trailing spaces on writes (`nvarchar` does not); the catalog asserts `ON`, Rust names `ASM-TSQL-ANSI-PADDING` for a routine that writes a table with a `varchar` column and keeps out every such table of a T-SQL tree that turns it off | T3 |
| String keys | outside: SQL Server pads key values (`'a'` and `'a '` collide, 2627, even under BIN2), PostgreSQL does not | T6 |
| Defaults | an integer literal for an `int` column or a slice string literal that fits for a string column, the same in both DDLs (a fourth column field, `i<int>`/`s<hex>`); both lowerings add the left-out columns with a default to the `INSERT` (`defaultsFor`); expressions, functions, sequences, a default on one side only, and different defaults stay outside | T4 (also: a default too long fails the `INSERT`, 2628 against 22001) |
| Indexes | a non-unique, unfiltered index without options on plain columns changes no result unless an entry exceeds the engine's limit (SQL Server 1700 bytes nonclustered, 900 clustered; PostgreSQL btree about 2704 bytes, `INCLUDE` counted); the Rust side checks declared sizes (`plain_table::PlainIndex::fits`) and the catalog asserts it | T7, T8 |

PostgreSQL parameters still carry no length (C2b), so a string parameter
cannot be written to a `varchar(n)` column; a local declared with a
length can. **What it rests on**, beyond C3b: `ASM-TABLES-AS-DECLARED`
(now also: the column types, defaults, indexes and column collations are
as the DDL declares them), `ASM-TSQL-ANSI-PADDING` (Rust-side, for a
routine that writes a `varchar` column), and the C2b residuals the
routine's expressions need. No new answer field or Lean residual; the
proofs are untouched (`check_sound` and `check_sound_c2a` keep their
statements; `Axioms.lean` unchanged). `ExamplesC3c` has the confirmed and
refused pairs and interpreter runs.

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
| T-SQL `BEGIN TRAN ... COMMIT` as a bracket in the routine's own list (or in the idiom's body), under `SET XACT_ABORT ON`, nothing between them that returns, calls, handles or controls transactions; lowers to nothing (checker 0.12.0; before, `COMMIT` lowered to `commit`) | "BEGIN/COMMIT TRANSACTION" (from memory), V10 | no |
| The rollback-and-rethrow idiom (`IF @@TRANCOUNT > 0 ROLLBACK; THROW;`, or `ROLLBACK; THROW;` around a body that is exactly one bracket), outside other handlers and outside `IF`/`WHILE`/nested blocks, lowers to its body (0.8.0: with two brackets an error between them finds no transaction, the `ROLLBACK` raises 3903, probed) | "ROLLBACK TRANSACTION", "THROW" (from memory) | no |
| PL/pgSQL `COMMIT` only in a procedure without `SECURITY DEFINER`, outside exception blocks; lowers to nothing (checker 0.12.0), wherever it stands there: after a transaction or after each statement T-SQL commits on its own (`tsql.txn.autocommit`); a confirmation names `ASM-NO-CALLER-TXN` from the `COMMIT`s (`pgTxn`) | V17 | no |
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

### Refusal diagnostics (checker 0.13.0)

`lowerT` and `lowerP` are partial and say only "outside the spec". So
that a corpus can be read for what keeps it out, an unconfirmed `/3`
answer whose reason is "the T-SQL routine is outside the C1 spec" or
"the PL/pgSQL routine is outside the C1 spec" carries one more field
after `reason`, `"why":["code", ...]` (`Sqldiff.Mirror.why` in
`Check.lean`, `Sqldiff/Mirror/Diagnose.lean`): the constructs the
lowering refused in that routine (the T-SQL one when both are refused),
as codes `family:detail`, ASCII and lower-case over `[a-z0-9:_/.-]`,
distinct, in the order the constructs stand, at most 24, never empty. A
user or type name in a detail is sanitised to that set (`sanitise`:
`tyName`'s `decimal(18,2)` is `decimal_18_2`, `nvarchar(max)` is
`nvarchar_max`, `@@ROWCOUNT` is `rowcount`); `<type>` is the declared
type as `tyName` spells it, or `untyped`. No other answer carries the
field; every reason text, every other field and the request format are
unchanged.

**Untrusted.** The diagnostic is computed after the answer is decided,
only when a lowering has refused, by asking the lowering functions
themselves node by node (`lowerParam`, `txnBracket`, `storesStmt`,
`lowerNode`, `lowerStmt`, `lowerExpr`, `typed`, ...) and classifying each
refused node by its abstract-syntax shape; it recurses into the children
the lowering refuses, so a failing inner node is named and its parent
only when every child lowers. It changes no answer, `check` is untouched,
no theorem mentions it, and `Axioms.lean` is unchanged. Where no code of
the table fits, the lower-cased constructor name is the detail
(`stmt:sselect`, `node:if`, `expr:cast`) so that nothing is silent, and a
routine refused without any stage naming a reason gets
`routine:unclassified`.

| Code | Meaning |
|---|---|
| `routine:function` | A procedure-only shape in a function: a query (`SELECT @x = ...`, `IF EXISTS`, a scalar subquery, the `FOR` block) |
| `routine:block-decls` | The T-SQL body block has a `DECLARE` section (the parser never writes one) |
| `routine:scope:undeclared` | A variable read or written that is neither a parameter nor a declared local (`wellScoped`) |
| `routine:scope:duplicate` | A name declared twice, or a local with a parameter's name, or the `FOR` block's record named like a variable |
| `routine:names:case` | Two T-SQL names equal up to ASCII case (`@a`, `@A`: one variable or two depends on the collation) |
| `routine:names:non-ascii` | A variable name with a character outside ASCII |
| `routine:param:<type>` | A parameter of a type no value of the slice fits, with a default (`routine:param:decimal_18_2`) |
| `type:<type>` | (0.14.0) A variable read where the lowering needs a value of the slice, declared with a type the slice has no values for (`type:datetime2_7`, `type:datetime`, `type:timestamptz`, `type:nvarchar_max`) |
| `routine:param:default` | A parameter default that is not NULL or a slice value fitting the declared type, or that reads a variable |
| `routine:unclassified` | The lowering refused the routine and no stage named a reason (should not happen; it keeps the field non-empty) |
| `stmt:select:result-set` | A `SELECT` that assigns no variable (T-SQL: no `@x =`; PL/pgSQL: no `INTO`): a result set |
| `stmt:select:from:join`, `:multi`, `:sub`, `:values`, `:func`, `:hints`, `:alias`, `:data-change`, `:none` | The `FROM`: a join, several sources, a derived table, `VALUES`, a table function, table hints, a table alias, a DML source, no `FROM` |
| `stmt:select:cte`, `:setop`, `:distinct`, `:top`, `:group`, `:having`, `:order`, `:offset`, `:locking`, `:for-json` | A `WITH`, a set operation, `DISTINCT`, `TOP`, `GROUP BY`, `HAVING`, `ORDER BY`, `OFFSET`/`LIMIT`, a locking clause, `FOR JSON` |
| `stmt:select:into` | An `INTO` the rules refuse: T-SQL `SELECT ... INTO`, PL/pgSQL `SELECT col INTO x` (NULL on no row, probed Q10), `INTO STRICT`, several targets |
| `stmt:select:agg:<fn>` | An aggregate in a select list outside the C3b `COUNT(*)` shape (`stmt:select:agg:sum`; `:count` for a `COUNT(*)` beside other items) |
| `stmt:select:value` | A picked value that reads a variable (`SELECT @x = @x + c`, a running total), can raise, or is a condition |
| `stmt:select:item`, `stmt:select:items` | A select item with both an alias and an assignment, or an unaliased item in a `FOR` block; a select list of the wrong length for the shape |
| `stmt:select:where:not-key` | The `WHERE` does not fix every primary-key column with `=` (which row T-SQL keeps is unspecified) |
| `stmt:select:table:unknown` | The table is not in the deployment's table map or the target catalog |
| `stmt:select:table:not-plain` | The table is in the catalog but not usable (`CatTable.wellFormed`: a string key, a default that does not fit, ...) |
| `stmt:select:target:<type>` | A pick into a variable whose declared type does not take the value (`stmt:select:target:varchar_10` for an `int` column) |
| `stmt:insert:select`, `:default`, `:returning`, `:conflict` | `INSERT ... SELECT`, `DEFAULT VALUES`, `RETURNING`/`OUTPUT` (where the parser keeps it), `ON CONFLICT` |
| `stmt:insert:cols` | No column list, an unknown or repeated column, no row, or a row of the wrong length |
| `stmt:insert:table:unknown`, `:not-plain` | As for `SELECT` |
| `stmt:insert:value` | A value the slice cannot type for the column, or that may not fit it (a string into `int`, 21 characters into `varchar(20)`) |
| `stmt:update:from`, `:alias`, `:key`, `:multi-set`, `:cols`, `:value`, `:table:unknown`, `:table:not-plain` | `UPDATE ... FROM`, a table alias, a key column set, `SET (a, b) = ...`, no or an unknown column, a value as for `INSERT`, the table as for `SELECT` |
| `stmt:delete:from`, `:alias`, `:table:unknown`, `:table:not-plain` | A second `FROM`/`USING`, a table alias, the table as for `SELECT` |
| `stmt:merge` | `MERGE` |
| `stmt:message` | `PRINT`/`RAISE NOTICE`-like messages |
| `stmt:declare:pg` | A `DECLARE` in a PL/pgSQL body (declarations belong to the `DECLARE` section) |
| `stmt:assign:target` | An assignment or `OUTPUT` target that is no variable (`@@x`, a qualified name) |
| `stmt:return:pg-proc` | `RETURN e` in a PL/pgSQL procedure |
| `stmt:dml:function` | A data statement in a function |
| `stmt:dml:rowcounts` | A T-SQL data statement while the deployment observes row counts |
| `stmt:<ctor>` | Another refused statement, by its constructor (`stmt:sselect`, `stmt:assign`, ...) |
| `node:for-query` | A `FOR` loop over a query outside the converter's block, or that block with another shape (record, aliases, body) |
| `node:block-decls` | A nested block with declarations (other than the converter's `FOR` block) |
| `node:call:handled`, `:args`, `:ret`, `:name` | A call inside a handled body; a named argument, too many arguments, or one that does not lower; `EXEC @rc = p`; a callee the deployment or the catalog does not name |
| `node:call:type` | (0.14.0) An argument that does not have exactly its callee's parameter type, or a callee whose parameter types the request does not give |
| `node:setting:<name>` | A `SET` the rules do not know, or off (`node:setting:ansi_nulls`, `:xact_abort` for `OFF`, `:nocount` while row counts are observed) |
| `node:try:handlers` | A `TRY` with handlers other than one `CATCH` / one `WHEN OTHERS` |
| `node:try:function` | `TRY`/`CATCH` in a T-SQL function |
| `node:try:idiom` | The rollback-and-rethrow idiom where it may not stand (inside `IF`, `WHILE`, a block or another handler); its body is diagnosed where it stands |
| `node:throw:args`, `:function`, `:place` | `THROW n, m, s` outside the literal forms; `THROW` in a function; a bare `THROW;` or `RAISE;` outside a handler |
| `node:raise:level`, `:format`, `:opts` | `RAISE` with a level other than `EXCEPTION`; a format with `%`, arguments, or nothing to say; options other than `MESSAGE` and `ERRCODE = 'P0001'` |
| `node:txn:no-xact-abort` | A transaction statement in a T-SQL routine that does not start with `SET XACT_ABORT ON` |
| `node:txn:bracket` | `BEGIN TRAN`/`COMMIT` that do not form a bracket: one left open, or a `RETURN`, call, `TRY` or transaction statement between them |
| `node:txn:nested` | A `BEGIN TRAN` inside an open one |
| `node:txn:<op>` | A transaction statement the rules refuse by its op or place: `node:txn:begin`/`:commit` outside a bracket (or, PL/pgSQL, where `COMMIT` may not stand: a function, `SECURITY DEFINER`, a handled block), `:rollback` outside the idiom, `:save`, `:named` for a named transaction |
| `node:txn:function` | A transaction statement in a function |
| `node:<ctor>` | Another refused node, by its constructor (`node:if`, `node:nblock`, `node:raise` for a `RAISE` in T-SQL, ...) |
| `expr:subquery`, `expr:exists`, `expr:in-query` | A subquery, `EXISTS` or `IN (SELECT ...)` inside an expression (not one of the whole-statement C3b shapes) |
| `expr:star` | `*` as an expression |
| `expr:name:qualified` | A two-part name (`t.c`) |
| `expr:name:unresolved` | A bare name that is no column of the statement's table (T-SQL), or a PL/pgSQL name that is both a column of the target and a variable, or the table's own name (an undeclared PL/pgSQL name is `routine:scope:undeclared`) |
| `expr:var:global:<name>` | A T-SQL `@@x` read (`expr:var:global:rowcount`, `:trancount`, `:error`) |
| `expr:pg-var` | A `@x` variable in PL/pgSQL |
| `expr:fn:error:<name>` | An error function (`error_number`, `error_message`, ..., `xact_state`) or PL/pgSQL `sqlstate`/`sqlerrm` |
| `expr:fn:distinct`, `:order`, `:over`, `:star`, `:within-group` | A call with `DISTINCT`, `ORDER BY`, `OVER`, `*`, `WITHIN GROUP` |
| `expr:fn:<name>` | A function outside the slice where the rules need a slice value (`expr:fn:upper`, `expr:fn:isnull` over strings that may truncate) |
| `expr:date-literal` | (0.14.0) A string literal where a date or timestamp is read that is not `'YYYY-MM-DD'` or `'YYYY-MM-DDThh:mm:ss[.f]'`, or has more fractional digits than the type keeps |
| `expr:bit-literal` | (0.14.0) A value stored into a `bit`/`boolean` that is not `0`/`1` (T-SQL) or `TRUE`/`FALSE` (PostgreSQL), NULL or another bit |
| `expr:dateadd:part`, `expr:datediff:part` | (0.14.0) `DATEADD` with a datepart other than `day`, `month`, `year` (and their abbreviations); `DATEDIFF` with one other than `day` |
| `expr:<ctor>` | Another expression outside the slice where one is needed, by its constructor (`expr:cast`, `expr:bin`, `expr:name` for a variable of another type, ...) |
| `store:assign:<type>`, `store:declare:<type>`, `store:return:<type>` | A store that could change its value ("String stores, on every path"; (0.14.0) a narrowing of an integer width, a bit or a date from a literal of the wrong form, a timestamp into a date): `SET`/`:=`, a `DECLARE` default, a function's `RETURN`, with the target's declared type (`store:assign:varchar_20`, `store:assign:smallint`, `store:declare:untyped`); the value's form follows when it is outside the slice |
| `store:call` | An `OUTPUT`/`OUT`/`INOUT` copy-back into a variable that cannot take every value |
| `store:pick:<type>`, `store:for:<type>` | A C3b pick (`SELECT @x = col`, a scalar subquery, `SELECT ... INTO`) or a `FOR` block assignment into a variable the value may not fit |

`test_gate.py` asserts the field on every outside-spec request and its
absence on every other answer; `ExamplesDiag.lean` has one or more
`#guard`s per family.
