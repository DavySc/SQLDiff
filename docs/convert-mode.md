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
- `WITH (…)` options are dropped only when they change storage or performance, never results (`DATA_COMPRESSION`, `FILLFACTOR`, `PAD_INDEX`, `ALLOW_ROW_LOCKS`, `IGNORE_DUP_KEY = OFF`, …). `MEMORY_OPTIMIZED`/`DURABILITY`, `IGNORE_DUP_KEY = ON` and anything unknown refuse the statement. `SYSTEM_VERSIONING = ON` has its own rule (below) for PostgreSQL and is refused for Db2 for i.

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

A template with generator directives (special forms such as an include) has them converted as comments, without the text they add, and the text they add can change what the rest means (a header's `SET XACT_ABORT ON`, a declaration, the end of a block). So every unit of such a template carries `ASM-TEMPLATE-DIRECTIVE`: it holds only if the woven output does, and no unit of it counts as converted until the woven instances are checked.

### Template trees

```sh
sqldiff convert templates/ -o out/ --to postgres --template --pack PACK [--parameter NAME …] [--extension tpl …]
```

`sqldiff convert --template` converts a whole tree of templates the same way, with the same two passes, catalogs and report as plain files. The output tree holds templates for the target. Every file is read as a template; files without placeholders (an included header, say) come through as plain SQL. `--extension` (repeatable, default `sql`) picks the files when the generator uses another extension.

- **One set of stand-ins for the tree.** All fragments use the same stand-in name for the same placeholder, so a table that `tables/entity.tpl.sql` creates as `<%SCHEMA%>.<%TABLE%>` binds in every template that uses `<%SCHEMA%>.<%TABLE%>`, and a misspelt column there is `bind.unknown-column`.
- **The premise is stated.** Binding across templates assumes one weave fills each placeholder with the same value in every template; two templates woven with different values would bind to the wrong instance's object. So in a tree with catalogs every unit with a placeholder also carries `ASM-TEMPLATE-SHARED-VALUES`, discharged only by checking the woven instances (`sqldiff instances`). `--no-catalog` binds nothing across files and needs no such assumption.
- **No binding by guess.** An object whose name holds a placeholder and that more than one file creates (two templates both creating `<%SCHEMA%>.<%TABLE%>`, for different instances) is left out of the catalogs, so nothing binds against the wrong one; the units that use it still carry `ASM-TEMPLATE-PLACEHOLDER`.
- **The output.** `ASM-TEMPLATE-DIRECTIVE` is written once, as the file's first line, instead of above every unit; every other status line is written as for plain files. Findings, notes and reasons quote placeholders as the template writes them.
- **The report** says `"templates": true`, and each file lists its placeholders, unresolved candidates and directives under `template`.

### Fragments, routine halves and value kinds

Not every template is whole statements. The pack's `[template]` section says what the rest are, so each converts as what it is:

```toml
[template]
# What a placeholder's values are. Default: name.
values = { KEYTYPE = "type", NOTE = "text", LIMIT = "number", FILTER = "expression" }
# Templates that hold part of a statement (first match wins). Default: statements.
fragments = [
  { glob = "where/**", kind = "predicate" },
  { glob = "columns/**", kind = "select-list" },
  { glob = "parts/body*.sql", kind = "routine-body" },
]
# A routine's opening and closing templates, converted as one routine.
pairs = [{ head = "parts/head.sql", foot = "parts/foot.sql" }]
```

- **Fragment kinds** are `statements`, `predicate` (what follows `WHERE`), `expression` (one value, also `select-list`), `set-list` (what follows `UPDATE … SET`) and `routine-body` (statements inside `BEGIN … END`). A fragment is converted inside a stand-in statement of that kind, from T-SQL only, and cut out again with the template's own leading and trailing white space. If converting it changed the stand-in around it (a declaration hoisted, a parameter added, code moved across its edge, or a rule that did not apply), the template is kept as it is and flagged `template.context-leak` (differs), with the conversion's own reason when it gave one: it cannot be converted on its own. Otherwise every unit carries `ASM-TEMPLATE-FRAGMENT`: its names were not bound and its meaning in the woven statement was not checked. The stand-in's table is in the catalogs with unknown columns, so binding against it is never a finding.
- **A routine-body fragment knows no parameters.** Converted on its own, the T-SQL parameter `@Id` in a body reads as a local variable and becomes `v_id`, while the opening template turns the same parameter into `p_id`. The unit then fails binding (differs), and so do the woven instances: nothing is claimed. Convert such a body by hand, or weave it into its routine before converting.
- **Pairs** are converted together, as one routine, with a marker between them, and split again at the marker. If the conversion moved code across the marker, both templates are kept as they are and flagged `template.pair-split` (differs). A template is in at most one pair; the bodies woven between the halves are converted on their own, as fragments.
- **Value kinds**: only `name` values are converted as names (`ASM-TEMPLATE-PLACEHOLDER`, checked against the target's name rules per instance). A `text` or `number` value means the same in every dialect. A `type`, `expression` or `sql` value is dialect code the rules cannot convert, so its unit is a decision (`template.value-per-dialect`): give the target's value in the instance manifest under `params_for`. The instance check discharges that decision, and a dialect-code value given only once, for every dialect, stays an assumption there (`ASM-TEMPLATE-VALUE-SAME`).

A converted template tree is never the last word: a template is right only when every instance the generator weaves from it is, which the instance check verifies.

### Checking every woven instance

```sh
sqldiff instances instances.toml --source templates/ --target out/ --to postgres -o check/ --pack PACK \
    [--woven generated/] [--accept ASM-…] [--catalog schema.sql]
```

`sqldiff instances` weaves every instance the generator produces twice, from the source templates and from the converted ones, with the same values, and checks each pair. The manifest lists the instances (a template, or several parts in order), their parameter values, and values per dialect where a value differs between dialects:

```toml
[params]                        # every instance
SCHEMA = "dbo"
[params_for.postgres]           # every instance, PostgreSQL side only
SCHEMA = "public"

[[instance]]
name = "Sales/SaveCustomer"     # unique; also the woven files' name
template = "procs/save-entity.tpl.sql"
output = "Sales/SaveCustomer.sql"   # the generator's own output under --woven (default: name + .sql)
params = { ENTITY = "Customer", TABLE = "Customer" }

[[instance]]
name = "Sales/Composed"
parts = ["header.sql", { template = "body.tpl.sql", params = { X = "1" } }]
```

Values merge manifest, then instance, then part; at each level a dialect's own values win. The names the manifest gives values for are the parameter list for the placeholder rules (`--parameter` adds more). The pack's `[template] include` names the special form that pulls another file in: its first capture is the path, next to the including file first, else from the tree root, never outside it (a symbolic link that points out of the tree is not followed). Any other special form is generator logic the weaver cannot run, and so is a missing value or an include cycle: that instance is **not woven** and nothing about it is claimed.

Each target instance is checked the way `sqldiff convert` checks its output, with the catalog of every woven instance on its side (and any `--catalog`):

| Check | Fails as |
|---|---|
| The target weave parses as the target dialect | differs |
| The output check: binding, leftover source syntax, target limits, hazards | differs |
| The engine's diff of the source weave against the target weave: a proven difference, or source syntax left in the target | differs |
| The engine's diff: a finding it could neither prove nor rule out (`equiv.unproven`, …) | unknown |
| The engine's diff could not compare a statement at all (`equiv.not-covered`): today every DDL statement, such as `CREATE TABLE`, so a table instance is never converted, whatever its column types | unknown |
| The engine's diff: a finding equal only under assumptions (listed with the rule) | under assumptions |
| A name a value is part of is a PostgreSQL reserved word unquoted (`Order`, `user`), or longer than 128 characters on Db2 for i | differs |
| A PostgreSQL name over 63 bytes is truncated (`ASM-PG-NAME-TRUNCATED`); two values that truncate to the same name | under assumptions; differs |
| A Db2 for i name from a value: no verified reserved-word list is built in (`ASM-DB2I-RESERVED-UNCHECKED`) | under assumptions |
| A `type`, `expression` or `sql` value with no value of its own for the target (`params_for`) (`ASM-TEMPLATE-VALUE-SAME`) | under assumptions |
| Two instances create the same table or routine (the later one fails or replaces the other; binding used one definition) (`weave.object-twice`) | unknown |
| The source weave does not parse | unknown |

The woven instances discharge `ASM-TEMPLATE-PLACEHOLDER`, `ASM-TEMPLATE-DIRECTIVE`, `ASM-TEMPLATE-SHARED-VALUES`, `ASM-TEMPLATE-FRAGMENT` and `template.value-per-dialect`: the values are known, the includes and fragments woven in, and each instance woven with its own values. Name checks apply only to `name` values. The status lines `sqldiff convert` wrote into the converted templates are read from the woven target, and the rule assumptions and decisions on them stand, because no instance can check them: an assumption you know holds is accepted with `--accept ID` (listed in the report); a decision or a difference is resolved by hand and its line deleted.

With `--woven DIR` (the generator's real output) each source weave must equal its `output` file, ignoring trailing white space, or the instance is not woven (`weave.generator-mismatch`, with the first line that differs), and every file there that no instance names is listed under "not in manifest". That is what shows the manifest describes the generator completely.

This is a stricter bar than `sqldiff convert`'s, where only a proven difference holds a unit back: an instance is converted only when no finding of the diff is left open. The assumptions the diff makes about every pair (name folding, the default schema) are listed per instance as `engine_assumptions` and do not count, as in `convert`.

`check/instance-report.json` and `.md` give each instance's status and items (side, line in the woven file, rule), each converted template's status (the worst instance that uses it, includes too) and the converted templates no instance uses ("not exercised", never checked). The woven files are written to `check/source/` and `check/target/`. Exit code 0 when every instance is converted or converted under assumptions and all generator output is accounted for, 1 otherwise.

### Keeping dialect trees in step

Once the generator keeps one template set per dialect, the sets drift: a template is added, changed or rewired in one tree and not in the others. `sqldiff drift` finds that, in CI or before a commit:

```sh
sqldiff drift --tree tsql=templates/mssql --tree postgres=templates/pg --tree db2i=templates/ibmi -o drift/ --pack PACK \
    [--reference tsql] [--base origin/main] [--manifest instances.toml] [--accept ASM-…] [--extension tpl …]
```

| Check | Rule |
|---|---|
| Every template (same relative path) is in every tree | `drift.missing` |
| A template has the same placeholder names in every tree, so one set of values weaves all of them | `drift.placeholders` |
| A template has the same directives (the pack's special forms, with their arguments) in the same order in every tree | `drift.directives` |
| With `--base REV`: a template changed between the merge base of `REV` and `HEAD` and the working tree in some trees and not in all | `drift.changed-alone` |

A change made in one tree on purpose adds the pack's `intentional_difference` author marker to that template (`-- SYNC-OK: why`, say) in the same change; the report lists it with its reason instead of failing. Only a marker on a line the change adds counts: one committed earlier explains no later change. With `--manifest`, every instance is also woven from the reference tree (the first `--tree` unless `--reference` says otherwise) and from each other tree and checked exactly like `sqldiff instances`, each into `drift/instances/<dialect>/`; a hand edit that changes what a template means then fails as differs or unknown there.

`drift/drift-report.json` and `.md` list every finding by template and tree. Exit code 0 when nothing drifted and every instance check passed, 1 otherwise, so a CI step is the command itself:

```yaml
- run: sqldiff drift --tree tsql=templates/mssql --tree postgres=templates/pg --tree db2i=templates/ibmi
         --base origin/${{ github.base_ref }} --manifest instances.toml --pack .sqldiff/pack -o drift/
```

`--base` needs the base branch's history in the checkout (`fetch-depth: 0`).

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
- Statements: `SET @x = …` → `:=`, `SELECT @x = … FROM` → `SELECT … INTO`, `TOP n` → `LIMIT n`, `PRINT` → `RAISE NOTICE`, `THROW` → `RAISE`, `EXEC` → `CALL`, `DELETE t` → `DELETE FROM t`, `DELETE t FROM t JOIN u ON c WHERE w` → `DELETE FROM t USING u WHERE (c) AND (w)` (Db2 for i: `DELETE FROM t WHERE EXISTS (SELECT 1 FROM u WHERE (c) AND (w))`), transaction statements with the procedure caveats, session options that PostgreSQL always behaves like.
- DDL: types, `IDENTITY` → `GENERATED ALWAYS AS IDENTITY`, `#temp` → `TEMP`, `BIT` defaults, storage options and `CLUSTERED` dropped, views → `CREATE OR REPLACE VIEW`.
- Expressions: `[x]` → `x` (or `"x"`), `dbo.` dropped, `N'…'` → `'…'`, string `+` → `||`, `ISNULL`, `LEN`, `CHARINDEX`, `NEWID`, `IIF`, `GETDATE`/`SYSDATETIME`/`GETUTCDATE`, `CAST`/`CONVERT` types, `DATEADD` (see below), `DATEPART`, `YEAR/MONTH/DAY`, `DATEDIFF` for day/month/year (boundary counts, as T-SQL defines them), `STRING_AGG … WITHIN GROUP`, `REPLICATE`, `SCOPE_IDENTITY`, `ERROR_MESSAGE`/`ERROR_NUMBER` in handlers, table hints, `DATEFROMPARTS` → `make_date`, `QUOTENAME(x)` → `quote_ident(x)` (`ASM-QUOTENAME-IDENT`: used as an identifier), `RAND()` → `random()` (`ASM-RAND-PER-QUERY`: once per query in T-SQL, per row in PostgreSQL), `NEXT VALUE FOR s` → `nextval('s')`, `0x…` → `'\x…'::bytea` (PostgreSQL 16+ would read `0x…` as an integer; `ASM-HEX-BINARY`).
- Sequences: `CREATE SEQUENCE` with every bound written out, because T-SQL starts an ascending sequence at the type's minimum (`-2147483648` for `INT`) and PostgreSQL at 1; the cache is left at PostgreSQL's `CACHE 1` (a larger one is per session and hands out values out of order).
- `DATEADD` keeps the type of its date argument, as T-SQL does, so the rewrite depends on that type: from a declared variable or parameter, a column of a table defined in the converted files (a bare name needs every table of the statement known and agreeing on the type; a statement with CTEs is not resolved), a `CAST`/`CONVERT`, a nested `DATEADD` or `GETDATE()`/`SYSDATETIME()`. On a `date`, days and weeks are integer addition (`d + 5`, `d - 5`, `d + 7 * n`) and months, quarters and years are `(d + make_interval(months => n))::date`, because `date + interval` is a timestamp in PostgreSQL. Other types get `+ make_interval(…)`, milliseconds and microseconds `+ n * interval '1 millisecond'`. A number that is not known to be an integer becomes `trunc(n)::integer` (T-SQL truncates toward zero), a bigint `(n)::integer` (out of int range is an error on both). Every rewrite carries `ASM-DATEADD-RANGE` (T-SQL raises an error past 9999, or past the datetime/smalldatetime range; PostgreSQL returns the date), years and quarters `ASM-DATEADD-YEAR-END` and times `ASM-DATEADD-TIME-WRAP` (both not on Microsoft's page). Refused with a reason: an unknown date type, a string date (read under DATEFORMAT), what T-SQL itself rejects (`hour` on a `date`, `day` on a `time`, `mcs`/`ns` on `date`/`datetime`/`smalldatetime`), nanoseconds (100 ns rounding), seconds and milliseconds on `smalldatetime`, milliseconds on `datetime` (1/300 s rounding), fractions finer than the type keeps, days or longer on `datetimeoffset` (timestamptz follows the session time zone), and a string number.
- Metadata: `sp_addextendedproperty` with literal arguments → `COMMENT ON SCHEMA/TABLE/VIEW/PROCEDURE/FUNCTION/COLUMN/INDEX/CONSTRAINT` (`ASM-EXTPROP-COMMENT`: the property name is dropped and there is one comment per object). Db2 for i: schemas, tables, views, routines and columns only, since an index lives in the current schema there.
- Keys: `PRIMARY KEY (a ASC)` and `UNIQUE (a DESC)` lose the sort order, which neither target takes in a key's column list; a dropped `DESC` is noted (`tsql.key-order`: only the key's index is built ascending).
- System-versioned tables (PostgreSQL): `PERIOD FOR SYSTEM_TIME` with `SYSTEM_VERSIONING = ON (HISTORY_TABLE = h)` becomes the table plus a BEFORE row trigger function `t_versioning` (`ASM-TEMPORAL-TRIGGER`). INSERT and UPDATE stamp the period columns with `transaction_timestamp()` in UTC and the end of time at the column's precision (`9999-12-31 23:59:59.999999` for `DATETIME2(7)`); UPDATE and DELETE first copy the old row into `h`, also for an UPDATE that changes nothing and for rows written earlier in the same transaction. The statements SQL Server rejects fail too: an explicit period value on INSERT (the columns default to `-infinity`, which no T-SQL value converts to; `DEFAULT` is allowed, as in T-SQL), a period column in an UPDATE's SET list (`BEFORE UPDATE OF`), `TRUNCATE`, and changing a row that a later transaction already changed (Msg 13535). With the tree's catalog the history table must have the same columns by name and type in the same order (else refused with SQL Server's Msg 13523/13524); when the tree has none the output creates it with `CREATE TABLE h (LIKE t)`, as SQL Server creates it; without a catalog that is `ASM-TEMPORAL-HISTORY`. Not reproduced, and said in the note: direct writes to `h` (SQL Server refuses them), schema changes that would need `h` and the trigger to follow, and an INSERT without a column list that leaves the period columns out. Refused: `HIDDEN` period columns, no `HISTORY_TABLE`, a finite `HISTORY_RETENTION_PERIOD`, `SYSTEM_VERSIONING = OFF`, memory-optimized tables, every `ALTER TABLE … SYSTEM_VERSIONING / PERIOD FOR SYSTEM_TIME`, and queries with `FOR SYSTEM_TIME`. A literal `'9999-12-31 23:59:59.9999999'` (the DATETIME2(7) end of time) is marked as differing (`tsql.end-of-time-literal`): PostgreSQL rounds it up to 10000-01-01, so it no longer matches the current rows' end. The rule was checked by running the same DML on SQL Server 2022 and PostgreSQL 18 (same history rows, same failing statements). The engine pairs the trigger's history INSERT with the T-SQL table's declared history write.
- Parameters may be written `@p AS int`.
- CTEs: a recursive CTE gets `WITH RECURSIVE` (`ASM-CTE-MAXRECURSION`: T-SQL stops past its recursion limit, 100 levels by default, PostgreSQL does not). A WITH list stays in front of INSERT, and of UPDATE and DELETE on a real table; a statement that changes a CTE (INSERT included: T-SQL then changes its base table), UPDATE or DELETE with a FROM clause, and MERGE are refused, and so is a recursive list where a query names a later CTE (RECURSIVE would make that name the CTE, where T-SQL reads the table).

`DELETE … FROM` with joins is converted only when the FROM list merely filters the target: every join is an inner or cross join (or a comma), and the target names exactly one FROM table, by its alias or by the one-part name of an unaliased table. Then T-SQL deletes each target row for which at least one combination of the other tables meets all ON and WHERE conditions, once however many match, and that is what `USING` and `EXISTS` do. Outer joins (other than the `LEFT JOIN … IS NULL` idiom), `APPLY`, join hints, nested `ON`, `OPTION (…)`, a target that is no FROM table (`DELETE FROM t FROM u …`) and a target that would bind by a schema-qualified or base-table name to an aliased table (`DELETE FROM dbo.t FROM dbo.t AS x …`) stay not converted. On Db2 for i the conditions move into the `EXISTS` subquery, where an unqualified column resolves to the inner tables first, while T-SQL rejects a column that two FROM tables have (Msg 209); so there every unqualified column must belong to exactly one table by the source catalog (without one, it must be qualified), and conditions with a subquery are refused. The engine's equivalence lowering knows the same semi-join shape on all three dialects, so it checks these rewrites itself rather than reporting them as unsupported. `DELETE TOP (n)` (which rows is unspecified) and `OUTPUT` stay not converted.

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
- **Queries.** `TOP n` → `FETCH FIRST n ROWS ONLY`; a `SELECT` without `FROM` gets `FROM SYSIBM.SYSDUMMY1`, in views and CTEs too, and the output check flags any that is left (`residue.db2i.select-without-from`); `SELECT @a = … FROM` → `SELECT … INTO` (Db2 raises 21000 on several rows); a table-valued function in FROM, `dbo.f(1) x`, becomes `TABLE(f(1)) x`, the form IBM i documents for a table function reference, with `AS f` added when T-SQL had no alias. T-SQL's own table built-ins (`STRING_SPLIT`, `OPENJSON`, …) and `sys` functions are not converted, and the output check flags any bare call in FROM (`bind.table-function-form`). A data change statement with a WITH list (`WITH c AS (…) DELETE c FROM c JOIN …`) is refused: whether Db2 for i takes a WITH list there was not checked.
- **DDL.** Types map to Db2 (`BIT` → `SMALLINT`, `MONEY` → `DECIMAL(19, 4)`, `DATETIME2(n)` → `TIMESTAMP(n)`, `UNIQUEIDENTIFIER` → `CHAR(36)`, `(N)VARCHAR(MAX)` → `CLOB`/`NCLOB` as a decision, `DATETIMEOFFSET` refused). Column defaults must be constants or `CURRENT TIMESTAMP`, so `DEFAULT NEWID()` or `SYSUTCDATETIME()` is refused. Primary key columns get `NOT NULL`. Several `ADD COLUMN`s are written without commas.
- **System-versioned tables** are refused: Db2 for i has system-period temporal tables (`ROW BEGIN`/`ROW END`, `ADD VERSIONING`), but which clock their values read, their precision and how the history table's columns must match are not verified on IBM i yet.
- **Sequences.** As for PostgreSQL, every bound written out, with `NO CACHE` so values are handed out in order. `NEXT VALUE FOR` is kept (Db2 for i has it); as a column default it is still refused.
- **Built-ins.** `QUOTENAME(x)` → `'"' || REPLACE(x, '"', '""') || '"'` (`ASM-QUOTENAME-IDENT`), `RAND()` stays (`ASM-RAND-PER-QUERY`), `0x…` → `BX'…'` (`ASM-HEX-BINARY`). `GETDATE()` → `CURRENT TIMESTAMP` (per statement, V20), `GETUTCDATE()` → `CURRENT TIMESTAMP - CURRENT TIMEZONE`, `NEWID()` → `GENERATE_UUID()` (upper case, V13), `LEN` → `LENGTH(RTRIM(…))`, `CHARINDEX` → `LOCATE`, `SCOPE_IDENTITY()` → `IDENTITY_VAL_LOCAL()`, `STRING_AGG` → `LISTAGG`, `DATEADD` → labelled durations (`d + 5 DAYS`, `d - 5 DAYS`, quarters `(3 * n) MONTHS`, weeks `(7 * n) DAYS`, milliseconds `(CAST(n AS BIGINT) * 1000) MICROSECONDS`) with the same type rules and refusals as for PostgreSQL, plus fractions of a second on `time` (Db2 TIME has none) and every `datetimeoffset`; a number not known to be an integer becomes `CAST(n AS INTEGER)`. Month-end clamping (`ASM-DB2-MONTH-END`), time wrap-around (`ASM-DB2-TIME-WRAP`), the cast's truncation (`ASM-DB2-DATEADD-NUMBER`) and the 0001–9999 range error (`ASM-DATEADD-RANGE`) are fact V27, probed in `probes/db2i`, `DATEPART` → `YEAR()`…`SECOND()`, `DATEDIFF` day/month/year → `DAYS(b) - DAYS(a)` and year/month arithmetic. A cast to a number notes V9 unless the operand is a number.

The engine still reports what the rules cannot change: a T-SQL procedure without `XACT_ABORT ON` goes on after a failed statement while Db2 stops (`equiv.error-continues`), and the time source and case sensitivity findings land on their units. `tests/convert_db2.rs` converts the whole T-SQL corpus to Db2 for i and requires that it parses and leaves no T-SQL residue.

## Next (P1 and later)

Expression trees (from the equivalence work) and binding let rules see types, so `+`, comparisons and `ISNULL` stop needing assumptions; the behavioural IR (WP3) and more idiom rules bring `@@ERROR`/`@@ROWCOUNT` checks and transaction structure into scope, and a catalog (`Facts`) lets the idiom rules prove key uniqueness and identity columns instead of assuming them. For Db2 for i, the unverified vendor facts (V4, V9, V15) need a real IBM i before their assumptions can be dropped, and the idiom rules have no Db2 forms yet (MERGE and upserts are converted by the token rules instead).
