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
| Default | Both or neither; compared by the engine as `SELECT default` only when storing it converts nothing (NULL, a number the column's integer, decimal or money type holds exactly, an ASCII string no longer than a string column, an argument-free call such as `SYSDATETIME()` or `NEWID()` on a date, time or uuid column), since the column stores the default converted to its type and the dialects convert differently (`int DEFAULT 1.5`: T-SQL stores 1, PostgreSQL 2); bit `0`/`1` against `FALSE`/`TRUE`; `NEXT VALUE FOR s` against `nextval('s')` by sequence name | different (`equiv.column-default`), or not proven |
| Primary key, unique | Same column sets; same names when either side names them; T-SQL's unique constraint holds one NULL, so a nullable column needs `UNIQUE NULLS NOT DISTINCT`; a string key column differs in case and trailing spaces (`ASM-TSQL-CI-COLLATION`); in a conversion check it is equal under `ASM-CI-COLLATION` instead (see below) | `equiv.table-key`, `equiv.constraint-name`, `equiv.unique-nulls`, `equiv.case-sensitivity`, `equiv.trailing-spaces` |
| Foreign key | Same columns, referenced table and columns, `ON DELETE`/`ON UPDATE` actions (both default to NO ACTION; PostgreSQL `MATCH SIMPLE` is T-SQL's rule); a string column matches case-insensitively in T-SQL (equal under `ASM-CI-COLLATION` in a conversion check) | `equiv.foreign-key`, `equiv.foreign-key-action`, `equiv.case-sensitivity` |
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

## Calls and comments

T-SQL `EXEC` against PostgreSQL `CALL`, and T-SQL `sp_addextendedproperty`
against PostgreSQL `COMMENT ON` (`equiv/call.rs`). Each statement is read
completely or not compared. The two sides' calls and comments are paired in
order by what they call or comment on; one without a counterpart is not
compared. A call must sit in the same place among the data statements on both
sides. A comment reads and writes no data, so its place does not matter, as
long as only comments follow it on each side (nothing after it can create,
change or drop the object again). Db2 for i calls and comments are not
compared yet; the open questions are probes `V30-*` in
[probes/db2i](../probes/db2i/README.md).

**Calls.** The called procedure is looked up in both sides' catalogs (in a
tree conversion: the source tree and the converted tree). It must be found
once on each side (PostgreSQL overloads with the same number of parameters are
kept apart by their types), take as many parameters, and have the same modes:
T-SQL OUTPUT against PostgreSQL INOUT, input against IN. Arguments are bound to
parameters by position or by name on each side, an omitted one (or T-SQL
`DEFAULT`) takes the parameter's default from the catalog, and each parameter,
by position, must then receive the same value:

| Value | Same when | Otherwise |
|---|---|---|
| Literals | Converted to the parameter type as each side does (strings as typed literals for dates, times, numbers, uuid; bit `0`/`1` against `FALSE`/`TRUE`), and the results are equal | different (`equiv.call-argument`); not compared where the conversion is not modelled: a string longer than the T-SQL parameter (SQL Server truncates it), more decimals than its scale (SQL Server rounds), `char` parameters (SQL Server pads), an integer for a boolean (PostgreSQL finds no procedure) |
| Variables | The statement engine shows them to be the same variable, their declared types do not differ, and each passes to the parameter unchanged (T-SQL: no truncation, rounding or overflow; PostgreSQL: an implicit cast that keeps the value, no bigint into int); an OUTPUT variable must also take the result back unchanged, and on PostgreSQL be of the INOUT parameter's own type (length aside) | not compared |
| NULL | NULL on both sides | not compared |

Every proven call rests on `ASM-CALLEE-CONVERTED` (the procedure on side B is
the conversion of the one on side A; its own body is compared in the file that
creates it), `ASM-IDENT-FOLD`, and `ASM-DEFAULT-SCHEMA` for unqualified names.
A parameter whose PostgreSQL type has a length, precision or scale adds
`ASM-PG-PARAMETER-TYPMOD`: PostgreSQL ignores them in a parameter's type, so
nothing is truncated, rounded or padded there.

Not compared: the return code (`EXEC @rc = …`, not read as a call at all),
dynamic SQL, a procedure named by a variable, `WITH RECOMPILE` and other
`WITH` or `AT` options, system procedures (`sp_`, `xp_`), arguments T-SQL
does not accept (expressions; only constants, variables and `DEFAULT`),
PostgreSQL's `name := value` form, a positional argument after a named one, an
unknown parameter name, an OUTPUT result that T-SQL does not copy back (no
`OUTPUT` on the argument), PostgreSQL OUT parameters (they receive no value,
T-SQL's OUTPUT does), INOUT outside PL/pgSQL (the values come back as a row),
names with more than two parts.

A failed call is a failed statement: in a procedure without `SET XACT_ABORT
ON` or a rethrowing CATCH, T-SQL goes on with the next statement and
PostgreSQL stops (`equiv.error-continues`).

**Comments.** The extended property's levels name the object: a schema; a
table, view, procedure or function; a table's or view's column; a table's
index or constraint. The PostgreSQL comment must name the same object, which
must exist in both catalogs (an index or a key constraint on that table; check
and foreign key constraints are not recorded, so those are not compared). The
text must be equal; a T-SQL literal without `N` that holds non-ASCII
characters adds `ASM-TSQL-CODE-PAGE`. Every proven comment rests on
`ASM-EXTPROP-COMMENT`: the property name is dropped and an object has one
comment. Not compared: an object commented twice in one file (PostgreSQL keeps
the last comment; T-SQL keeps both properties or fails on the same name), a
value that is not a string literal, an escape string (`E'…'`) or a backslash
(read differently without `standard_conforming_strings`), `COMMENT ON … IS
NULL`, a quoted PostgreSQL name with capitals, other object kinds.

Facts (SQL Server 2022 and PostgreSQL 18, run 2026-10-26; scripts in the
project files, `eval/calls/2026-10-26/`): SQL Server passes 'abcdefgh' to an
`nvarchar(5)` parameter as 'abcde', 1.005 to `decimal(5,2)` as 1.01, 'ab' to
`char(5)` padded, 1.7 to `int` as 1, and fails a bigint beyond the int range;
an OUTPUT parameter receives the argument's value, and its result is truncated
into a shorter variable. PostgreSQL passes 'abcdefgh' whole to
`varchar(5)`, 1.005 unrounded to `numeric(5,2)` and 'ab' unpadded to
`char(5)` (`CREATE PROCEDURE`: type modifiers are discarded); finds no
procedure for a bigint argument to an int parameter nor for 1 to a boolean;
gives an OUT parameter NULL whatever is passed; an INOUT parameter receives the
value; copying back into a shorter `varchar` variable fails; an INOUT argument that needs any cast (a `varchar(10)` variable for a `text` parameter, `int` for `bigint`) fails the call as not writable.

## Dynamic SQL

T-SQL `EXEC (…)` and `EXEC [sys.]sp_executesql …` run a string as a batch;
PL/pgSQL `EXECUTE '…'` runs a string as one statement. Both are compared by
the statement their text runs (`equiv/dynamic.rs`), paired in order and in
the same place among the data statements, like calls.

The T-SQL text must be fixed by the code alone: string literals, `NCHAR(n)`
and `CHAR(n)` of a constant, `QUOTENAME` and `+` over those, and local
variables whose last assignment before the statement (`SET @v = …`, `SET @v
+= …`, `DECLARE @v T = …`, or a `DECLARE` without a value: NULL) is such an
expression. The reading walks back through the statements before it, into
the enclosing blocks, past an IF to the statements before the IF and out of
a TRY part. It stops, and the text is unknown, at a parameter, a value read
from a table or a call, any other statement that mentions the variable
(running or printing it does not count), an IF or block that may assign it,
a loop or a CATCH block around the statement, GOTO or labels anywhere,
`SET CONCAT_NULL_YIELDS_NULL`, a declaration that is not `nvarchar(n|max)`,
`varchar(n|max)` or `sysname`, varchar text outside ASCII (it depends on the
code page) and characters outside the Basic Multilingual Plane. A value is
cut to its variable's declared length; a concatenation that would be cut at
4000 / 8000 characters (no operand of a `max` type) is unknown. The
PostgreSQL side must be `EXECUTE` of one string constant, without INTO or
USING.

The T-SQL text must be one statement that runs the same as a batch and as a
PostgreSQL `EXECUTE`: no query that returns rows (T-SQL sends them to the
client, `EXECUTE` discards them), no `OUTPUT` clause, no variables (the
batch cannot see the caller's), no `#temp` table (dropped when the batch
ends), no `SET`, `USE`, transaction control or nested dynamic SQL, and only
one statement (without XACT_ABORT a failing statement lets the next one in
the batch run). The two texts are then compared as two files, and the pair
is equal exactly when that comparison is; its assumptions carry over, plus
`ASM-TSQL-DYNAMIC-SESSION-OPTIONS` (a dynamic batch takes ANSI_NULLS and
QUOTED_IDENTIFIER from the session, not from the procedure). A difference
found by a witness is kept as "different" only when nothing but variable
assignments runs before the first dynamic statement on both sides; after
other statements the tables may not hold the witness rows, and the pair is
unknown.

Facts (SQL Server 2022 and PostgreSQL 18.6, run 2026-10-29): `EXEC (NULL)`,
`EXEC (N'')` and `sp_executesql NULL` run nothing and raise nothing; a varchar
argument to `sp_executesql` fails ("expects parameter '@statement' of type
'ntext/nchar/nvarchar'"); `nvarchar(5)` keeps 'abcde' of 'abcdefgh'; two
3000-character `nvarchar` literals concatenate to 4000 characters, a
`nvarchar(max)` variable first keeps all 6000; a 5000-character `N'…'`
literal is not cut; `QUOTENAME` of 129 characters is NULL; a `#temp` table
created by `EXEC` is gone afterwards; `@x` inside `EXEC` fails ("Must declare
the scalar variable"); `EXEC (N'INSERT … 1/0; INSERT …')` runs the second
INSERT without XACT_ABORT and neither with it. PostgreSQL `EXECUTE NULL`
fails ("query string argument of EXECUTE is null"), `EXECUTE ''` does nothing,
`EXECUTE 'SELECT 1'` discards the row, and one `EXECUTE` may run several
statements; `format('%I', NULL)` fails.

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
| NULLs in a unique key | T-SQL holds one row with NULL per key; PostgreSQL needs `NULLS NOT DISTINCT`, unless the column is NOT NULL on a side or a filter's `c IS NOT NULL` leaves NULLs out; a filter naming the column any other way (`c > 0`) is not proven | `equiv.unique-nulls` |
| String unique key | T-SQL compares case-insensitively and ignores trailing spaces (`ASM-TSQL-CI-COLLATION`); equal under `ASM-CI-COLLATION` in a conversion check | `equiv.case-sensitivity`, `equiv.trailing-spaces` (fixed-length PostgreSQL char ignores trailing spaces too) |
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

A different start, step, last value reached or cycling is `equiv.sequence`
with the first value that differs. A bound the step never reaches is not a
difference, and the near bound (the minimum going up) counts only when both
sides cycle, as their restart point. A T-SQL tinyint sequence against a smallint one with the
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

## Scripts

A conversion turns each T-SQL batch that needs variables or control flow
(a `DECLARE`, `PRINT`, `IF`, `WHILE`, `TRY`, `BEGIN … END`, or any `@variable`)
into one PostgreSQL `DO` block or one Db2 for i compound statement, and keeps
the statements of other batches as plain statements. The engine splits both
scripts the same way (`equiv/script.rs`) and compares each batch with its
block, with only that batch's variables: they end at `GO` as a block's end at
its `END`. Consecutive plain batches are one part, as are consecutive plain
statements on the other side. When the parts do not line up (a block for a
batch the split calls plain, or the reverse), the scripts are compared whole
and the blocks stay not compared.

Inside a part, statements are compared as inside a routine, with these
additions:

| What | Equal when | Otherwise |
|---|---|---|
| Message | `PRINT 'text'` against `RAISE NOTICE '%', 'text'` or `RAISE NOTICE 'text'` (no `%`), a constant of at most 4000 characters | not proven: NULL prints as an empty line in T-SQL and as `<NULL>` in PostgreSQL, numbers and dates in each engine's own format |
| Multi-row `VALUES` | Same rows in the same order; a table whose columns are not all listed (or not in the catalog) adds `ASM-VALUES-ROW-ORDER` for generated values | the engine's difference, or not proven |
| Binary constant | `0xAB01` against `'\xAB01'::bytea` | not proven |
| `+` | Numbers only; on any other type (binary strings, unknown types) it stays per dialect | not proven |
| Generated column | Never written | a statement that writes a computed, `GENERATED ALWAYS` or period column is not proven: one side rejects or replaces the value depending on settings and triggers the statement does not show |

### Variables

Statement terms leave a variable's declared type out, but the type converts
every value assigned to it. Checked on SQL Server 2022 and PostgreSQL 18:
an assignment rounds like a `CAST` (`datetime2(0)` and `timestamp(0)` take
`'2020-01-01 10:00:00.6'` to 10:00:01; `timestamp` takes
`'9999-12-31 23:59:59.9999999'` to 10000-01-01 00:00:00, `datetime2(7)` keeps
it), a T-SQL string or binary variable cuts a longer value off silently where
PostgreSQL raises "value too long", and an overflowing literal
(`DECLARE @i tinyint = 300`) fails while the batch goes on.

So the variables the statements identify (by declaration order, or by name
when a dropped declaration shifts the order: `@CurrentDateTime` and
`v_current_date_time`) are compared by type, with the column type rules
above:

- same values stored: compared; a bounded string adds `ASM-VARIABLE-LENGTH`,
  a wider PostgreSQL type `ASM-VARIABLE-RANGE`, unless every write is a
  literal that fits. Every literal the code assigns must still convert to the
  same value on both sides (T-SQL truncates `1.5` into an `int` to 1,
  PostgreSQL rounds it to 2; `'abcdef'` into `varchar(3)` is cut off in
  T-SQL and raises in PostgreSQL), else the variable is not proven;
- otherwise compared only when every write to the variable, on both sides,
  is a literal that converts to the same known value (`'20200101'` into
  `datetime2(7)` and into `timestamp`); any other write (`SELECT … INTO`,
  `SET @v = @v + 1`, an `OUTPUT` argument, a write inside an `IF`) leaves the
  variable not proven, with the reason on its declaration;
- Db2 for i variables are not compared (no verified type facts; questions
  queued for the IBM i probes).

### Errors

`equiv.error-continues` (a T-SQL statement fails and the batch goes on, the
other side stops) applies when the other side's statements run as one unit:
a routine, a `DO` block, a compound statement. Plain statements run one by
one on both sides, and whether the script goes on is the client's choice
(sqlcmd `-b`, psql `ON_ERROR_STOP`); the behavioural diff models them. A
literal assignment that cannot fail (NULL, a string or binary constant into a
variable of that kind, a literal whose conversion is modelled and succeeds)
is not taken as the failing statement.

## Pairing the statements of a conversion

Two hand-written artifacts are paired statement by statement in order (by
position when both have as many statements, otherwise by an alignment that
keeps only what every best alignment agrees on). When `sqldiff convert`
checks its own output it knows more: each unit records the input span it
read and the output span it wrote, and the check passes these to the
engine as anchors (`Options::anchors`). Then a statement pairs only with a
statement in the same anchor (the innermost one holding it on each side;
several when one input statement gave several pieces of output, such as a
declaration moved to the top of its block and the assignment left in
place). Statements linked through shared anchors form a group: one
statement per side is a pair, a larger group is aligned on its own. A
statement no anchor links has no counterpart, and pairs that cross another
pair are dropped ("the conversion put it in another order"): comparing
statements one by one says nothing about a changed order. So a dropped
PRINT, a statement the engine cannot read, or a run of identical statements
never shifts the pairing of the statements around it.

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

## Casts and literal types

A conversion (CAST, CONVERT, `::`, storing a value in a column, assigning it
to a variable, parameter or return value) has one term in every dialect only
where SQL Server 2022 and PostgreSQL 18 give the same value or the same error
for it. The runs are in `eval/casts/`. Any other conversion keeps the dialect
and the source type (`dcast`) and equals nothing in another dialect, so the
pair is "unknown" or, where a value shows the difference, "different":

| Conversion | T-SQL | PostgreSQL | Rule |
|---|---|---|---|
| decimal or float to an integer | drops the fraction (1.5 → 1, -2.5 → -2) | rounds (numeric half away from zero, float half to even) | `equiv.cast-rounding`; `trunc(x)` makes it equal |
| float to decimal(p,2) | from the binary value (2.675 → 2.67) | from the shortest decimal form (2.68) | `equiv.cast-rounding` |
| float to text | 6 significant digits (1234567 → '1.23457e+006') | shortest exact form ('1234567') | `equiv.float-text` |
| text to a number | '' is 0; '1e3' is no decimal | '' fails; '1e3' is 1000 | `equiv.cast-string-number`; a string literal compares by the value it converts to ('12' as int is 12), not compared where that may round, fail or pad |
| timestamp or time to fewer fractional digits | rounds a half up; time stays below 24:00 | rounds a half away from 2000-01-01; time can reach 24:00:00 | `equiv.time-rounding` |
| datetime2(p), datetime, time(p) to text | every fractional digit ('03:04:00.000'), datetime as 'Jan  2 2026  3:04AM' | no trailing zero fraction ('03:04:00') | `equiv.time-text` |
| a real against another number | the other value becomes real | both become double precision | unknown |
| tinyint arithmetic; a decimal sum past 38 digits | tinyint overflows at 255; decimal(38) overflows | no tinyint; numeric is exact | unknown |
| datetimeoffset to datetime2, date to text, decimal to float, money | | | unknown |

Integers to integers, decimals and floats, decimals to decimals (both round
half away from zero), a decimal of scale 0 to an integer, integers and
decimals of known precision to text long enough for them, strings to strings
(explicit casts truncate silently on both sides, a stored string that is too
long fails on both), dates and timestamps to dates, and timestamps to at
least as many fractional digits are the same everywhere. A T-SQL variable
truncates a string that is too long where a PL/pgSQL one fails, so that
assignment is dialect-specific.

Literals take each dialect's type: `1.5e0` is float in T-SQL and Db2 for i
but numeric in PostgreSQL, `2147483648` is decimal(10,0) in T-SQL but bigint
in PostgreSQL, `0x1F` is varbinary in T-SQL and the integer 31 in
PostgreSQL, and PostgreSQL reads `N'..'` as character(n), which is not
compared. Db2 for i conversions are compared only between integers, to wider
decimals and for strings that fit; the open questions are probes V31 in
`probes/db2i`.

Without catalogs a value's type is mostly unknown. Storing or assigning it
states `ASM-UNTYPED-STORE`, and casting it to a number or string states
`ASM-UNTYPED-CAST`; a cast of it to anything else is unknown.

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

## String collation in conversions

Davy's decision (2026-10-29): a conversion keeps the target's text types (no
nondeterministic ICU collation, no `citext`) and flags each statement whose
result T-SQL's collation can change. In a conversion check (`sqldiff
convert`, woven instances; `equiv::compare::ci_collation_scope`), string
equality that differs from T-SQL's only in case sensitivity and trailing
spaces is equal under `ASM-CI-COLLATION`, which the reviewer accepts per
statement: `=` and `<>` (also in joins), `IN` lists whose operand and items
are all strings (catalog types), GROUP BY keys, DISTINCT, unique keys and
indexes, and foreign keys. The assumption also names what SQL Server 2022's
default `SQL_Latin1_General_CP1_CI_AS` equates beyond case for nvarchar
(checked on SQL Server 2022: `N'æ' = N'ae'`, `N'ß' = N'ss'`, full-width
`N'Ａ' = N'A'`, `NCHAR(0)` ignored; none of these for varchar). LIKE is
covered when the value is varchar and the pattern a literal without `[`
(a T-SQL character class) or `\` (a PostgreSQL escape). Checked on SQL
Server 2022 and PostgreSQL 18: T-SQL LIKE ignores case, drops a varchar
value's trailing spaces (`'ab ' LIKE '%b'` is true; not for nvarchar),
expands `N'æx' LIKE N'ae%'` to true, and keeps the pattern's trailing
spaces significant, as PostgreSQL does. A char value (blank-padded on one
side only) or a pattern that is not a literal is not covered. Ordering
(`<`, `BETWEEN`, ORDER BY), `IN (subquery)` and simple CASE are not covered
either: they depend on more than case and trailing spaces, and stay not
proven. IF and WHILE conditions are compared as statements are, so they
are covered the same way. A plain `sqldiff diff` still reports `equiv.case-sensitivity` and
`equiv.trailing-spaces`.
