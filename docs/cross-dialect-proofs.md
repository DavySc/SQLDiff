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

