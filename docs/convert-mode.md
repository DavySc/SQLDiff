# Convert mode (P0)

`sqldiff-gui convert [FILE]` opens a two-pane page: T-SQL you can edit on the left, PostgreSQL on the right. The right side is regenerated about 150 ms after you stop typing (debounced; stale responses are dropped). A sample is shown when no file is given; nothing is ever written to disk. `--stdout` prints the conversion and the per-statement badges instead of serving the page.

Only T-SQL → PostgreSQL has rules so far. The feasibility study that led here is the "Live conversion feasibility" doc in the project; this is its phase P0.

## What "correct" means here

Every converted unit (a statement, a compound header such as `IF … THEN`, a routine header, or a hoisted `DECLARE`) carries a status, best to worst:

| Status | Meaning |
|---|---|
| converted | Rules that need no assumption, and the engine found nothing in this unit |
| under assumptions | Equal if every listed assumption holds (each has a stable id such as `ASM-CI-COLLATION`) |
| needs a decision | Several faithful targets exist; the note says which one was emitted and the alternative |
| differs | The engine's diff of input against output found a difference, leftover T-SQL, or the output does not parse as PostgreSQL |
| not converted | No rule applies; the original is kept as a commented block with the reason |

The status is the worst of the rule notes and of the engine's own check (`checked`). The engine check runs the normal `sqldiff diff` of the input (side A) against the output (side B) and attaches each finding to the unit it points into, so engine findings such as `error.contract` or `write.unpaired` show up next to the statement that caused them. Today the cross-dialect engine cannot prove statement equality, so the best a unit can get is "converted": tested rules, no assumption, nothing found. "Proven" needs the equivalence and IR work.

A rule never guesses: if any part of a statement has no rule (for example `@@ERROR`, `UPDATE … FROM` with a join, `DATEDIFF(week, …)`), the whole statement is commented out and marked not converted.

### Idiom rules

Before a statement is commented out, it goes to the idiom rules (`sqldiff_core::rules`, see [idiom-rules.md](idiom-rules.md)), which recognise whole patterns rather than tokens: `MERGE` and `IF EXISTS` upserts → `INSERT … ON CONFLICT`, `IF NOT EXISTS … INSERT` → `ON CONFLICT DO NOTHING`, the `LEFT JOIN … IS NULL` delete → `NOT EXISTS`, `INSERT` + `SCOPE_IDENTITY()` → `RETURNING … INTO` or `lastval()`, and `sp_getapplock` with its return-code check → `pg_advisory_xact_lock`. Their output replaces the comment when they convert the statement completely, with their notes (and an `idiom-rules` note naming the rules) on the unit. Patterns that span statements (`INSERT` + `SCOPE_IDENTITY`, `sp_getapplock` + its check) become one unit, since the token rules cannot see them as a pair.

Two limits keep this honest. Statements the token rules already convert keep their conversion (a plain `IF EXISTS … UPDATE … ELSE INSERT` stays an `IF`, with the race intact, rather than becoming an atomic `ON CONFLICT`). And a statement refused because of where it stands (a result set in a procedure, `COMMIT` inside a TRY block) is never handed to the rules, which see statements out of context. The rules never check themselves: their output goes through the same engine diff as everything else.

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
- Expressions: `[x]` → `x` (or `"x"`), `dbo.` dropped, `N'…'` → `'…'`, string `+` → `||`, `ISNULL`, `LEN`, `CHARINDEX`, `NEWID`, `IIF`, `GETDATE`/`SYSDATETIME`/`GETUTCDATE`, `CAST`/`CONVERT` types, `DATEADD`, `DATEPART`, `YEAR/MONTH/DAY`, `DATEDIFF` for day/month/year (boundary counts, as T-SQL defines them), `STRING_AGG … WITHIN GROUP`, `REPLICATE`, `SCOPE_IDENTITY`, `ERROR_MESSAGE`/`ERROR_NUMBER` in handlers, table hints.

The tests in `crates/sqldiff-convert/tests/convert.rs` convert every T-SQL file in the corpus and require that the output parses as PostgreSQL and carries no T-SQL residue outside commented-out units.

## Next (P1 and later)

Expression trees (from the equivalence work) and binding let rules see types, so `+`, comparisons and `ISNULL` stop needing assumptions; the behavioural IR (WP3) and more idiom rules bring `@@ERROR`/`@@ROWCOUNT` checks and transaction structure into scope, and a catalog (`Facts`) lets the idiom rules prove key uniqueness and identity columns instead of assuming them. Db2 for i output follows the same pattern once the PostgreSQL rules are proven against the oracle containers.
