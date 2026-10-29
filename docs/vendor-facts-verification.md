# Vendor facts verification (design doc section 13, V1 to V26)

Checked against official vendor documentation on 2026-10-23. Target versions are **SQL Server 2022** and **PostgreSQL 18**. Db2 for i was checked against the IBM i 7.4 to 7.6 docs.

## Target versions

- **PostgreSQL 18:** links point to `/docs/18/`. Everything below is available in 18. Features that are newer than some deployed servers: `MERGE` (15+), `WHEN NOT MATCHED BY SOURCE`, MERGE `RETURNING` and `merge_action()` (17+), `uuidv7()`/`uuidv4()` and `RETURNING OLD/NEW` (18+). A dialect profile for an older PostgreSQL must gate these.
- **SQL Server 2022:** links point to Microsoft Learn with `?view=sql-server-ver16` (the 2022 view). Microsoft keeps one page for all versions, and the pages read for V10, V11, V13, V19, V20 and V21 show no version-specific text, so the behaviour is the same in 2022 and the latest release. `ORDER BY … NULLS FIRST/LAST` is not in the 2022 syntax.

## Status legend

| Status | Meaning |
|---|---|
| **Confirmed** | The vendor doc states it; the claim holds as written. |
| **Corrected** | The vendor doc contradicts or materially refines the claim. Read the note before implementing. |
| **Partial** | Part of the claim is documented; the rest is inference or undocumented. |
| **Unverified** | The vendor page could not be read (IBM Documentation refused most automated fetches) or the vendor does not document it. Needs a test on a real system before the lowering rule is implemented. |

## Summary

| ID | Topic | Status | Headline |
|---|---|---|---|
| V1 | Db2 i MERGE, no `NOT MATCHED BY SOURCE` | Confirmed | Only `WHEN NOT MATCHED`, and its action can only be `INSERT` or `SIGNAL`. |
| V2 | Db2 i MERGE `NOT ATOMIC CONTINUE ON SQLEXCEPTION` | **Corrected** | Supported, but `ATOMIC` is ignored under No Commit, so under `*NONE` every MERGE is effectively partial. |
| V3 | Db2 i MERGE, several source rows per target | Partial | Documented as forbidden; the SQLSTATE is not on the page. |
| V4 | ATOMIC compound and UNDO under `*NONE` | Unverified | Inference: nothing is rolled back under `*NONE`. Needs a test. |
| V5 | Default commit level of SQL routines | Partial | Taken from the options in effect at create time; per-interface defaults not confirmed. |
| V6 | Activation group and commitment definition | Confirmed | `ACTGRP(*CALLER)`; commitment definition scoped to the activation group by default. |
| V7 | DGTT `ON COMMIT` default and `NOT LOGGED` | **Corrected** | `ON COMMIT DELETE ROWS` default confirmed; `NOT LOGGED` also defaults to `ON ROLLBACK DELETE ROWS`. |
| V8 | Blank padding in Db2 i comparisons | Confirmed | Shorter string padded with blanks; VARCHARs differing only in trailing blanks are equal. |
| V9 | `CAST('' AS INT)` on Db2 i | Unverified | Casting page could not be read. |
| V10 | T-SQL errors that abort the batch with `XACT_ABORT OFF` | **Corrected** | Microsoft publishes no exact list. Must be a configurable pack list plus tests. |
| V11 | Statements that reset `@@ROWCOUNT` | Confirmed | Documented list below; it is "examples", not exhaustive. |
| V12 | PostgreSQL MERGE versions | Confirmed | 15: `MERGE`. 17: `NOT MATCHED BY SOURCE`, `RETURNING`, `merge_action()`. 18 adds `RETURNING OLD/NEW`. |
| V13 | UUID and UTC helpers | Partial | All v4 by default; Db2 i returns UPPERCASE text, PostgreSQL lowercase. |
| V14 | SAVEPOINT in Db2 compound statements | Partial | Needs commitment control; ATOMIC opens a new savepoint level. |
| V15 | `WITH RS USE AND KEEP EXCLUSIVE LOCKS` on `SELECT INTO` | Unverified | Isolation clause allowed on `SELECT INTO`; lock-request rules not readable. |
| V16 | ROLLBACK on NOT LOGGED DGTT vs table variables | **Corrected** | Not equivalent by default: the DGTT is emptied on rollback unless `ON ROLLBACK PRESERVE ROWS`. |
| V17 | PostgreSQL `COMMIT` in procedures | Confirmed | Rules listed below. |
| V18 | `WHEN OTHERS` exclusions | Confirmed | `QUERY_CANCELED` and `ASSERT_FAILURE`. |
| V19 | NULL ordering | Partial | PG: NULLs high. T-SQL: NULLs low. Db2 i: page not readable. |
| V20 | Time functions | Confirmed (PG, Db2 i), Partial (T-SQL) | Db2 i `CURRENT TIMESTAMP` is per statement, PG `now()` per transaction. |
| V21 | Advisory locks vs app locks | Confirmed | Mapping and differences below. |
| V22 | Deferrable constraints | Confirmed | When each error surfaces, below. |
| V23 | `plpgsql.variable_conflict` | Confirmed | Default `error`. |
| V24 | Volatility and strictness | Confirmed | Defaults `VOLATILE` and `CALLED ON NULL INPUT`. |
| V25 | Routine security context | Confirmed (T-SQL, PG), Unverified (Db2 i) | Owner vs caller privileges per dialect; Db2 i default `USRPRF *NAMING` needs a test. |
| V26 | Db2 i CTE scoping and recursion | Unverified | Forward reference to a later CTE, and a recursive CTE without a column list: probes in `probes/db2i`. |
| V27 | Db2 i labeled durations as the DATEADD rewrite | Unverified | Month-end clamping and warning 01506, TIME wrap-around, `CAST(… AS INTEGER)` truncation, BIGINT microsecond durations, the 9999 range error: probes `V27-*` in `probes/db2i`. |
| V29 | Db2 i unique indexes, index names, sequence defaults | Unverified | NULLs, case and trailing blanks in a UNIQUE index; an index named like a table; the default type and first value of a sequence: probes `V29-*` in `probes/db2i`. |
| V32 | Db2 i ORDER BY and FETCH FIRST below the outer query | Unverified | FETCH FIRST n ROWS ONLY and ORDER BY in a derived table, IN and scalar subquery, CTE (and INSERT … SELECT, view), and a NULL row count: probes `V32-*` in `probes/db2i`. |
| V33 | Db2 i date and timestamp strings | Partial | ISO `yyyy-mm-dd` dates, ISO and IBM SQL timestamps (0 to 12 fraction digits, padded or truncated to the precision) documented; T-SQL's compact `yyyymmdd` is not a Db2 i date or timestamp string. That an assignment reads them as a CAST does is inferred: probes `V33-*` in `probes/db2i`. |
| V34 | Db2 i BEFORE INSERT trigger as a sequence default | Unverified | A NOT NULL column left out of an INSERT, filled by a `WHEN (N.c IS NULL) SET N.c = NEXT VALUE FOR s` trigger before the NOT NULL check, per row, for INSERT … SELECT and MERGE, no value taken when the INSERT gives one; a written NULL and the DEFAULT keyword: probes `V34-*` in `probes/db2i`. |

Items that change implementation decisions: **V2, V7/V16, V10, V13 (UUID case), V20 (Db2 time source)**.

## Details

### V1. Db2 for i `MERGE` has no `WHEN NOT MATCHED BY SOURCE` — Confirmed

The syntax has only `WHEN NOT MATCHED [AND search-condition]`, meaning rows where the ON condition is false or unknown, and only `INSERT` or a `SIGNAL` statement may follow `THEN`. There is no `BY SOURCE` or `BY TARGET`. A `NOT MATCHED BY SOURCE … DELETE` must therefore be lowered to a separate `DELETE … WHERE NOT EXISTS`, which changes atomicity (see V2).

Source: [IBM i 7.5 MERGE](https://www.ibm.com/docs/en/i/7.5.0?topic=statements-merge), [IBM i 7.4 MERGE](https://www.ibm.com/support/knowledgecenter/ssw_ibm_i_74/db2/rbafzmerge.htm)

### V2. `NOT ATOMIC CONTINUE ON SQLEXCEPTION` — Corrected (important refinement)

- `ATOMIC` (default): an error in any update, delete or insert operation rolls back the entire MERGE.
- `NOT ATOMIC`: only the failing operation is rolled back. "The row being processed at the time of the error is not inserted, updated, or deleted." "Any other changes previously made during the processing of the MERGE statement are not rolled back." `CONTINUE ON SQLEXCEPTION` goes on to the next row; `STOP ON SQLEXCEPTION` stops, keeping earlier changes.
- **New fact:** "ATOMIC is only honored when the statement is run with an isolation level other than No Commit." Under `COMMIT(*NONE)` / `WITH NC`, an `ATOMIC` MERGE behaves like `NOT ATOMIC STOP ON SQLEXCEPTION`: rows processed before the error stay changed. The lowering and the diff must treat MERGE under `*NONE` as non-atomic.
- The page itself has an inconsistency ("CONTINUE ON EXCEPTION" in one note); treat it as a doc typo.
- Lock note: under `*RR/*ALL/*CS/*CHG` one MERGE can lock at most 500,000,000 rows (a *Limit* lint candidate).

Source: [IBM i 7.5 MERGE](https://www.ibm.com/docs/en/i/7.5.0?topic=statements-merge)

### V3. Several source rows on one target — Partial

Documented rule: "Each row in the target can only be operated on once", and a target row "can only be identified as MATCHED with one row in the result table of the table-reference." So it is an error, as in PostgreSQL and SQL Server. The SQLSTATE/SQLCODE is **not** stated on the page (Db2 family usage suggests 21506; unverified). Unknown: whether it is detected before or during the run, which matters under `NOT ATOMIC` and `*NONE` (partial application). Needs a test.

For comparison, PostgreSQL: a repeated `UPDATE`/`DELETE` raises a cardinality violation and a repeated `INSERT` raises a unique violation ([PG MERGE](https://www.postgresql.org/docs/18/sql-merge.html)).

Source: [IBM i 7.5 MERGE](https://www.ibm.com/docs/en/i/7.5.0?topic=statements-merge)

### V4. ATOMIC compound and UNDO handler under `*NONE` — Unverified

Documented (compound dynamic statement): `ATOMIC` means an unhandled exception rolls the compound statement back; `UNDO` requires `ATOMIC`; an UNDO handler rolls back the changes made by the compound statement and then continues at its end; `COMMIT` and `ROLLBACK` (except `ROLLBACK TO SAVEPOINT`) are not allowed inside ATOMIC. ([7.3 compound (dynamic)](https://www.ibm.com/docs/ca/ssw_ibm_i_73/db2/rbafzcompounddynstmt.htm))

Not found in readable docs: what ATOMIC and UNDO do under `*NONE`. Strong inference: changes made without commitment control cannot be rolled back ([commitment control](https://www.ibm.com/docs/ssw_ibm_i_74/sqlp/rbafydicomm.htm)), and MERGE explicitly ignores ATOMIC under No Commit (V2), so an ATOMIC block or UNDO handler under `*NONE` very likely leaves earlier changes in place, or the create/run is rejected. **Test on a real system:** an ATOMIC procedure with `SET OPTION COMMIT = *NONE` that inserts and then fails, with and without an UNDO handler. Page to read by hand: compound statement in the [7.5 SQL Reference PDF](https://www.ibm.com/docs/en/ssw_ibm_i_75/pdf/rbafzpdf.pdf) (SQL control statements chapter).

### V5. Default commit level for SQL routines per creation interface — Partial

Documented:
- `CREATE PROCEDURE (SQL)`: "The default values for the options depend on the options in effect at create time." So the commit level compiled into the routine comes from the creating environment unless `SET OPTION COMMIT = …` is in the source. ([7.5 CREATE PROCEDURE (SQL)](https://www.ibm.com/docs/en/i/7.5.0?topic=statements-create-procedure-sql))
- The default lock level comes from the `COMMIT` parameter of `CRTSQLxxx`, `STRSQL` or `RUNSQLSTM`. ([commitment control](https://www.ibm.com/docs/ssw_ibm_i_74/sqlp/rbafydicomm.htm), [RUNSQLSTM commitment control](https://www.ibm.com/docs/zh/ssw_ibm_i_73/sqlp/rbafyccsqlstmt.htm))
- IBM i Access ODBC: by default runs autocommit with the `*NONE` isolation level unless `TRUEAUTOCOMMIT=1`. ([ODBC isolation considerations](https://www.ibm.com/support/knowledgecenter/ssw_ibm_i_71/rzaik/rzaikcommitcontrol.htm))

Not confirmed (pages unreadable): the default `COMMIT` value of each command (commonly `*CHG` for `RUNSQLSTM`/`CRTSQLxxx`, `*NONE` for `STRSQL`) and of ACS Run SQL Scripts / JDBC. **Rule for the tool:** never assume; read `SET OPTION COMMIT` from source or `QSYS2.SYSPROCS`/program attributes, otherwise record it in the assumption ledger.

### V6. Activation group and commitment definition scope — Confirmed

- SQL procedures are created with `ACTGRP(*CALLER)` ("When called, the SQL procedure runs in the activation group of the calling program"); `AUTONOMOUS` procedures use `ACTGRP(QSQAUTOAG)`. `COMMIT ON RETURN` defaults to `NO`. ([7.5 CREATE PROCEDURE (SQL)](https://www.ibm.com/docs/en/i/7.5.0?topic=statements-create-procedure-sql))
- Db2 for i starts commitment control with `CMTSCOPE(*ACTGRP)` by default; a job-level commitment definition is used if one was started with `CMTSCOPE(*JOB)`. Normal end of an activation group commits; abnormal end rolls back (unless `*JOB`). ([commitment control](https://www.ibm.com/docs/ssw_ibm_i_74/sqlp/rbafydicomm.htm))
- Uncommitted work of a dynamic compound statement "is scoped to the activation group of the connection". ([7.3 compound (dynamic)](https://www.ibm.com/docs/ca/ssw_ibm_i_73/db2/rbafzcompounddynstmt.htm))

### V7. Declared global temporary tables — Corrected (refinement)

- `ON COMMIT` default: "The default is DELETE ROWS." Confirmed.
- `NOT LOGGED`: "Changes to the table are not logged, including creation of the table." On rollback "the changes are not rolled back", **but** `ON ROLLBACK DELETE ROWS` is the default: "All rows of the table will be deleted." `ON ROLLBACK PRESERVE ROWS` keeps them. If the table was created in the rolled-back unit of work it is dropped; if dropped in it, it is recreated empty.
- `ON ROLLBACK` "does not apply if the declared temporary table was opened under isolation level No Commit (NC) or if a ROLLBACK HOLD operation is performed."
- Lifetime: dropped when the application process (on IBM i, the job) ends. A second `DECLARE` fails without `WITH REPLACE`.
- Not confirmed: whether `NOT LOGGED` is the default when neither `LOGGED` nor `NOT LOGGED` is written (the page section read did not state it). Check by hand before relying on it.

Source: [7.5 DECLARE GLOBAL TEMPORARY TABLE](https://www.ibm.com/support/knowledgecenter/ssw_ibm_i_75/db2/rbafzdgtt.htm)

### V8. Blank-padding of VARCHAR comparisons — Confirmed

"A temporary copy of the shorter string is padded on the right with blanks before comparison." "Two varying-length strings with different lengths are equal if they differ only in the number of trailing blanks." DISTINCT, MIN/MAX, UNION, EXCEPT, INTERSECT may return any one of such equal values. Same as SQL Server's ANSI padding for `=`; different from PostgreSQL `varchar`/`text` (trailing blanks significant; only `char(n)` ignores them). Not covered: `LIKE` (not padded in the Db2 family; unverified for Db2 i).

Source: [String comparisons (Db2 for i)](https://www.ibm.com/docs/pl/ssw_ibm_i_71/db2/rbafzstringcomp.htm)

### V9. `CAST('' AS INT)` on Db2 for i — Unverified

The casting pages ([7.6 Casting data types](https://www.ibm.com/docs/en/i/7.6.0?topic=statement-casting-data-types)) could not be read. Expected from the Db2 family: leading and trailing blanks are ignored, and an empty or all-blank string raises SQLSTATE 22018 (invalid character value). In SQL Server `CAST('' AS int)` returns `0`; in PostgreSQL `''::int` raises `22P02`. **Test on a real system** before writing the lowering rule.

### V10. T-SQL errors that abort the batch with `XACT_ABORT OFF` — Corrected

Microsoft does not publish an exact list. What the docs do say:
- "When SET XACT_ABORT is OFF, in some cases only the Transact-SQL statement that raised the error is rolled back and the transaction continues processing. Depending upon the severity of the error, the entire transaction may be rolled back even when SET XACT_ABORT is OFF." Default is OFF in batches, ON in triggers.
- "Compile errors, such as syntax errors, are not affected by SET XACT_ABORT."
- "When ANSI_WARNINGS=OFF, permissions violations cause transactions to abort."
- `THROW` honours `XACT_ABORT`; `RAISERROR` does not.
- TRY…CATCH does not catch severity ≤ 10, severity ≥ 20 errors that end the connection, attentions, `KILL`, and at the same execution level compile errors, statement-level recompilation errors and deferred name resolution errors (these are caught one level down, for example inside a called procedure or `sp_executesql`).
- An error can make the transaction uncommittable (`XACT_STATE() = -1`); only reads and `ROLLBACK` are then allowed, and the engine rolls it back at batch end.

**Rule for the tool:** keep batch-aborting errors as a configurable list in a pack, seeded from known classes (deadlock 1205, conversion errors, compile/name-resolution errors, severity ≥ 20), and back it with empirical tests per server version. Results against such errors should be "equal under assumptions".

Sources: [SET XACT_ABORT](https://learn.microsoft.com/sql/t-sql/statements/set-xact-abort-transact-sql?view=sql-server-ver16), [TRY…CATCH](https://learn.microsoft.com/en-us/sql/t-sql/language-elements/try-catch-transact-sql?view=sql-server-ver16)

### V11. Statements that reset `@@ROWCOUNT` — Confirmed

Documented (given as examples, not as an exhaustive list):
- Reset to 0: `USE`, `SET <option>`, `DEALLOCATE CURSOR`, `CLOSE CURSOR`, `PRINT`, `RAISERROR`, `BEGIN TRANSACTION`, `COMMIT TRANSACTION`.
- Set to 1: simple assignments (`SET @v = …`, `RETURN`, `READTEXT`, `SELECT GETDATE()`, `SELECT 'text'`), `DECLARE CURSOR`, `FETCH`.
- Set to rows affected or read: DML, and query assignments such as `SELECT @v = c1 FROM t1`.
- Preserved: `EXECUTE`; natively compiled procedures.

Note: an `IF` or a `SET NOCOUNT ON` between the DML and the `@@ROWCOUNT` check is a common conversion trap (`SET` resets it). `ROLLBACK`, `SAVE TRAN` and `IF` are not on the page; treat them as unknown and test.

Source: [@@ROWCOUNT](https://learn.microsoft.com/en-us/sql/t-sql/functions/rowcount-transact-sql?view=sql-server-ver16)

### V12. PostgreSQL MERGE versions — Confirmed

- 15: "Add SQL MERGE command" ([PG 15 release notes](https://www.postgresql.org/docs/15/release-15.html)).
- 17: `WHEN NOT MATCHED BY SOURCE`, `RETURNING` in MERGE, `merge_action()`, MERGE on updatable views ([PG 17 release notes](https://www.postgresql.org/docs/17/release-17.html)).
- 18 (addition): `RETURNING OLD/NEW` aliases for INSERT/UPDATE/DELETE/MERGE ([PG 18 release notes](https://www.postgresql.org/docs/18/release-18.html)).
- Concurrency: MERGE follows ordinary isolation rules; the docs point to `INSERT … ON CONFLICT` when concurrent inserts must turn into updates ([PG MERGE](https://www.postgresql.org/docs/18/sql-merge.html)).

### V13. UUID generation and UTC helpers — Partial

| | PostgreSQL | SQL Server | Db2 for i |
|---|---|---|---|
| Random UUID | `gen_random_uuid()` (v4, core since 13); `uuidv4()` alias (18) | `NEWID()` "compliant with RFC4122" (version not named) | `GENERATE_UUID()` → `CHAR(36)`, v4, **uppercase**; `GENERATE_UUID_BINARY()` |
| Time-ordered UUID | `uuidv7()` (18) | `NEWSEQUENTIALID()` (default constraints only; not checked here) | none found |
| Text form | lowercase | uppercase when cast to string | uppercase |

Conversion traps: case of UUID text (string comparison and joins across systems), and type (`uuid` vs `uniqueidentifier` vs `CHAR(36)`/binary). The IBM support page lists `GENERATE_UUID` for 7.4 and 7.5 via a Technology Refresh; the exact TR is not stated.

UTC: PostgreSQL `timestamp AT TIME ZONE z` interprets the value as local time in `z` and returns `timestamptz`; `timestamptz AT TIME ZONE z` returns the wall-clock time in `z`. SQL Server `SYSDATETIME()` is server local time (`datetime2(7)`). Db2 for i UTC helpers (`CURRENT TIMEZONE` arithmetic) not checked.

Sources: [PG UUID functions](https://www.postgresql.org/docs/18/functions-uuid.html), [PG 13 notes](https://www.postgresql.org/docs/13/release-13.html), [PG 18 notes](https://www.postgresql.org/docs/18/release-18.html), [PG date/time](https://www.postgresql.org/docs/18/functions-datetime.html), [NEWID](https://learn.microsoft.com/en-us/sql/t-sql/functions/newid-transact-sql?view=sql-server-ver16), [GENERATE_UUID (7.6)](https://www.ibm.com/support/knowledgecenter/ssw_ibm_i_76/db2/rbafzscagenerateuuid.htm), [IBM support: GENERATE_UUID](https://www.ibm.com/support/pages/generateuuid-and-generateuuidbinary-scalar-functions)

### V14. SAVEPOINT inside Db2 compound statements — Partial

- `RELEASE SAVEPOINT` "is not allowed if commitment control is not active for the activation group" ([7.5 RELEASE SAVEPOINT](https://www.ibm.com/support/knowledgecenter/ssw_ibm_i_75/db2/rbafzrlssvpt.htm)). The `SAVEPOINT` statement page could not be read; the same requirement is very likely.
- Savepoint levels: a new level starts at the `BEGIN` of an ATOMIC compound statement and ends at its `END`; triggers and `NEW SAVEPOINT LEVEL` procedures also open a level. Savepoints can only be referenced inside their own level and are released when it ends; COMMIT/ROLLBACK release all savepoints ([savepoints (SQL programming)](https://www.ibm.com/docs/it/ssw_ibm_i_71/sqlp/rbafysavepoint.htm)).
- In an ATOMIC compound (dynamic) statement, `COMMIT` and `ROLLBACK` are not allowed, but `ROLLBACK TO SAVEPOINT` is ([7.3 compound (dynamic)](https://www.ibm.com/docs/ca/ssw_ibm_i_73/db2/rbafzcompounddynstmt.htm)).

Consequence: a T-SQL `SAVE TRAN` inside a procedure cannot be lowered to a savepoint when the Db2 routine runs under `*NONE`, and a savepoint set inside an ATOMIC block cannot be targeted from outside it.

### V15. `SELECT … WITH RS USE AND KEEP EXCLUSIVE LOCKS` on `SELECT INTO` — Unverified

Readable doc: the isolation clause can be specified on `DELETE`, `INSERT`, `SELECT INTO`, `UPDATE` and select-statements, and applies only to that statement; every level except No Commit holds exclusive locks on changed rows until the end of the unit of work ([7.3 isolation level](https://www.ibm.com/support/knowledgecenter/en/ssw_ibm_i_73/db2/rbafzisol.htm)). The lock-request clause rules (only after `RS`/`RR`? accepted on `SELECT INTO`?) could not be read (`rbafzisocl.htm`, `rbafzhselin.htm`). Read the isolation-clause and SELECT INTO sections of the [7.5 SQL Reference PDF](https://www.ibm.com/docs/en/ssw_ibm_i_75/pdf/rbafzpdf.pdf) by hand, and test that the lock is held under `*CHG` and that it is a no-op or an error under `*NONE`.

### V16. ROLLBACK on a NOT LOGGED DGTT vs T-SQL table variables — Corrected

- T-SQL: "transaction rollbacks don't affect" table variables ([table data type](https://learn.microsoft.com/en-us/sql/t-sql/data-types/table-transact-sql?view=sql-server-ver16)).
- Db2 for i NOT LOGGED DGTT: changes are not rolled back, but with the default `ON ROLLBACK DELETE ROWS` all rows are deleted on rollback. Only `NOT LOGGED ON ROLLBACK PRESERVE ROWS` matches table-variable behaviour. Under No Commit the ON ROLLBACK clause does not apply. ([7.5 DGTT](https://www.ibm.com/support/knowledgecenter/ssw_ibm_i_75/db2/rbafzdgtt.htm))
- PostgreSQL has no equivalent: temporary tables are transactional.

The lowering of table variables to DGTTs must emit `NOT LOGGED ON ROLLBACK PRESERVE ROWS` (and `ON COMMIT PRESERVE ROWS`).

### V17. PostgreSQL `COMMIT` in procedures — Confirmed

- Allowed in procedures called by `CALL` and in `DO` blocks; a new transaction starts automatically.
- Only when every frame from the top level is `CALL` or `DO`: `CALL p1()` → `CALL p2()` → `CALL p3()` may commit, `CALL p1()` → `SELECT f()` → `CALL p3()` may not. So a procedure called from a function never can.
- "If CALL is executed in a transaction block, then the called procedure cannot execute transaction control statements." (explicit `BEGIN … CALL …` fails).
- Not inside a block with an `EXCEPTION` clause (it is a subtransaction). PL/pgSQL has no `SAVEPOINT` statements.
- Not in cursor loops driven by non-read-only commands; read-only cursor loops are converted to holdable cursors at the first COMMIT, and their locks are released.
- `COMMIT AND CHAIN` / `ROLLBACK AND CHAIN` keep transaction characteristics.
- `SECURITY DEFINER` and `SET` clauses on the procedure block transaction control: "A `SECURITY DEFINER` procedure cannot execute transaction control statements (for example, `COMMIT` and `ROLLBACK`, depending on the language)", and the same sentence for a `SET` clause ([CREATE PROCEDURE](https://www.postgresql.org/docs/18/sql-createprocedure.html), checked 2026-10-24).

Sources: [PL/pgSQL transaction management](https://www.postgresql.org/docs/18/plpgsql-transactions.html), [CALL](https://www.postgresql.org/docs/18/sql-call.html)

### V18. What `EXCEPTION WHEN OTHERS` does not catch — Confirmed

"The special condition name OTHERS matches every error type except QUERY_CANCELED and ASSERT_FAILURE." When an error is caught, local variables keep their values but "all changes to persistent database state within the block are rolled back." FATAL/PANIC errors end the session and are never catchable (general PostgreSQL behaviour, not on this page).

Source: [PL/pgSQL trapping errors](https://www.postgresql.org/docs/18/plpgsql-control-structures.html)

### V19. Default NULL ordering — Partial

- PostgreSQL: "null values sort as if larger than any non-null value": `NULLS LAST` for `ASC`, `NULLS FIRST` for `DESC`. `NULLS FIRST/LAST` supported. ([PG ORDER BY](https://www.postgresql.org/docs/18/queries-order.html))
- SQL Server: "NULL values are treated as the lowest possible values": first in `ASC`, last in `DESC`. No `NULLS FIRST/LAST` syntax; use a `CASE` sort key. ([ORDER BY clause](https://learn.microsoft.com/en-us/sql/t-sql/queries/select-order-by-clause-transact-sql?view=sql-server-ver16))
- Db2 for i: page not readable. Db2 family behaviour is NULL as the highest value (same as PostgreSQL), with `NULLS FIRST/LAST` available. Confirm in the ORDER BY section of the [7.5 SQL Reference PDF](https://www.ibm.com/docs/en/ssw_ibm_i_75/pdf/rbafzpdf.pdf). Also note that a sort sequence (`SRTSEQ`) can change character ordering on Db2 for i.

### V20. Time sources — Confirmed (PostgreSQL, Db2 for i), Partial (SQL Server)

| Function | Fixed for |
|---|---|
| PG `now()`, `CURRENT_TIMESTAMP`, `transaction_timestamp()` | the transaction (start time) |
| PG `statement_timestamp()` | the statement (receipt of the client message) |
| PG `clock_timestamp()` | nothing; changes within a statement |
| Db2 i `CURRENT TIMESTAMP`, `NOW()` | the statement: "all values are based on a single clock reading" |
| SQL Server `SYSDATETIME()` | not documented; nondeterministic, server local time, `datetime2(7)` |

Consequence: Db2 `CURRENT TIMESTAMP` → PG `statement_timestamp()`, not `now()`, when a routine runs several statements in one transaction. SQL Server's per-query runtime-constant evaluation is widely observed but not documented, so it is an assumption.

Sources: [PG date/time functions](https://www.postgresql.org/docs/18/functions-datetime.html), [Db2 for i NOW](https://www.ibm.com/support/knowledgecenter/ssw_ibm_i_71/db2/rbafzscanow.htm), [SYSDATETIME](https://learn.microsoft.com/en-us/sql/t-sql/functions/sysdatetime-transact-sql?view=sql-server-ver16)

### V21. PostgreSQL advisory locks vs T-SQL application locks — Confirmed

| | PostgreSQL advisory locks | SQL Server `sp_getapplock` |
|---|---|---|
| Transaction scope | `pg_advisory_xact_lock*`; released at end of transaction; no manual unlock | `@LockOwner = 'Transaction'` (default); must be in a transaction; released at commit or rollback |
| Session scope | `pg_advisory_lock*`; held until unlocked or session end; **ignores rollback** (a lock taken in a rolled-back transaction stays held) | `@LockOwner = 'Session'`; released by `sp_releaseapplock` or logout |
| Re-entrancy | stacks; one unlock per lock | stacks; one release per request |
| Key | one `bigint` or two `int`; "these two key spaces do not overlap" | `nvarchar(255)` name, binary (case-sensitive) compare, qualified by database id |
| Modes | shared, exclusive | Shared, Update, IntentShared, IntentExclusive, Exclusive |
| No wait | `pg_try_advisory_*` | `@LockTimeout = 0` |
| Deadlock | normal deadlock error | "doesn't roll back the transaction that requested the application lock"; check the return code (-3) |

Lowering a name to a key needs a stable hash, and a collision risk must be recorded as an assumption.

Sources: [PG advisory locks](https://www.postgresql.org/docs/18/explicit-locking.html), [PG advisory lock functions](https://www.postgresql.org/docs/18/functions-admin.html), [sp_getapplock](https://learn.microsoft.com/en-us/sql/relational-databases/system-stored-procedures/sp-getapplock-transact-sql?view=sql-server-ver16)

### V22. Deferrable constraints: when each error surfaces — Confirmed

- `NOT NULL` and `CHECK`: never deferrable; checked "immediately when a row is inserted or modified (not at the end of the statement)".
- Non-deferrable `UNIQUE`/`PRIMARY KEY`/`EXCLUDE`: checked immediately, row by row, "not at end of statement as the standard would suggest" (so `UPDATE t SET id = id + 1` can fail where SQL Server succeeds).
- `DEFERRABLE INITIALLY IMMEDIATE` (and foreign keys that are not deferred): checked at the end of each statement.
- `DEFERRED`: checked at `COMMIT`, so the error surfaces at COMMIT (outside the routine's exception block). `SET CONSTRAINTS … IMMEDIATE` forces the check at that point and fails if violated.
- Only `UNIQUE`, `PRIMARY KEY`, `REFERENCES` and `EXCLUDE` can be deferrable.

Source: [SET CONSTRAINTS](https://www.postgresql.org/docs/18/sql-set-constraints.html)

### V23. `plpgsql.variable_conflict` — Confirmed

Values `error` (factory default), `use_variable`, `use_column`. Per function: `#variable_conflict …` at the start of the body overrides it. With `error`, a name that could be a variable or a column is a compile-time error; `use_variable` matches pre-9.0 PostgreSQL; `use_column` matches Oracle-like systems. Substitution happens only in SELECT/INSERT/UPDATE/DELETE/MERGE (and commands containing them), never in `EXECUTE` strings. Changing the global setting needs superuser and affects only later compilations.

Source: [PL/pgSQL under the hood](https://www.postgresql.org/docs/18/plpgsql-implementation.html)

### V24. Volatility and strictness — Confirmed

- Default volatility: `VOLATILE`. `STABLE` and `IMMUTABLE` "use a snapshot established as of the start of the calling query", so they do not see changes made by the calling statement; `VOLATILE` functions take a fresh snapshot per query. Mislabelled `IMMUTABLE` functions can be folded at plan time (stale values in cached PL/pgSQL plans); functions with side effects must be `VOLATILE`.
- Strictness default: `CALLED ON NULL INPUT`. `STRICT` (= `RETURNS NULL ON NULL INPUT`) skips the call and returns NULL if any argument is NULL, so a body that would have raised or returned a non-NULL value for NULL input silently returns NULL.
- Conversion implications: T-SQL scalar UDFs and Db2 `DETERMINISTIC`/`NOT DETERMINISTIC` and `RETURNS NULL ON NULL INPUT` map onto these, but the snapshot rule has no direct T-SQL equivalent.

Sources: [Function volatility](https://www.postgresql.org/docs/18/xfunc-volatility.html), [CREATE FUNCTION](https://www.postgresql.org/docs/18/sql-createfunction.html)

### V25. Routine security context — Confirmed (SQL Server, PostgreSQL), Unverified (Db2 for i)

- T-SQL: `EXECUTE AS CALLER` is the default; `OWNER` runs the module's statements as its owner, `SELF` as whoever created or last altered it, `'user'` as that user. Under `CALLER`, permission checks are skipped along an ownership chain (module and objects with the same owner, static SQL only), so a caller with only `EXECUTE` can still use the tables.
- PostgreSQL: `SECURITY INVOKER` is the default; `SECURITY DEFINER` runs "with the privileges of the user that owns it". There is no ownership chaining. Unqualified names still resolve through the caller's `search_path` unless the routine has `SET search_path`.
- Db2 for i (to verify on a system): `SET OPTION USRPRF = *OWNER` adopts the owner's authority in addition to the caller's, `*USER` uses the caller's; the default `*NAMING` is documented as `*OWNER` under SQL naming and `*USER` under system naming. `DYNUSRPRF` applies the same choice to dynamic SQL. The engine never relies on the default: a Db2 routine without `USRPRF` compares as the caller's authority only under `ASM-DB2-USRPRF-USER`, and against an owner's context it stays unknown. The converter writes `USRPRF` and `DYNUSRPRF` out.

Probe for Davy's IBM i: create a procedure without `SET OPTION USRPRF` under SQL naming and one under system naming, both reading a table the caller has no authority to; call each as a user that holds only `*EXECUTE` on the program.

### V26. Common table expressions on Db2 for i — Unverified

Two forms that the T-SQL to Db2 for i converter passes through unchanged, raised in the CTE conversion review:

- A CTE that names a later CTE of the same `WITH` list (`A AS (SELECT x FROM b), b AS (...)`) when a table `b` also exists: does `A` read the later CTE, the table, or is it an error? Asked with and without a recursive CTE earlier in the list.
- A recursive CTE without a column list (`WITH r AS (SELECT 1 AS n ... UNION ALL SELECT n + 1 FROM r ...)`): accepted, or is the column list required?

No readable IBM i page settles either, so both are open questions with no expectation (probes `V26-*` in [probes/db2i](../probes/db2i/README.md)).

### V27. Labeled durations as the rewrite of DATEADD — Unverified

T-SQL `DATEADD` becomes Db2 for i labeled-duration arithmetic (`d + n DAYS`, `(3 * n) MONTHS`, `(CAST(n AS BIGINT) * 1000) MICROSECONDS`). The IBM i date, time and timestamp arithmetic pages could not be read (IBM Documentation refused the automated fetches), so what the converter relies on is held as assumptions until a run answers the `V27-*` probes in [probes/db2i](../probes/db2i/README.md):

- Adding or subtracting months or years moves a day the result month lacks to its last day (2024-01-31 + 1 MONTHS = 2024-02-29; 2024-02-29 + 1 MONTHS = 2024-03-29, not 03-31), with warning 01506 (`ASM-DB2-MONTH-END`).
- TIME arithmetic wraps around midnight (`ASM-DB2-TIME-WRAP`).
- `CAST(1.9 AS INTEGER)` is 1 and `CAST(-1.9 AS INTEGER)` is -1, for decimals and floats (`ASM-DB2-DATEADD-NUMBER`); out of INTEGER range is an error.
- A BIGINT microsecond duration is accepted and exact; `DATE('9999-12-31') + 1 DAYS` is an error (`ASM-DATEADD-RANGE`).

The T-SQL side was checked on Microsoft's [DATEADD page](https://learn.microsoft.com/sql/t-sql/functions/dateadd-transact-sql): dateparts and abbreviations, truncation of a fractional number, the int range, the string-literal and same-type return rules, the month-end rule for `month`, the smalldatetime and nanosecond rounding rules, and the range error. The page does not cover years or quarters at month end, or `time` wrapping around midnight; the converter states both as assumptions (`ASM-DATEADD-YEAR-END`, `ASM-DATEADD-TIME-WRAP`).

### V29. Unique indexes, index names and sequence defaults on Db2 for i — Unverified

What converted indexes and sequences rely on (`equiv/ddl.rs`, "Indexes, sequences and schemas" in [cross-dialect-proofs.md](cross-dialect-proofs.md)). SQL Server 2022 and PostgreSQL 18 were checked on real systems; Db2 for i was not, and no IBM i page was read for these, so the engine relies on none of them:

- Does a `UNIQUE` index hold only one row with a NULL key, as T-SQL's does, and does `UNIQUE WHERE NOT NULL` hold any number? Until answered, a Db2 for i unique index is not proven.
- Does a unique `VARCHAR` key accept `'A'` next to `'a'` under the job's sort sequence, and `'a '` next to `'a'`?
- Is an index named like a table of its schema rejected (`ASM-INDEX-NAME-SCHEMA`)?
- Which type and first value does a sequence without options have? The converter writes every value out and `NO CACHE`, and the engine compares a Db2 for i sequence only when it does.
- A schema name longer than 10 characters: its library gets a generated system name (not probed, since the probes create nothing outside `SQLDFPROBE`); such schemas are not proven.

Probes `V29-*` in [probes/db2i](../probes/db2i/README.md).

### V33. Date and timestamp strings on Db2 for i — Partial

T-SQL assigns compact literals to date and datetime2 variables (`DECLARE @d datetime2(7) = '20200101'`, `@EndOfTime = '99991231 23:59:59.9999999'`, 143 such literals in WideWorldImporters). Read on 2026-10-29 in the IBM i 7.6 [Timestamp strings](https://www.ibm.com/docs/en/ssw_ibm_i_76/db2/rbafztsstrings.htm) and IBM i 7.4 [Date strings](https://ibm.com/docs/th/ssw_ibm_i_74/db2/rbafzdatestrings.htm) pages of the Db2 for i SQL Reference (the 7.5 pages refused the fetch):

- A timestamp string "starts with a digit and has a length of at least 14 characters", in the ISO form `yyyy-mm-dd hh:mm:ss.nnnnnnnnnnnn`, the IBM SQL form `yyyy-mm-dd-hh.mm.ss.nnnnnnnnnnnn`, or 14 to 26 digits without separators. "The number of digits of fractional seconds can vary from 0 to 12"; the separator before them may be left out with them. In a cast, "Digits in the string beyond the timestamp precision of the cast are truncated" and missing ones "are assumed to be zeros". Leading zeros may be left out of month, day, hour, minute and second; trailing blanks are allowed.
- A date string "starts with a digit and has a length of at least 6 characters", in ISO or JIS `yyyy-mm-dd`, USA `mm/dd/yyyy`, EUR `dd.mm.yyyy`, the job's default format (DATFMT/DATSEP, two-digit years), or the unformatted Julian `yyyyddd`.
- So T-SQL's `'20200101'` is neither a Db2 for i date string (8 digits: no listed form) nor a timestamp string (shorter than 14), and `'99991231 23:59:59.9999999'` is not a timestamp string (a space, but no dashes). A Db2 for i output that kept them failed at run time.
- Not documented on these pages, and inferred: that assigning a string to a DATE or TIMESTAMP variable or column reads it as the cast does (the "Datetime assignments" page could not be read). Held as `ASM-FACT-V33`.

The converter writes a date or date-and-time literal assigned to a Db2 for i variable in the ISO form of the value T-SQL stores (`'2020-01-01 00:00:00'`, `'9999-12-31 23:59:59.9999999'` into TIMESTAMP(7)), only when T-SQL stores it exactly. The engine (`equiv/literal.rs`) reads only two-digit ISO dates and two-digit ISO or IBM SQL timestamps with at most the target's fraction digits, in a CAST or an assignment, never in a comparison. Probes `V33-*` in [probes/db2i](../probes/db2i/README.md).

### V34. A BEFORE INSERT trigger as the conversion of a sequence default — Unverified

Db2 for i takes no `NEXT VALUE FOR` in a column default, so the converter writes a T-SQL `DEFAULT (NEXT VALUE FOR s)` on a NOT NULL column as a `NO CASCADE BEFORE INSERT … REFERENCING NEW AS N FOR EACH ROW MODE DB2SQL WHEN (N.c IS NULL) SET N.c = NEXT VALUE FOR s` trigger (`ASM-FACT-V34`). The IBM i CREATE TRIGGER page could not be read (IBM Documentation refused the fetch), so these stay open until a run answers the `V34-*` probes in [probes/db2i](../probes/db2i/README.md):

- An INSERT that leaves the column out (it has no default) reaches the trigger, and the NOT NULL check comes after the trigger sets it.
- One value per row of a multi-row INSERT and of INSERT … SELECT; the trigger fires for MERGE's INSERT too.
- An INSERT that gives the column takes no sequence value.
- Open, no expectation: an INSERT that writes NULL into the column (T-SQL refuses it; the trigger would fill it, as `ASM-SEQUENCE-DEFAULT-TRIGGER` states) and one that writes `DEFAULT`.

### OPENJSON and PostgreSQL jsonb — Confirmed (SQL Server, PostgreSQL)

The PostgreSQL conversion of `OPENJSON(@v) WITH (col type ['[lax|strict] $.key'], ...)` (see `canon::pg_json_rows`) was checked on SQL Server 2022 and PostgreSQL 18.6 on 2026-10-29. The probes and their output are in [eval/openjson](../eval/openjson/): `ms*.sql` for SQL Server (`sqlcmd -I -W`), `pg*.sql` for PostgreSQL (`psql -At`).

- **Rows.** OPENJSON reads an object as one row, except `{}`, which has none. It reads an array as one row per element, also for `{}`, scalar and array elements. NULL gives no rows. A scalar document, a string document and `''` are errors. `jsonb_array_elements` over `CASE jsonb_typeof(j) WHEN 'object' THEN CASE j WHEN '{}' THEN '[]' ELSE jsonb_build_array(j) END ELSE j END` gives the same rows and the same errors. One exception: SQL Server accepts text after a complete array (`[{"a":1}] x`), and PostgreSQL rejects it.
- **Paths.**
  - A lax path (the default) reads NULL where the key is missing or the element is not an object; so does `e ->> 'key'`.
  - A strict path fails on a missing key or a non-object element, and reads NULL for JSON null; so does `jsonb_path_query_first(e, 'strict $."key"') #>> '{}'`.
  - Keys are case-sensitive on both. The default path is `$.` followed by the column name as written.
  - Where the two differ, the conversion relies on `ASM-OPENJSON-VALUES`:
    - On an object or array value, OPENJSON reads NULL (lax) or fails (strict), while PostgreSQL returns its text.
    - With duplicate keys, SQL Server takes the first and jsonb the last.
    - jsonb rewrites numbers (`1e2` reads as `100`).
- **Types.** A column's value converts like `CONVERT(type, text)` in SQL Server and like `CAST(text AS type)` in PostgreSQL. They agree on plain values:
  - Strings into nvarchar(n) / varchar(n) are cut silently. Each side counts in its own units (UTF-16 code units, characters), which are the same within the BMP. Trailing blanks are kept.
  - Integers convert alike, and overflow is an error on both. Both reject a fraction (`12.7`, `12.0`).
  - Decimals round half away from zero (0.125 → 0.13, -0.125 → -0.13), and overflow is an error on both.
  - `yyyy-mm-dd` dates convert alike from 0001-01-01 to 9999-12-31.
  - For bit / boolean, `true`, `false`, `1` and `0` convert alike.

  Where they differ:
  - `''` is 0, 1900-01-01 or 0 in SQL Server, and an error in PostgreSQL.
  - The bit `2` is 1 in SQL Server, and an error as a boolean in PostgreSQL.
  - Exponents are not accepted the same way.
  - varbinary decodes base64; there is no conversion for it.

## Follow-up

IBM Documentation refused most automated fetches, so V4, V9, V15 and parts of V3, V5, V7, V14 and V19 need either a manual read of the [7.5 SQL Reference PDF](https://www.ibm.com/docs/en/ssw_ibm_i_75/pdf/rbafzpdf.pdf) or, better, a small probe script run on an IBM i system. These probes belong in the per-dialect conformance tests (design doc section 12).

The probe script and its expected outcomes are in [probes/db2i](../probes/db2i/README.md). Results submitted from a real IBM i are judged there, and `cargo test` keeps the hedged assumption texts in step with the result (`probes/db2i/facts.toml`). Until a run is submitted, every fact above keeps its status.
