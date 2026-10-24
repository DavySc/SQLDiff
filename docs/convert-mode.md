# Convert mode (P0)

`sqldiff-gui convert [FILE]` opens a two-pane page: T-SQL you can edit on the left, PostgreSQL on the right. The right side is regenerated about 150 ms after you stop typing (debounced; stale responses are dropped). A sample is shown when no file is given; nothing is ever written to disk. `--stdout` prints the conversion and the per-statement badges instead of serving the page.

T-SQL → PostgreSQL and T-SQL → Db2 for i have rules (`--to db2i`; see [Db2 for i](#db2-for-i) below). The feasibility study that led here is the "Live conversion feasibility" doc in the project; this is its phase P0.

## What "correct" means here

Every converted unit (a statement, a compound header such as `IF … THEN`, a routine header, or a hoisted `DECLARE`) carries a status, best to worst:

| Status | Meaning |
|---|---|
| converted | Rules that need no assumption, and the engine found nothing in this unit |
| under assumptions | Equal if every listed assumption holds (each has a stable id such as `ASM-CI-COLLATION`) |
| needs a decision | Several faithful targets exist; the note says which one was emitted and the alternative |
| differs | The engine's diff of input against output found a difference, leftover T-SQL, or the output does not parse as PostgreSQL |
| not converted | No rule applies; the original is kept as a commented block with the reason |

The status is the worst of the rule notes and of the engine's own checks (`checked`): the diff of input against output, and the output checked on its own as the target dialect (`output_check`, what `sqldiff check -d <target>` reports). Any binding finding (unknown built-in, table, column, variable, type, routine signature), leftover source syntax (residue), target limit or hazard in the output makes its unit `differs`, proven or not; analysis findings about the code's own quality do not. A gating finding that points into no unit is listed in `unplaced`, and a file with one never counts as converted. The engine check runs the normal `sqldiff diff` of the input (side A) against the output (side B) and attaches each finding to the unit it points into, so engine findings such as `error.contract` or `write.unpaired` show up next to the statement that caused them. Today the cross-dialect engine cannot prove statement equality, so the best a unit can get is "converted": tested rules, no assumption, nothing found. "Proven" needs the equivalence and IR work.

A rule never guesses: if any part of a statement has no rule (for example `@@ERROR`, `UPDATE … FROM` with a join, `DATEDIFF(week, …)`), the whole statement is commented out and marked not converted.

### Idiom rules

Before a statement is commented out, it goes to the idiom rules (`sqldiff_core::rules`, see [idiom-rules.md](idiom-rules.md)), which recognise whole patterns rather than tokens: `MERGE` and `IF EXISTS` upserts → `INSERT … ON CONFLICT`, `IF NOT EXISTS … INSERT` → `ON CONFLICT DO NOTHING`, the `LEFT JOIN … IS NULL` delete → `NOT EXISTS`, `INSERT` + `SCOPE_IDENTITY()` → `RETURNING … INTO` or `lastval()`, and `sp_getapplock` with its return-code check → `pg_advisory_xact_lock`. Their output replaces the comment when they convert the statement completely, with their notes (and an `idiom-rules` note naming the rules) on the unit. Patterns that span statements (`INSERT` + `SCOPE_IDENTITY`, `sp_getapplock` + its check) become one unit, since the token rules cannot see them as a pair.

Two limits keep this honest. Statements the token rules already convert keep their conversion (a plain `IF EXISTS … UPDATE … ELSE INSERT` stays an `IF`, with the race intact, rather than becoming an atomic `ON CONFLICT`). And a statement refused because of where it stands (a result set in a procedure, `COMMIT` inside a TRY block) is never handed to the rules, which see statements out of context. The rules never check themselves: their output goes through the same engine diff as everything else.

### Never silent

- A T-SQL built-in the target does not have (on the built-in lists in `profiles/builtins.toml`) refuses its statement instead of passing through: `HASHBYTES`, `CHECKSUM`, `SERVERPROPERTY`, `RAND(seed)`, … Names in object position (`INSERT INTO log (…)`) are not calls.
- A statement that builds text later run by `EXEC (@sql)` or `sp_executesql @sql` is `differs` (`dynamic-sql.text`): the statement converts, the SQL in the string is still T-SQL.
- Inside a routine or block, a statement kept as a comment is followed by `RAISE EXCEPTION 'sqldiff: statement at source line N was not converted'` (Db2 for i: `SIGNAL SQLSTATE '75001'`), so the converted routine stops there instead of quietly skipping it. At the top level of a script the statement is only commented out.
- `WITH (…)` options are dropped only when they change storage or performance, never results (`DATA_COMPRESSION`, `FILLFACTOR`, `PAD_INDEX`, `ALLOW_ROW_LOCKS`, `IGNORE_DUP_KEY = OFF`, …). `SYSTEM_VERSIONING`, `MEMORY_OPTIMIZED`/`DURABILITY`, `IGNORE_DUP_KEY = ON` and anything unknown refuse the statement, and so does a system-versioned table (`PERIOD FOR SYSTEM_TIME`).

## Whole trees: `sqldiff convert`

```sh
sqldiff convert SRC -o OUT --to postgres   # or --to db2i; --from tsql is the default
```

`SRC` is a directory (every `.sql` file below it, hidden directories skipped) or one file. `OUT` mirrors the layout with the same file names, so `sqldiff tree SRC OUT --dialect-a tsql --dialect-b postgres` pairs them again. `OUT` must be empty (or pass `--overwrite`) and may not be inside `SRC`. Files are read as UTF-8 (with or without BOM) or UTF-16 with a BOM, as SSDT saves some; anything else is listed as unreadable and not converted.

Two passes, each over `-j N` threads (default: all cores):

1. **Rules.** Every file is converted with the source catalog of the whole tree (`CREATE TABLE/VIEW/PROCEDURE/FUNCTION` from every input file). Rules that need another file use it: `EXEC p … @x OUTPUT` converts to `CALL p(…, v_x)` only when `p` is in the tree, every OUTPUT parameter of `p` gets a declared variable marked OUTPUT, and nothing else is marked OUTPUT (a CALL would assign an INOUT argument back where T-SQL does not). Named arguments then need no `ASM-PARAM-NAMES` assumption.
2. **Check.** Every file is diffed against its output and its output checked on its own, with the source catalog for side A and the catalog of the whole output tree for side B. A call, column or table defined in another file binds; one whose definition could not be converted does not exist in the output tree, so the statements that use it are `differs` (for example every query on a table with a `geography` column).

Every unit that is not plainly converted gets a comment line above it in the output, `-- [sqldiff: under assumptions] ASM-…` or `-- [sqldiff: differs] bind.unknown-builtin` (not-converted units already open with `-- [not converted]`), so a reviewer reading only the output tree sees them. `--no-catalog` checks each file alone.

`OUT/conversion-report.json` has, per file, its status (the worst unit status), unit counts, time, and every unit that is not plainly converted with its input and output line (in the annotated file), rules and reason; totals by status and the most frequent blockers. `OUT/conversion-report.md` is the same for reading. Exit code 0 when every file is converted or converted under assumptions, 1 otherwise, 2 on usage or I/O errors.

On WideWorldImporters (336 files, 58,610 lines, 4 cores): PostgreSQL in 9 s, Db2 for i in 25 s. 33 files convert completely for each target; most of the rest carry assumptions, dynamic SQL or tables that need types without a rule (`geography`), whose dependants then do not bind.

## Templates

`sqldiff-gui convert FILE --template --pack PACK` converts a generator template. The placeholders and special forms follow the pack's `[template]` syntax and the same placeholder rules as `sqldiff diff --template` ([templates-and-composition.md](templates-and-composition.md)); `--parameter NAME` (repeatable) lists the parameter names when the delimiters could also be SQL (`[...]` in T-SQL), so only those names are placeholders.

The template's fragment (placeholders as `TPL_<NAME>` identifiers, special forms as comments) is converted and checked like any input, then the placeholders and special forms go back into the output as the template writes them, so the right pane is a PostgreSQL template for the same generator. Badges and spans point into the template. Each unit with a placeholder carries `ASM-TEMPLATE-PLACEHOLDER`: the placeholder was converted as a name, so every value the generator fills in must be a name PostgreSQL accepts there. A placeholder the rules renamed beyond recognition is flagged on its unit (`template.placeholder-lost`, differs), and a directive that did not survive in order is flagged too; neither is put back by guess. The header shows how many placeholders the template has.

```sh
sqldiff-gui convert docs/examples/templates/save-entity.tpl.sql --template --pack docs/examples/pack --isolated
```

## The page

- **Location pane** (the strip at the right edge): every unit that is not plain "converted", at its position in the output; click to jump. The box shows the visible part.
- **Navigation:** `Alt+↓` / `Alt+↑` next and previous unit that needs attention, `Alt+Home` / `Alt+End` first and last. The status chips in the header jump to the next unit with that status.
- **Linked selection:** moving the caret in the editor selects the unit it is in; clicking a line on the right selects that unit and highlights its source in the editor. Scrolling one side brings the matching unit to the top of the other.
- **Detail pane:** source and output of the selected unit side by side, with each rule note and each engine finding.
- **Accept everywhere:** an assumption you know holds (for example `ASM-DBO-SCHEMA`) can be accepted once; units that only needed it count as converted. The choice is kept in this browser only and never hides engine findings.
- **Copy** and **Save .sql** take the whole output.

## Rules (P0)

Token-level rules over the lossless CST; expressions are still flat, so nothing here relies on operand types beyond what declarations and literals show.

- Routines: `CREATE PROCEDURE` → `CREATE OR REPLACE PROCEDURE … LANGUAGE plpgsql AS $$ … $$`, scalar functions likewise; parameters `@ItemId` → `p_item_id`, locals → `v_…` (the prefixes avoid PL/pgSQL's variable/column ambiguity); `OUTPUT` → `INOUT`; `RETURN n` in a procedure → an `INOUT return_code`. `DECLARE` is hoisted, with initialisers kept as assignments in place. Scripts with variables or control flow become a `DO` block.
- Control flow: `IF/ELSE IF/ELSE` → `IF/ELSIF/ELSE/END IF`, `WHILE` → `WHILE … LOOP`, `BREAK` → `EXIT`, `TRY/CATCH` → `BEGIN … EXCEPTION WHEN OTHERS` (with the subtransaction assumption), and the `ROLLBACK; THROW;` CATCH idiom → let the error propagate.
- Statements: `SET @x = …` → `:=`, `SELECT @x = … FROM` → `SELECT … INTO`, `TOP n` → `LIMIT n`, `PRINT` → `RAISE NOTICE`, `THROW` → `RAISE`, `EXEC` → `CALL`, `DELETE t` → `DELETE FROM t`, transaction statements with the procedure caveats, session options that PostgreSQL always behaves like.
- DDL: types, `IDENTITY` → `GENERATED ALWAYS AS IDENTITY`, `#temp` → `TEMP`, `BIT` defaults, storage options and `CLUSTERED` dropped, views → `CREATE OR REPLACE VIEW`.
- Expressions: `[x]` → `x` (or `"x"`), `dbo.` dropped, `N'…'` → `'…'`, string `+` → `||`, `ISNULL`, `LEN`, `CHARINDEX`, `NEWID`, `IIF`, `GETDATE`/`SYSDATETIME`/`GETUTCDATE`, `CAST`/`CONVERT` types, `DATEADD`, `DATEPART`, `YEAR/MONTH/DAY`, `DATEDIFF` for day/month/year (boundary counts, as T-SQL defines them), `STRING_AGG … WITHIN GROUP`, `REPLICATE`, `SCOPE_IDENTITY`, `ERROR_MESSAGE`/`ERROR_NUMBER` in handlers, table hints, `DATEFROMPARTS` → `make_date`, `QUOTENAME(x)` → `quote_ident(x)` (`ASM-QUOTENAME-IDENT`: used as an identifier), `RAND()` → `random()` (`ASM-RAND-PER-QUERY`: once per query in T-SQL, per row in PostgreSQL), `NEXT VALUE FOR s` → `nextval('s')`, `0x…` → `'\x…'::bytea` (PostgreSQL 16+ would read `0x…` as an integer; `ASM-HEX-BINARY`).
- Sequences: `CREATE SEQUENCE` with every bound written out, because T-SQL starts an ascending sequence at the type's minimum (`-2147483648` for `INT`) and PostgreSQL at 1; the cache is left at PostgreSQL's `CACHE 1` (a larger one is per session and hands out values out of order).
- Metadata: `sp_addextendedproperty` with literal arguments → `COMMENT ON SCHEMA/TABLE/VIEW/PROCEDURE/FUNCTION/COLUMN/INDEX/CONSTRAINT` (`ASM-EXTPROP-COMMENT`: the property name is dropped and there is one comment per object). Db2 for i: schemas, tables, views, routines and columns only, since an index lives in the current schema there.
- Parameters may be written `@p AS int`.

The tests in `crates/sqldiff-convert/tests/convert.rs` convert every T-SQL file in the corpus and require that the output parses as PostgreSQL and carries no T-SQL residue outside commented-out units.

## Db2 for i

`sqldiff-gui convert --to db2i` (or `convert(text, Tsql, Db2i)`) converts T-SQL to Db2 for i SQL PL with the same walk as PostgreSQL: the statements, refusals and engine check are shared, and the target decides the spelling and the notes. The output is diffed against the input by the engine (whose Db2 lowering does not use these rules) and must parse as Db2 for i.

Vendor facts come from [vendor-facts-verification.md](vendor-facts-verification.md). Facts that were never checked on a real IBM i (V4, V9, V15, part of V14) never decide a conversion silently: every unit that depends on one carries an assumption naming the fact (`ASM-FACT-V9`, `ASM-FACT-V14`, `ASM-DB2-SCRIPT-COMMIT` for V4/V5) or the lock-hint note points at V15.

What differs from the PostgreSQL output:

- **Commit level.** Procedures get `SET OPTION COMMIT = *CS` (`ASM-DB2-COMMIT-CS`): each statement is atomic and reads see committed rows, as in T-SQL. Without it the level comes from the creating job (V5), and under `*NONE` nothing is rolled back, not even a failed statement or an ATOMIC MERGE (V2, V4). Scripts run at the job's level, so their transaction statements, `SAVEPOINT` and MERGE note it.
- **Authority.** Every routine states `USRPRF` and `DYNUSRPRF`: `*OWNER` for `EXECUTE AS OWNER` (`ASM-OWNER-PRIVILEGES`), else `*USER`, so the authority never depends on the naming convention at creation (`*NAMING`, fact V25). On PostgreSQL, `EXECUTE AS OWNER` becomes `SECURITY DEFINER`, except in a procedure that commits or rolls back, which PostgreSQL forbids (needs a decision). `RECOMPILE`, `ENCRYPTION` and `SCHEMABINDING` are dropped as having no effect; other `WITH` options need a decision. See [cross-dialect-proofs.md](cross-dialect-proofs.md).
- **Routines.** `CREATE OR REPLACE PROCEDURE … (IN p_x …, INOUT p_y …) LANGUAGE SQL … BEGIN … END;`; declarations are hoisted to the top of the block (variables, then cursors, then handlers, as Db2 requires). `RETURN n` stays (`ASM-RETURN-STATUS`: callers read `DB2_RETURN_STATUS`). Scripts with variables become a compound statement `BEGIN … END;`.
- **Result sets.** A query that returns rows from a procedure becomes `DECLARE result_n INSENSITIVE CURSOR WITH RETURN TO CLIENT FOR …` plus `OPEN result_n;` where it stood, and the procedure declares `DYNAMIC RESULT SETS n`. Functions and scripts still cannot return rows.
- **Control flow.** `ELSEIF`, `WHILE … DO … END WHILE`, with a label on every loop so `BREAK`/`CONTINUE` become `LEAVE`/`ITERATE label`. A statement list left without a statement (a dropped `PRINT`) gets `BEGIN END;`.
- **Errors.** `SET XACT_ABORT ON` (before any other work) becomes an EXIT handler `ROLLBACK; RESIGNAL;` for the whole routine. `TRY/CATCH` becomes a compound statement whose EXIT handler is the CATCH part: unlike PostgreSQL nothing the TRY part completed is undone, which is T-SQL's behaviour. `ERROR_MESSAGE()`/`ERROR_NUMBER()` read variables filled by `GET DIAGNOSTICS` at the start of the handler. The `ROLLBACK; THROW;` idiom becomes `ROLLBACK; RESIGNAL;`. `THROW n, msg, s` → `SIGNAL SQLSTATE '75000' SET MESSAGE_TEXT = msg`; `PRINT` is dropped (needs a decision).
- **Transactions.** `BEGIN TRAN` is dropped (a unit of work is always open under commitment control); `COMMIT`/`ROLLBACK` stay, also inside TRY and CATCH. `SAVE TRAN x` → `SAVEPOINT x ON ROLLBACK RETAIN CURSORS` and `ROLLBACK TRAN x` → `ROLLBACK TO SAVEPOINT x` (V14). A `ROLLBACK TRAN name` that names no savepoint is refused for both targets.
- **MERGE** is kept (`MERGE INTO`, `WHEN NOT MATCHED BY TARGET` → `WHEN NOT MATCHED`); `NOT MATCHED BY SOURCE` (V1) and `OUTPUT` are refused.
- **Temporary data.** `#t` → `DECLARE GLOBAL TEMPORARY TABLE SESSION.t … ON COMMIT PRESERVE ROWS LOGGED WITH REPLACE`; a table variable `@t` → `SESSION.tv_t … NOT LOGGED ON ROLLBACK PRESERVE ROWS`, the only form that keeps its rows on rollback (V16).
- **Queries.** `TOP n` → `FETCH FIRST n ROWS ONLY`; a `SELECT` without `FROM` gets `FROM SYSIBM.SYSDUMMY1`; `SELECT @a = … FROM` → `SELECT … INTO` (Db2 raises 21000 on several rows).
- **DDL.** Types map to Db2 (`BIT` → `SMALLINT`, `MONEY` → `DECIMAL(19, 4)`, `DATETIME2(n)` → `TIMESTAMP(n)`, `UNIQUEIDENTIFIER` → `CHAR(36)`, `(N)VARCHAR(MAX)` → `CLOB`/`NCLOB` as a decision, `DATETIMEOFFSET` refused). Column defaults must be constants or `CURRENT TIMESTAMP`, so `DEFAULT NEWID()` or `SYSUTCDATETIME()` is refused. Primary key columns get `NOT NULL`. Several `ADD COLUMN`s are written without commas.
- **Sequences.** As for PostgreSQL, every bound written out, with `NO CACHE` so values are handed out in order. `NEXT VALUE FOR` is kept (Db2 for i has it); as a column default it is still refused.
- **Built-ins.** `QUOTENAME(x)` → `'"' || REPLACE(x, '"', '""') || '"'` (`ASM-QUOTENAME-IDENT`), `RAND()` stays (`ASM-RAND-PER-QUERY`), `0x…` → `BX'…'` (`ASM-HEX-BINARY`). `GETDATE()` → `CURRENT TIMESTAMP` (per statement, V20), `GETUTCDATE()` → `CURRENT TIMESTAMP - CURRENT TIMEZONE`, `NEWID()` → `GENERATE_UUID()` (upper case, V13), `LEN` → `LENGTH(RTRIM(…))`, `CHARINDEX` → `LOCATE`, `SCOPE_IDENTITY()` → `IDENTITY_VAL_LOCAL()`, `STRING_AGG` → `LISTAGG`, `DATEADD` → labelled durations (`d + (n) DAYS`), `DATEPART` → `YEAR()`…`SECOND()`, `DATEDIFF` day/month/year → `DAYS(b) - DAYS(a)` and year/month arithmetic. A cast to a number notes V9 unless the operand is a number.

The engine still reports what the rules cannot change: a T-SQL procedure without `XACT_ABORT ON` goes on after a failed statement while Db2 stops (`equiv.error-continues`), and the time source and case sensitivity findings land on their units. `tests/convert_db2.rs` converts the whole T-SQL corpus to Db2 for i and requires that it parses and leaves no T-SQL residue.

## Next (P1 and later)

Expression trees (from the equivalence work) and binding let rules see types, so `+`, comparisons and `ISNULL` stop needing assumptions; the behavioural IR (WP3) and more idiom rules bring `@@ERROR`/`@@ROWCOUNT` checks and transaction structure into scope, and a catalog (`Facts`) lets the idiom rules prove key uniqueness and identity columns instead of assuming them. For Db2 for i, the unverified vendor facts (V4, V9, V15) need a real IBM i before their assumptions can be dropped, and the idiom rules have no Db2 forms yet (MERGE and upserts are converted by the token rules instead).
