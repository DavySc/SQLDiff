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

## Benchmark

`eval/cross-dialect/` in the project files: `bench.py` converts the
WideWorldImporters procedures (Website, WebApi, Application, Integration) to
PostgreSQL and diffs each pair; `mutate.py` applies behaviour-changing edits to
the pairs proven equal. On 2026-10-24: 15 of 15 fully converted procedures
equal under `ASM-OWNER-PRIVILEGES` and `ASM-IDENT-FOLD` (0 of 15 before), and
255 mutations with no false equal.
