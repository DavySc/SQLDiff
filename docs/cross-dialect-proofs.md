# Cross-dialect proofs of routines

How `sqldiff diff` (and `tree`, `pairs`, `queue`) decides that a routine in one
dialect behaves like its counterpart in another, and what an "equal under
assumptions" relies on. Statement-level comparison itself is described in
`crates/sqldiff-core/src/equiv`; this page covers the routine around the
statements and the names inside them.

## Routine options

Every option between the parameter list and the body is classified
(`equiv/options.rs`). An option the engine does not know leaves the routine
uncovered, so the verdict stays unknown; nothing is skipped silently.

| Option | Treatment |
|---|---|
| T-SQL `EXECUTE AS CALLER`/`OWNER`, PostgreSQL `SECURITY INVOKER`/`DEFINER`, Db2 for i `SET OPTION USRPRF = *USER`/`*OWNER` | Compared as the security context (below) |
| T-SQL `EXECUTE AS SELF` or `'user'`, `NATIVE_COMPILATION`, `FOR REPLICATION`, `INLINE = ON`, `RETURNS NULL ON NULL INPUT`; PostgreSQL `STRICT`, `PARALLEL SAFE`/`RESTRICTED`, `LEAKPROOF`, `SET x FROM CURRENT`, `SET` of a setting that can change results (`TimeZone`, `DateStyle`, `lock_timeout`, …), languages other than SQL and PL/pgSQL; Db2 for i `COMMIT ON RETURN YES`, `NEW SAVEPOINT LEVEL`, `DYNAMIC RESULT SETS n > 0`, `CONCURRENT ACCESS RESOLUTION`, `SET OPTION` entries other than those below | Not covered: unknown |
| T-SQL `RECOMPILE`, `ENCRYPTION`, `SCHEMABINDING`, `VIEW_METADATA`, `INLINE = OFF`; PostgreSQL `COST`, `ROWS`, `VOLATILE`, `PARALLEL UNSAFE`, `SET` of planner, memory and logging settings; Db2 for i `SPECIFIC`, `FENCED`, `PROGRAM TYPE`, `NOT DETERMINISTIC`, `SET OPTION DBGVIEW`/`DBGENCKEY`/`OUTPUT`/`TGTRLS`/`COMPILEOPT`/`DLYPRP`, `SRTSEQ = *HEX` | No effect on results: listed as a normalization |
| PostgreSQL `STABLE`/`IMMUTABLE`, Db2 for i `DETERMINISTIC` | `ASM-ROUTINE-LABEL`; a labelled routine that writes is not covered |
| PostgreSQL `SET search_path` | `ASM-DEFAULT-SCHEMA` when `public` is the first schema (after `pg_catalog` and `"$user"`); any other path is not covered |
| Db2 for i `SET OPTION COMMIT` | Read by the transaction model (`ir::interp`, `ASM-DB2-COMMIT-LEVEL`) |
| PostgreSQL procedure with `SECURITY DEFINER` or a `SET` clause that runs `COMMIT`/`ROLLBACK` | Not covered: PostgreSQL raises invalid transaction termination (fact V17) |

## Security context

Each routine runs its statements with the caller's privileges or the owner's
(fact V25). Routines that touch no table need nothing. Otherwise:

| A | B | Verdict contribution |
|---|---|---|
| caller | caller | `ASM-TSQL-OWNERSHIP-CHAIN` when one side is T-SQL (ownership chaining skips checks the other side makes) |
| owner | owner | `ASM-OWNER-PRIVILEGES` |
| owner | caller (PostgreSQL or Db2) | **different** (`equiv.security-context`): a caller holding only EXECUTE succeeds on one side and fails on the other |
| owner | T-SQL caller | unknown: ownership chains decide which calls fail |
| Db2 without `USRPRF` | caller | `ASM-DB2-USRPRF-USER` |
| Db2 without `USRPRF` | owner | unknown |

The converter keeps the context: `EXECUTE AS OWNER` becomes `SECURITY DEFINER`
(or `USRPRF = *OWNER, DYNUSRPRF = *OWNER` on Db2 for i), except in a
PostgreSQL procedure that commits, where it needs a decision. Db2 for i output
always states `USRPRF`.

## Names

Table names keep their schema. Unqualified names, T-SQL `dbo` and PostgreSQL
`public` are the default schema, and any other schema must match after
folding: `Sales.Orders` never matches `Archive.Orders` or plain `Orders`, in
one dialect or across two.

Across dialects, two assumptions are stated whenever a compared statement
names a table:

- `ASM-IDENT-FOLD`: unquoted names name the same objects after case folding
  (PostgreSQL and Db2 for i objects created from unquoted names, T-SQL with a
  case-insensitive catalog collation). Quoted names whose case matters never
  match an unquoted spelling.
- `ASM-DEFAULT-SCHEMA`: when a default schema is used, it is the same schema
  on both sides.

Within one dialect, `ASM-DEFAULT-SCHEMA` is stated only when one version
writes `dbo.t` and the other `t`.

So a cross-dialect pair that touches tables is at best "equal under
assumptions", never plain "equal".

## Files with several objects

Sources list one object per CREATE when a file holds several (schema dumps,
migration scripts), with ids `path#name` (`~2`, `~3` for repeated names such as
overloads). An object keeps the statements that name it (`CREATE INDEX … ON`,
`ALTER TABLE`, `COMMENT ON`, `GRANT … ON`); other statements go with the next
object, or the last one at the end of the file. Its text is the whole file
with the other objects blanked, so line numbers are those of the file.
Same-named objects of one dialect (PostgreSQL triggers named per table) are
reported as ambiguous and not paired.

When only one side splits a file (A's `s.sql` holds `t` and `u`, B's `s.sql`
only `t`), B's whole file pairs with `s.sql#t` (pairing rule `split-object`)
under three conditions. First, the two files would pair by key unsplit.
Second, B's file defines exactly one table, view or routine, and it has the
same kind, name and schema as the object (`dbo`, `public` and no schema
count as the same). Third, each is the only candidate for the other. `u`
stays "only in A". Anything else stays unpaired and is reported as such.

## Tables

A script's top-level `CREATE TABLE` statements, and those beside a file's one
routine, are compared table against table, in order (`equiv/table.rs`). They
must sit in the same place: after as many data statements on each side, or on
the same side of the routine. Only T-SQL against PostgreSQL is compared; Db2
for i column types have no verified facts yet, so a Db2 for i table is not
compared (not proven).

Each statement is read completely. A clause the model does not know leaves the
table not compared: a computed or generated column, a column collation, a
temporary, unlogged or partitioned table, `IF NOT EXISTS`, `PERIOD FOR
SYSTEM_TIME` and system versioning, an inline `INDEX`, `NOT FOR REPLICATION`,
`IGNORE_DUP_KEY = ON`, `MEMORY_OPTIMIZED`, a partition scheme, a quoted
PostgreSQL name with capitals (other code names it unquoted), and every type
not listed below. Clauses that only shape storage are passed over and listed
as normalizations: filegroups (`ON [PRIMARY]`, `TEXTIMAGE_ON`), compression,
`CLUSTERED`/`NONCLUSTERED`, key sort orders, index fill options,
`ROWGUIDCOL`, PostgreSQL `WITH (fillfactor …)` and `TABLESPACE`.

| What | Equal when | Otherwise |
|---|---|---|
| Name | Same schema and name after folding; `dbo`, `public` and no schema are the default schema | different (`equiv.table-name`) |
| Columns | Same number, same names, same order (`SELECT *`, `INSERT` without a column list) | different (`equiv.table-columns`) |
| Type | Same stored values, below | different (`equiv.column-type`), or equal under `ASM-COLUMN-RANGE` when PostgreSQL accepts more |
| NULL | Same nullability; a T-SQL column that says nothing is nullable under `ASM-TSQL-ANSI-NULL-DEFAULT`; key and identity columns are NOT NULL on both | different (`equiv.column-null`) |
| Identity | Same first value and step (PostgreSQL without `START WITH` starts at 1, or -1 going down), under `ASM-IDENTITY-VALUES`; `GENERATED BY DEFAULT` accepts more | different (`equiv.identity`) |
| Default | Both or neither; compared by the engine as `SELECT default`; bit `0`/`1` against `FALSE`/`TRUE`; `NEXT VALUE FOR s` against `nextval('s')` by sequence name | different (`equiv.column-default`), or not proven |
| Primary key, unique | Same column sets; same names when either side names them; T-SQL's unique constraint holds one NULL, so a nullable column needs `UNIQUE NULLS NOT DISTINCT`; a string key column differs in case and trailing spaces (`ASM-TSQL-CI-COLLATION`) | `equiv.table-key`, `equiv.constraint-name`, `equiv.unique-nulls`, `equiv.case-sensitivity`, `equiv.trailing-spaces` |
| Foreign key | Same columns, referenced table and columns, `ON DELETE`/`ON UPDATE` actions (both default to NO ACTION; PostgreSQL `MATCH SIMPLE` is T-SQL's rule); a string column matches case-insensitively in T-SQL | `equiv.foreign-key`, `equiv.foreign-key-action`, `equiv.case-sensitivity` |
| Check | As many on each side, paired by name, otherwise in order; compared by the engine as `SELECT 1 FROM t WHERE NOT (predicate)`, the rows the check rejects | `equiv.check`, or the engine's own difference, or not proven |

Types (SQL Server 2022, PostgreSQL 18):

| T-SQL | PostgreSQL | Verdict |
|---|---|---|
| `bigint`, `int`, `smallint` | the same width | equal |
| narrower integer, `tinyint` | wider integer | `ASM-COLUMN-RANGE` (tinyint is 0 to 255) |
| `bit` | `boolean` | `ASM-BIT-BOOLEAN-COLUMN`; statements that use the column are compared on their own |
| `decimal(p, s)` (default 18, 0) | `numeric(p, s)` | equal; more integer digits: `ASM-COLUMN-RANGE`; fewer, another scale, or unconstrained `numeric`: different |
| `money`, `smallmoney` | `numeric(19, 4)`, `numeric(10, 4)` or wider | `ASM-COLUMN-RANGE`, `ASM-TSQL-MONEY-ROUNDING` |
| `real` / `float(1–24)`, `float` / `float(25–53)` | `real`, `double precision` | `ASM-COLUMN-RANGE` (PostgreSQL also stores NaN and infinity); real against double: different |
| `char(n)`, `varchar(n)` | `char(n)`, `varchar(n)` | `ASM-TSQL-CODE-PAGE`; longer target: `ASM-COLUMN-RANGE`; shorter: different |
| `nchar(n)`, `nvarchar(n)` | `char(n)`, `varchar(n)` | `ASM-COLUMN-RANGE` (nvarchar counts UTF-16 code units, so a character outside the Basic Multilingual Plane counts twice) |
| `varchar(max)`, `nvarchar(max)` | `text` or `varchar` | `ASM-VALUE-SIZE-1GB` (PostgreSQL's limit for one field); with a length: different |
| fixed against varying | | different (blank padding) |
| `date` | `date` | `ASM-COLUMN-RANGE` |
| `time(p)`, `datetime2(p)` (default 7) | `time(p)`, `timestamp(p)` (default and at most 6) | same precision: `ASM-COLUMN-RANGE`; another precision, so `datetime2` (7 digits) against `timestamp`: different |
| `datetime`, `smalldatetime` | `timestamp` | different (they round to 1/300 s and to the minute) |
| `datetimeoffset` | `timestamptz` | different (the offset is not kept) |
| `uniqueidentifier` | `uuid` | equal |
| `binary(n)` | `bytea` | different (zero padding); `varbinary(n)`: `ASM-COLUMN-RANGE`; `varbinary(max)`: `ASM-VALUE-SIZE-1GB` |

Facts: SQL Server data types (`decimal and numeric`, `float and real`,
`char and varchar`, `nchar and nvarchar`, `datetime2`, `datetime`,
`smalldatetime`, `datetimeoffset`, `time`, `binary and varbinary`, `IDENTITY
(Property)`, `UNIQUE constraints`, `SET ANSI_NULL_DFLT_ON`) and PostgreSQL 18
chapter 8 (data types), `CREATE TABLE` (identity columns, `NULLS NOT
DISTINCT`, referential actions, `MATCH SIMPLE`) and appendix K (limits:
1 GB per field).

## Indexes, sequences and schemas

A script's top-level `CREATE INDEX`, `CREATE SEQUENCE` and `CREATE SCHEMA`
statements, and those beside a file's one routine, are compared statement
against statement (`equiv/ddl.rs`). They are paired by kind and name (an index
also by its table, since T-SQL index names are per table); a statement whose
name has no counterpart is paired in order with the one other unnamed statement
of its kind (and table), so a rename is a difference. Each must come after as
many data statements on both sides, and an index after its table when the file
creates the table. T-SQL is compared against PostgreSQL and against Db2 for i.

Each statement is read completely. What the model does not read leaves the
statement not compared: columnstore, full-text, XML and spatial indexes,
expression keys, a key collation or operator class, PostgreSQL `USING` other
than btree, `CONCURRENTLY`, `IF NOT EXISTS`, `ON ONLY`, an unnamed index,
`IGNORE_DUP_KEY = ON`, `DROP_EXISTING = ON`, `FILESTREAM_ON`, `CREATE OR
REPLACE`, a sequence of a type other than smallint, int, bigint and tinyint,
`RESTART`, PostgreSQL `OWNED BY`, schema elements inside `CREATE SCHEMA`, and
Db2 for i `UNIQUE WHERE NOT NULL`, `WHERE` and `INCLUDE`. Clauses that shape
only storage or how the index is built are listed as normalizations: `CLUSTERED`
/ `NONCLUSTERED` (with `ASM-CLUSTERED` for a clustered one), filegroups and
partition schemes (`ON …`), `WITH (FILLFACTOR, PAD_INDEX, ONLINE, MAXDOP,
SORT_IN_TEMPDB, DATA_COMPRESSION, …)`, PostgreSQL `WITH (fillfactor,
deduplicate_items)`, `TABLESPACE` and `USING btree`.

What an index changes in results is only which rows a unique index rejects.
Column order, sort directions, included columns and a plain index's filter
shape how fast statements run: when they differ the pair is not proven (the
engine does not judge speed), never equal and never different.

| What | Equal when | Otherwise |
|---|---|---|
| Table and name | Same table; same index name after folding (`ASM-INDEX-NAME-SCHEMA`: on the target, index names are schema objects) | `equiv.index-table`, `equiv.index-name` (DROP INDEX, ALTER INDEX and hints name it) |
| Name clash | No two indexes, or an index and a table, of one name in one schema of the file on the target side | `equiv.index-name-clash` (the second CREATE fails: relation already exists) |
| Unique | Both or neither | `equiv.index-unique` |
| Unique key | Same column set (order is shape) | `equiv.index-key` |
| Shape | Same key columns in order, same ASC/DESC, same included columns | not proven |
| NULL position | Passed over: T-SQL sorts NULLs first, PostgreSQL and Db2 for i last in ascending keys; ORDER BY statements are compared on their own | |
| Filter | Neither, or both, compared by the engine as `SELECT 1 FROM t WHERE filter`, the rows the index covers; a T-SQL filter adds `ASM-TSQL-FILTERED-INDEX-SET-OPTIONS` | one side only: not proven; different rows: the engine's difference for a unique index, not proven for a plain one |
| NULLs in a unique key | T-SQL holds one row with NULL per key; PostgreSQL needs `NULLS NOT DISTINCT`, unless the column is NOT NULL on a side or a filter's `c IS NOT NULL` leaves NULLs out | `equiv.unique-nulls` |
| String unique key | T-SQL compares case-insensitively and ignores trailing spaces (`ASM-TSQL-CI-COLLATION`) | `equiv.case-sensitivity`, `equiv.trailing-spaces` (fixed-length PostgreSQL char ignores trailing spaces too) |
| Db2 for i unique | Not compared: NULL handling, case and trailing blanks are probes `V29-UNIQUE-*` | not proven |

Key column facts come from a `CREATE TABLE` of the same file, else the catalog
(only when no other schema has a table of that name, and the column states no
collation); otherwise a unique index is not proven.

Sequences are compared by the values they hand out, with every default filled
in:

| | T-SQL (SQL Server 2022) | PostgreSQL 18 | Db2 for i |
|---|---|---|---|
| Type | bigint | bigint | not relied on: every value must be written (fact V29) |
| Bounds going up | the type's range | 1 to the type's maximum | |
| Bounds going down | the type's range | the type's minimum to -1 | |
| Start | the minimum going up, the maximum going down | the same | |
| Cycle | NO CYCLE; restarts at the minimum going up | the same | |

A different start, step, bound or cycling is `equiv.sequence` with the first
value that differs. A T-SQL tinyint sequence against a smallint one with the
same bounds is equal under `ASM-SEQUENCE-TYPE`; other types are not proven. The
cache is passed over: values are handed out in call order on both sides, and
values a crash or rollback skips are gaps on both. A PostgreSQL `CACHE` above 1
is per session, so values are no longer in call order across sessions: not
proven. A Db2 for i sequence must say `NO CACHE`.

Schemas: same name after folding (`equiv.schema-name`), and the same owner, or
T-SQL `dbo` against no `AUTHORIZATION` (`ASM-SCHEMA-OWNER`); other owners are
not proven. A Db2 for i schema name longer than 10 characters gets another,
generated system name for its library: not proven.

The converter writes what the proof needs: `NULLS NOT DISTINCT` on a
PostgreSQL unique index, `WITH (…)` and filegroups after a filter dropped
(PostgreSQL rejects `WITH` after `WHERE`), partition schemes dropped,
columnstore, XML and spatial indexes not converted, `AUTHORIZATION dbo` dropped
under `ASM-SCHEMA-OWNER`, and `ASM-INDEX-NAME-SCHEMA` on every index.

Facts checked on SQL Server 2022 and PostgreSQL 18 (2026-10-26): a T-SQL
unique index rejects a second NULL, `('x', NULL)` twice, `'A'` after `'a'` and
`'a '` after `'a'`; a filtered unique index on `IS NOT NULL` holds any number
of NULLs; an INSERT under `SET ANSI_NULLS OFF` into a table with a filtered
index fails (Msg 1934); two tables may each have an index `IX_1`. PostgreSQL
accepts two NULLs in a unique index, rejects them with `NULLS NOT DISTINCT`
(also for `('x', NULL)`), accepts `'A'` and `'a '` next to `'a'`, rejects an
index named like another index or a table of the schema, rejects `WITH` after
`WHERE`, and has no role `dbo`. Sequence defaults as in the table, read from
`sys.sequences` and `pg_sequences`; both restart a cycling sequence at its
minimum.

## Witnesses for changed statements

When two paired statements lower to terms that differ in a way no rule
explains (a changed comparison operator, `AND` against `OR`, a dropped or
changed WHERE, another parameter, literal or column, a changed SET or INSERT
value), `equiv::xwit` looks for a witness on the canonical terms: one table
of one or two rows plus parameter values, under which both statements finish
and their results differ. A witness makes the pair "different" (rule
`equiv.witness`) and says what happens, for example: "When parameter 1
(cityid / p_city_id) is 0 and application.cities holds the row (cityid = 0),
side A (T-SQL) deletes 1 row and side B (PostgreSQL) deletes no row."

The terms are dialect-neutral, so one evaluator serves every pair of
dialects. It only evaluates what means the same everywhere for the values it
uses: integers within the parameter and catalog types (no decimals), NULL,
short ASCII words (no blanks, so case-insensitive and blank-padded
comparisons agree with the others), and opaque values that are only copied.
Results it compares: the rows a DELETE or UPDATE leaves, the row an INSERT
adds, the rows a query returns (TOP / LIMIT only with fewer qualifying rows),
a returned value (a function's looked-up value, single row or none), and an
OUT parameter that nothing reads or sets later. Anything else (joins,
subqueries next to a table, grouping, string ordering, casts to non-numbers,
local variables, temporary tables) stays "unknown". Two-row worlds differ in
every column and hold no NULL in the first row, so no unique key over those
columns is broken; worlds without NULL columns are tried first.

A witness is only searched for the one statement pair of a routine that is
not proven equal, with every statement lowered and no control flow outside
the comparison, and only when no other statement uses its table or assigns
a parameter it reads. Two changed statements can compensate each other
(`x := p + 1; x := x * 2` against `x := p + 2; x := (x - 1) * 2`), and
another statement can make the witness rows unreachable or undo them.

Every witness that reads or writes a table states `ASM-WITNESS-DATA`: the
columns hold the values it gives them, and no constraint or trigger rejects
or replaces the write (column types, constraints and triggers are not known
from the routine). Parameters are matched by position, so a witness is only
searched when both routines are the same kind with the same number of
parameters. Unless each side is that one statement with nothing around it
(no transaction control), the witness also states
`ASM-WITNESS-PATH`: the inputs reach the statement and its result is not
undone or overwritten later.

A search finds nothing for rewrites that keep the meaning (`x > p` against
`x >= p + 1` on integers, `x = p OR x <> p` against `x IS NOT NULL`), so they
stay "unknown"; the search never makes a verdict "equal".

## Benchmark

`eval/cross-dialect/` in the project files: `bench.py` converts the
WideWorldImporters procedures (Website, WebApi, Application, Integration) to
PostgreSQL and diffs each pair; `mutate.py` applies behaviour-changing edits to
the pairs proven equal. On 2026-10-24: 15 of 15 fully converted procedures
equal under `ASM-OWNER-PRIVILEGES` and `ASM-IDENT-FOLD` (0 of 15 before), and
255 mutations with no false equal.

On 2026-10-26 (`eval/cross-dialect-witness/`): the same 255 mutations give 120
"different" (45 before) and still no false equal; every WHERE operator,
dropped WHERE, parameter, literal and column mutation is now "different". The
token-aware `xwmut.py` over every converted WWI routine (PostgreSQL and Db2
for i) turns all 60 behaviour-changing mutants of the equal pairs into
"different" (none before), with no false equal and no witness on a
meaning-preserving edit.

