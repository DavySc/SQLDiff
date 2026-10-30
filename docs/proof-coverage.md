# Proof coverage

Generated from `crates/sqldiff-core/profiles/rule-proofs.toml`; do not edit. After changing the registry run `SQLDIFF_BLESS=1 cargo test -p sqldiff-core --test rule_proofs`.

Every conversion rule id the converter's notes and trace steps can carry (`tsql.*`, `pg.*`, `idiom.*`, `db2.*`) and every rule-file head (`translate`, `idiom`, `lint`), with what backs it (docs/verified-conversion.md, C0.9). The test that regenerates this file also scans the sources and fails when an id is used but not registered, registered but not used, or used in a file its entry does not list.

The Lean checker (`formal/lean/Sqldiff/Mirror/Lower.lean`) does not read rule ids: it re-lowers the output on disk, and `Sqldiff.Mirror.check_sound` (or `check_sound_c2a`) is about that lowering. A rule is **lemma**-backed when the construct it rewrites, in the form the converter writes it, is inside the lowering's spec; its citation names the section of `Lower.lean` whose rule reads that form, and the note says where the coverage is partial. **axiom** rests on a vendor fact of docs/vendor-facts-verification.md with no lemma. **engine** is backed only by the Rust engine comparison, the tests and the probes.

A lemma backs a rule, not a unit: a unit is confirmed only when the checker accepts its whole routine pair, and the conversion report's `proof` field says so per unit.

## Summary

| Kind | Rules | Lemma | Axiom | Engine |
|---|---|---|---|---|
| `converter` | 182 | 35 (19%) | 6 (3%) | 141 (77%) |
| `translate` | 15 | 3 (20%) | 1 (7%) | 11 (73%) |
| `idiom` | 10 | 0 (0%) | 1 (10%) | 9 (90%) |
| `lint` | 4 | 0 (0%) | 0 (0%) | 4 (100%) |
| **All** | 211 | 38 (18%) | 8 (4%) | 165 (78%) |

A family (`tsql.func.*`) counts as one rule; the members registered on their own count on their own.

## Registry

| Id | Kind | Proof | Note | Used in |
|---|---|---|---|---|
| `advisory-lock/applock` | `idiom` | axiom: V21 | sp_getapplock against pg_advisory_xact_lock (ASM-APPLOCK-CHECK-RAISES) | `sqldiff-core/profiles/rules/idioms.rules` |
| `cardinality/correlated-update-unrestricted` | `lint` | engine | a lint changes no output | `sqldiff-core/profiles/rules/lints.rules` |
| `charindex/strpos` | `translate` | engine |  | `sqldiff-core/profiles/rules/tsql-pg.rules` |
| `concurrency/insert-if-absent-race` | `lint` | engine | a lint changes no output | `sqldiff-core/profiles/rules/lints.rules` |
| `concurrency/upsert-race` | `lint` | engine | a lint changes no output | `sqldiff-core/profiles/rules/lints.rules` |
| `datediff-day/date-minus` | `translate` | engine |  | `sqldiff-core/profiles/rules/tsql-pg.rules` |
| `db2.lob` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `db2.merge` | `converter` | axiom: V2 | MERGE kept in a procedure compiled with *CS; in a script ASM-FACT-V2 | `sqldiff-convert/src/tsql.rs` |
| `db2.print` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `db2.proc-transaction` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `db2.sqlcode` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `db2.xact-abort` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `dbo-schema` | `translate` | engine | C3a maps a table's `dbo.t` through the deployment under ASM-DBO-SCHEMA | `sqldiff-core/profiles/rules/tsql-pg.rules` |
| `delete-orphans/anti-join` | `idiom` | engine |  | `sqldiff-core/profiles/rules/idioms.rules` |
| `idiom.rollback-rethrow` | `converter` | lemma: `Sqldiff.Mirror/C4 errors and transactions/The rollback-and-rethrow idiom` | PostgreSQL target, at the routine's top level around one transaction bracket (ASM-NO-CALLER-TXN); the Db2 for i `ROLLBACK; RESIGNAL` form engine | `sqldiff-convert/src/tsql.rs` |
| `iif/case` | `translate` | engine | T-SQL IIF is outside the slice; the searched CASE it becomes is in it | `sqldiff-core/profiles/rules/tsql-pg.rules` |
| `insert-identity/lastval` | `idiom` | engine | ASM-LASTVAL-SAME-INSERT | `sqldiff-core/profiles/rules/idioms.rules` |
| `insert-identity/returning` | `idiom` | engine |  | `sqldiff-core/profiles/rules/idioms.rules` |
| `insert-if-absent/if-not-exists` | `idiom` | engine |  | `sqldiff-core/profiles/rules/idioms.rules` |
| `insert-if-absent/on-conflict` | `idiom` | engine |  | `sqldiff-core/profiles/rules/idioms.rules` |
| `isnull/coalesce` | `translate` | lemma: `Sqldiff.Mirror/C2a operators/ISNULL` | int arguments (C2a), or a string variable or parameter with a fallback that fits it (C2b strings/ISNULL); other types engine (ASM-ISNULL-TYPE) | `sqldiff-core/profiles/rules/tsql-pg.rules` |
| `len/length` | `translate` | lemma: `Sqldiff.Mirror/C2b strings/LEN` | `LEN(x)` against `length(rtrim(x))` on a string of the slice (ASM-TSQL-LEN-BMP) | `sqldiff-core/profiles/rules/tsql-pg.rules` |
| `newid/gen-random-uuid` | `translate` | engine |  | `sqldiff-core/profiles/rules/tsql-pg.rules` |
| `nolock/plain-read` | `translate` | engine |  | `sqldiff-core/profiles/rules/tsql-pg.rules` |
| `pg.bit-number` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `pg.lastval` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `pg.name-collision` | `converter` | engine |  | `sqldiff-convert/src/instances.rs` |
| `pg.param-default-order` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `pg.reserved-name` | `converter` | engine |  | `sqldiff-convert/src/names.rs` |
| `pg.security-definer-transaction` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `pg.sqlstate` | `converter` | engine | ERROR_NUMBER() and SQLSTATE are outside the C4 spec wherever they stand ('Error functions') | `sqldiff-convert/src/tsql.rs` |
| `pg.table-variable-rollback` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `pg.trigger-per-event` | `converter` | engine |  | `sqldiff-convert/src/tsql/trigger.rs` |
| `plus/concat` | `translate` | lemma: `Sqldiff.Mirror/C2b strings/Concatenation` | both operands strings of the slice (ASM-TSQL-PLUS-IS-CONCAT) | `sqldiff-core/profiles/rules/tsql-pg.rules` |
| `select-assign/into` | `translate` | engine | `SELECT e INTO v FROM ...` is outside C3b except `COUNT(*)` into an int variable (ASM-EXACTLY-ONE-ROW) | `sqldiff-core/profiles/rules/tsql-pg.rules` |
| `select-assign2/into` | `translate` | engine | as select-assign/into | `sqldiff-core/profiles/rules/tsql-pg.rules` |
| `semantic/top-without-order` | `lint` | engine | a lint changes no output | `sqldiff-core/profiles/rules/lints.rules` |
| `string-agg/within-group` | `translate` | engine |  | `sqldiff-core/profiles/rules/tsql-pg.rules` |
| `sysdatetime/statement-timestamp` | `translate` | axiom: V20 | once per statement on both sides (ASM-TSQL-SYSDATETIME-PER-STATEMENT); the T-SQL part of V20 is partial | `sqldiff-core/profiles/rules/tsql-pg.rules` |
| `top-distinct/limit` | `translate` | engine |  | `sqldiff-core/profiles/rules/tsql-pg.rules` |
| `top/limit` | `translate` | engine |  | `sqldiff-core/profiles/rules/tsql-pg.rules` |
| `tsql.*` | `converter` | engine | T-SQL global variables (`@@ROWCOUNT` is `tsql.rowcount`) and built-ins without a rule (`OBJECT_ID()` is `tsql.object_id`), both refused with the reason | `sqldiff-convert/src/tsql.rs` |
| `tsql.bit.arithmetic` | `converter` | engine |  | `sqldiff-convert/src/tsql/bits.rs` |
| `tsql.bit.between` | `converter` | engine |  | `sqldiff-convert/src/tsql/bits.rs` |
| `tsql.bit.bitwise` | `converter` | engine |  | `sqldiff-convert/src/tsql/bits.rs` |
| `tsql.bit.boolean-default` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.bit.case-result` | `converter` | engine |  | `sqldiff-convert/src/tsql/bits.rs` |
| `tsql.bit.coalesce` | `converter` | engine |  | `sqldiff-convert/src/tsql/bits.rs` |
| `tsql.bit.compare` | `converter` | lemma: `Sqldiff.Mirror/C2c typed values/bit against boolean` | a bit variable or column against the literals 0/1 (TRUE/FALSE); comparisons with other numbers, strings or lists, BETWEEN and IN stay on the engine | `sqldiff-convert/src/tsql/bits.rs` |
| `tsql.bit.concat` | `converter` | engine |  | `sqldiff-convert/src/tsql/bits.rs` |
| `tsql.bit.in` | `converter` | engine |  | `sqldiff-convert/src/tsql/bits.rs` |
| `tsql.bit.not` | `converter` | engine |  | `sqldiff-convert/src/tsql/bits.rs` |
| `tsql.bit.store` | `converter` | lemma: `Sqldiff.Mirror/C2c typed values/bit against boolean` | the literals 0/1 (FALSE/TRUE) and NULL stored into a bit variable or column; a number or expression stored as (v <> 0), and CASE results, stay on the engine | `sqldiff-convert/src/tsql/bits.rs` |
| `tsql.builtin` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.catalog.exists` | `converter` | engine | a catalog existence test; `IF EXISTS (SELECT 1 FROM t ...)` on a plain table is C3b rule 3 and leaves no step | `sqldiff-convert/src/tsql.rs` |
| `tsql.charindex` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.convert-style` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.cte.recursive` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.ctl.if` | `converter` | lemma: `Sqldiff.Mirror/C2a operators/IF and WHILE` | a condition of the slice; `IF [NOT] EXISTS (...)` as the whole condition is C3b rule 3; other conditions are C1 | `sqldiff-convert/src/tsql.rs` |
| `tsql.ctl.try-catch` | `converter` | lemma: `Sqldiff.Mirror/C4 errors and transactions/TRY-CATCH against EXCEPTION WHEN OTHERS` | the `exception-block` form in a procedure, under ASM-TRY-SUBTRANSACTION and ASM-CATCH-CORE-ERRORS; the Db2 for i handler forms engine | `sqldiff-convert/src/tsql.rs` |
| `tsql.ctl.while` | `converter` | lemma: `Sqldiff.Mirror/C2a operators/IF and WHILE` | a condition of the slice; other conditions are C1 | `sqldiff-convert/src/tsql.rs` |
| `tsql.cursor.close` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.cursor.deallocate` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.cursor.declare` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.cursor.fetch` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.cursor.open` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.cursor.while` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.database.filegroup` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.datalength` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.dateadd` | `converter` | lemma: `Sqldiff.Mirror/C2c typed values/DATEADD` | day, month and year on a date or datetime2(p), p at most 6, with an int number (ASM-DATEADD-RANGE, ASM-DATEADD-YEAR-END); other dateparts, types and numbers stay on the engine | `sqldiff-convert/src/tsql.rs` |
| `tsql.datediff` | `converter` | lemma: `Sqldiff.Mirror/C2c typed values/DATEDIFF` | day only, on dates and datetime2(p), p at most 6; month and year stay on the engine | `sqldiff-convert/src/tsql.rs` |
| `tsql.decl.hoist` | `converter` | lemma: `Sqldiff.Mirror/Blocks/declTys` | the lowering gathers T-SQL DECLAREs where they stand and PL/pgSQL ones from the block header, so the move changes nothing it reads | `sqldiff-convert/src/tsql.rs` |
| `tsql.decl.init` | `converter` | lemma: `Sqldiff.Mirror/Statements` | `DECLARE @x t = e` as the assignment `x := e` after the declaration, `e` in the slice | `sqldiff-convert/src/tsql.rs` |
| `tsql.decl.table-variable` | `converter` | engine | PostgreSQL TEMP table engine; the Db2 for i NOT LOGGED table rests on V16 (ASM-TABLE-VARIABLE) | `sqldiff-convert/src/tsql.rs` |
| `tsql.decl.variable` | `converter` | lemma: `Sqldiff.Mirror/Blocks/declTys` | locals of a slice type (int, varchar(n), nvarchar(n)); other types are C1 (uninterpreted values) | `sqldiff-convert/src/tsql.rs` |
| `tsql.dml.delete-from` | `converter` | lemma: `Sqldiff.Mirror/C3a data statements/DELETE` | `DELETE t [WHERE p]` on a plain table (the step is the added FROM); joins (tsql.dml.delete-join) engine | `sqldiff-convert/src/tsql.rs` |
| `tsql.dml.delete-join` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.dml.insert-into` | `converter` | lemma: `Sqldiff.Mirror/C3a data statements/INSERT` | `INSERT t (cols) VALUES (...)` on a plain table (the step is the added INTO); INSERT ... SELECT, OUTPUT and other forms engine | `sqldiff-convert/src/tsql.rs` |
| `tsql.dml.output-into` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.dml.update-from` | `converter` | engine | UPDATE ... FROM with a join; a plain `UPDATE t SET` leaves no step and is C3a | `sqldiff-convert/src/tsql.rs` |
| `tsql.end-of-time-literal` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.exec.named-argument` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.expr.binary-concat` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.expr.binary-literal` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.expr.concat` | `converter` | lemma: `Sqldiff.Mirror/C2b strings/Concatenation` | both operands strings of the slice, `+` against `\|\|` (ASM-CONCAT-NULL); a string literal next to a number engine | `sqldiff-convert/src/tsql.rs` |
| `tsql.expr.datetime-literal` | `converter` | axiom: V33 | Db2 for i only (ASM-FACT-V33) | `sqldiff-convert/src/tsql.rs` |
| `tsql.expr.dbo-schema` | `converter` | engine | C3a maps a table's `dbo.t` through the deployment under ASM-DBO-SCHEMA; the step itself is not read | `sqldiff-convert/src/tsql.rs` |
| `tsql.expr.end-of-time-literal` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.expr.like-pattern` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.expr.literal-fraction` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.expr.module-setting` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.expr.next-value` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.expr.plus-numeric` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.expr.quoted-ident` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.expr.string-compare` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.expr.table-variable` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.expr.temp-table` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.expr.undeclared-variable` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.expr.unicode-literal` | `converter` | lemma: `Sqldiff.Mirror/C2b strings/Literals` | `N'...'` of printable ASCII without a backslash reads as the same literal as `'...'` | `sqldiff-convert/src/tsql.rs` |
| `tsql.expr.variable` | `converter` | lemma: `Sqldiff.Mirror/Expressions` | a variable read; the rename (`p_`/`v_` prefix) is the certificate's renaming, checked injective | `sqldiff-convert/src/tsql.rs` |
| `tsql.from.openjson` | `converter` | engine | OPENJSON ... WITH in FROM → a jsonb_array_elements derived table; the engine compares the column list (key, strict or lax, type) under ASM-OPENJSON-VALUES | `sqldiff-convert/src/tsql.rs` |
| `tsql.from.table-function` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.from.table-hint` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.func.*` | `converter` | engine | Every rewritten built-in call, named after the function (`tsql.func.getdate`); the members with a lemma are listed on their own | `sqldiff-convert/src/tsql.rs` |
| `tsql.func.current_timestamp` | `converter` | engine | a time source; Db2 for i rests on V20 (ASM-TIME-SOURCE) | `sqldiff-convert/src/tsql.rs` |
| `tsql.func.isnull` | `converter` | lemma: `Sqldiff.Mirror/C2a operators/ISNULL` | int arguments (C2a), or a string variable or parameter with a fallback that fits it (C2b strings/ISNULL); other types engine (ASM-ISNULL-TYPE) | `sqldiff-convert/src/tsql.rs` |
| `tsql.func.len` | `converter` | lemma: `Sqldiff.Mirror/C2b strings/LEN` | `LEN(x)` against `length(rtrim(x))` on a string variable, parameter or literal of the slice (ASM-TSQL-LEN-BMP) | `sqldiff-convert/src/tsql.rs` |
| `tsql.func.string_agg` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.geography.point-constant` | `converter` | engine | SQL Server point bytes to PostGIS EWKB; eval/geography shows the doubles bit-identical | `sqldiff-convert/src/tsql/geography.rs` |
| `tsql.global-temp` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.index.clustered` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.index.nulls-not-distinct` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.index.qualify` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.key-order` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.lock-hint` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.merge` | `converter` | engine | PostgreSQL MERGE needs 15 or later (V12); Db2 for i has no WHEN NOT MATCHED BY SOURCE (V1) | `sqldiff-convert/src/tsql.rs` |
| `tsql.merge.by-target` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.merge.into` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.nchar` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.openjson` | `converter` | engine | the note on an OPENJSON form that is not converted; it rewrites nothing | `sqldiff-convert/src/tsql.rs` |
| `tsql.order.nulls` | `converter` | axiom: V19 | NULLS FIRST/LAST after T-SQL's placement (ASM-FACT-V19); the Db2 for i part of V19 is unverified | `sqldiff-convert/src/tsql/paging.rs` |
| `tsql.partition-scheme` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.plan-hint` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.query-hints` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.routine-options` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs`<br>`sqldiff-convert/src/tsql/trigger.rs` |
| `tsql.routine-options-dropped` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs`<br>`sqldiff-convert/src/tsql/trigger.rs` |
| `tsql.routine.alter` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.routine.execute-as` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.routine.function` | `converter` | lemma: `Sqldiff.Mirror/Routines` | scalar functions; TRY/CATCH, THROW, transactions, data statements and queries are procedure-only in the spec | `sqldiff-convert/src/tsql.rs` |
| `tsql.routine.param` | `converter` | lemma: `Sqldiff.Mirror/Routines/lowerParam` | name, mode and a NULL or slice default; the types under 'Parameter and return types' | `sqldiff-convert/src/tsql.rs` |
| `tsql.routine.procedure` | `converter` | lemma: `Sqldiff.Mirror/Routines` | CREATE [OR ALTER] PROCEDURE against CREATE OR REPLACE PROCEDURE ... LANGUAGE plpgsql; the body is checked statement by statement | `sqldiff-convert/src/tsql.rs` |
| `tsql.routine.result-set` | `converter` | engine | the casts of a result set's columns to their declared types (RETURN QUERY needs exact types, checked on PostgreSQL 18.6) | `sqldiff-convert/src/tsql.rs` |
| `tsql.routine.result-set-function` | `converter` | engine | a procedure's one result set as a function RETURNS TABLE under ASM-RESULT-SET-FUNCTION | `sqldiff-convert/src/tsql.rs` |
| `tsql.routine.return-code` | `converter` | lemma: `Sqldiff.Mirror/T-SQL return status` | `INOUT return_code integer DEFAULT NULL` as the last parameter, the status set to 0 first | `sqldiff-convert/src/tsql.rs` |
| `tsql.routine.returns` | `converter` | lemma: `Sqldiff.Mirror/Parameter and return types` | int and slice string return types; others engine | `sqldiff-convert/src/tsql.rs` |
| `tsql.routine.view` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.rowcount.found` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.schema.authorization-dbo` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.select.assign` | `converter` | engine | `SELECT @x = e` without FROM as `x := e`; not read as an assignment by the Lean lowering | `sqldiff-convert/src/tsql.rs` |
| `tsql.select.assign-into` | `converter` | lemma: `Sqldiff.Mirror/C3b queries/rule 4` | `SELECT @x = COUNT(*) FROM t [WHERE c]` into an int variable only (`SELECT COUNT(*) INTO x`); every other SELECT ... INTO engine (ASM-SELECT-ASSIGN-CARDINALITY) | `sqldiff-convert/src/tsql.rs` |
| `tsql.select.assign-loop` | `converter` | lemma: `Sqldiff.Mirror/C3b queries/rule 1` | one plain table, a WHERE that fixes every primary-key column, int or fitting string targets, procedures only; other queries engine | `sqldiff-convert/src/tsql.rs` |
| `tsql.select.dummy-from` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.select.top` | `converter` | engine | PostgreSQL LIMIT engine; Db2 for i FETCH FIRST below the outer query rests on V32 (ASM-FACT-V32) | `sqldiff-convert/src/tsql/paging.rs` |
| `tsql.sequence` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.sequence-cache` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.server-metadata` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.sqlcmd-include` | `converter` | engine |  | `sqldiff-convert/src/lib.rs`<br>`sqldiff-convert/src/tsql.rs` |
| `tsql.stmt.break` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.stmt.continue` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.stmt.drop-trigger` | `converter` | engine |  | `sqldiff-convert/src/tsql/trigger.rs` |
| `tsql.stmt.dynamic-known-text` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.stmt.dynamic-parameters` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.stmt.exec` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.stmt.extended-property` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.stmt.grant` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.stmt.print` | `converter` | engine | a message is outside the spec (`Message`) | `sqldiff-convert/src/tsql.rs` |
| `tsql.stmt.raiserror` | `converter` | engine | RAISERROR is outside the C4 spec (V10: it does not honour XACT_ABORT) | `sqldiff-convert/src/tsql.rs` |
| `tsql.stmt.result-set` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.stmt.return` | `converter` | lemma: `Sqldiff.Mirror/T-SQL return status` | the `return-code` form in a procedure (`return_code := COALESCE(e, 0); RETURN;`); the trigger form (`RETURN NULL`) engine | `sqldiff-convert/src/tsql.rs` |
| `tsql.stmt.set-option` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.stmt.set-variable` | `converter` | lemma: `Sqldiff.Mirror/Statements` | `SET @x = e` with `e` in the slice (`x := e`), and `SET @x = (SELECT e FROM t WHERE c)` (C3b rule 2); compound operators (`+=`) engine | `sqldiff-convert/src/tsql.rs` |
| `tsql.stmt.sqlcmd-include` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.stmt.throw` | `converter` | lemma: `Sqldiff.Mirror/C4 errors and transactions/THROW and RAISE EXCEPTION` | a literal number from 50000, a literal message and state (`raise`), and `THROW;` directly in a handler (`rethrow`, 'Re-raising'); variables as arguments engine | `sqldiff-convert/src/tsql.rs` |
| `tsql.stmt.truncate` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.storage-options` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.table-builtin` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.table.add-column` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.table.add-default` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.table.check-constraint` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.table.column-default` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.table.column-option-dropped` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.table.columnstore` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.table.computed-column` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.table.computed-concat` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.table.computed-iso126` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.table.computed-json` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.table.identity` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.table.inline-index` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.table.primary-key-not-null` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.table.sequence-default-trigger` | `converter` | axiom: V34 | Db2 for i only (ASM-FACT-V34); the fact is unverified | `sqldiff-convert/src/tsql.rs` |
| `tsql.table.temp` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.table.with-check` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.temporal-history-created` | `converter` | engine |  | `sqldiff-convert/src/tsql/temporal.rs` |
| `tsql.temporal.period` | `converter` | engine |  | `sqldiff-convert/src/tsql/temporal.rs` |
| `tsql.temporal.period-column` | `converter` | engine |  | `sqldiff-convert/src/tsql/temporal.rs` |
| `tsql.temporal.switch` | `converter` | engine |  | `sqldiff-convert/src/tsql/temporal.rs` |
| `tsql.temporal.table` | `converter` | engine |  | `sqldiff-convert/src/tsql/temporal.rs` |
| `tsql.temporal.versioning-option` | `converter` | engine |  | `sqldiff-convert/src/tsql/temporal.rs` |
| `tsql.trigger` | `converter` | engine |  | `sqldiff-convert/src/tsql/trigger.rs` |
| `tsql.trigger.missing-image` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.trigger.rowcount-guard` | `converter` | engine |  | `sqldiff-convert/src/tsql/trigger.rs` |
| `tsql.txn.autocommit` | `converter` | lemma: `Sqldiff.Mirror/C4 errors and transactions/Transactions, PL/pgSQL` | the COMMIT added after a statement T-SQL commits on its own (ASM-NO-CALLER-TXN); the Lean rule names this id | `sqldiff-convert/src/tsql.rs` |
| `tsql.txn.begin` | `converter` | lemma: `Sqldiff.Mirror/C4 errors and transactions/Transactions, T-SQL` | as a bracket with its COMMIT in a procedure under SET XACT_ABORT ON, outside TRY, IF, WHILE and nested blocks (ASM-NO-CALLER-TXN, ASM-TSQL-DOOMED-TXN); elsewhere engine | `sqldiff-convert/src/tsql.rs` |
| `tsql.txn.commit` | `converter` | lemma: `Sqldiff.Mirror/C4 errors and transactions/Transactions, T-SQL` | as the end of a bracket (see tsql.txn.begin), the PL/pgSQL COMMIT under 'Transactions, PL/pgSQL'; elsewhere engine | `sqldiff-convert/src/tsql.rs` |
| `tsql.txn.rollback` | `converter` | engine | a ROLLBACK is outside the C4 spec except inside idiom.rollback-rethrow | `sqldiff-convert/src/tsql.rs` |
| `tsql.txn.rollback-savepoint` | `converter` | axiom: V14 | the note names V14 (ASM-FACT-V14) on every target; the fact is about Db2 for i commitment control | `sqldiff-convert/src/tsql.rs` |
| `tsql.txn.save` | `converter` | axiom: V14 | the note names V14 (ASM-FACT-V14) on every target; the fact is about Db2 for i commitment control | `sqldiff-convert/src/tsql.rs` |
| `tsql.txn.trancount-guard` | `converter` | engine | the `IF @@TRANCOUNT > 0` guard outside idiom.rollback-rethrow | `sqldiff-convert/src/tsql.rs` |
| `tsql.txn.wrapper` | `converter` | engine | the call is one transaction: outside the C4 bracket shape | `sqldiff-convert/src/tsql.rs` |
| `tsql.type.*` | `converter` | engine | The type mapping, named after the T-SQL type (`tsql.type.bigint`); the members with a lemma are listed on their own | `sqldiff-convert/src/tsql.rs` |
| `tsql.type.int` | `converter` | lemma: `Sqldiff.Mirror/C2a types` | `int` against PostgreSQL `integer` (also 'Parameter and return types') | `sqldiff-convert/src/tsql.rs` |
| `tsql.type.integer` | `converter` | lemma: `Sqldiff.Mirror/C2a types` | `integer` against PostgreSQL `integer` (also 'Parameter and return types') | `sqldiff-convert/src/tsql.rs` |
| `tsql.type.nvarchar` | `converter` | lemma: `Sqldiff.Mirror/C2b strings/Types` | `nvarchar(n)` with 1 <= n <= 4000 against `varchar(n)` (read with the `wide` bound); `max` and `nchar(n)` engine | `sqldiff-convert/src/tsql.rs` |
| `tsql.type.user-defined` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.type.varchar` | `converter` | lemma: `Sqldiff.Mirror/C2b strings/Types` | `varchar(n)` with 1 <= n <= 4000 against `varchar(n)`; `max`, no length and `char(n)` engine | `sqldiff-convert/src/tsql.rs` |
| `tsql.unique-nulls` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `tsql.view-options` | `converter` | engine |  | `sqldiff-convert/src/tsql.rs` |
| `upsert/if-exists` | `idiom` | engine |  | `sqldiff-core/profiles/rules/idioms.rules` |
| `upsert/if-exists-locked` | `idiom` | engine |  | `sqldiff-core/profiles/rules/idioms.rules` |
| `upsert/merge` | `idiom` | engine |  | `sqldiff-core/profiles/rules/idioms.rules` |
| `upsert/on-conflict` | `idiom` | engine |  | `sqldiff-core/profiles/rules/idioms.rules` |

## Checker refusal reasons

A lemma-backed rule does not make a unit confirmed: the checker takes the whole routine pair and refuses it when any construct of either side is outside the slice, with one reason of the fixed taxonomy in [formal/lean/README.md](../formal/lean/README.md). The conversion report's per-unit `proof` field carries that reason, and the per-corpus histogram of reasons says which gaps of the slice matter most. The registry above answers the other question: for a rule the converter applied, whether the checker could read its output at all.
