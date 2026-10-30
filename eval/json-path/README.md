# JSON_QUERY and JSON_VALUE probes

These probes check the PostgreSQL rule for a computed column `JSON_QUERY(col, '$.key')` or `JSON_VALUE(col, '$.key')`, `TRY_CONVERT(datetime2, …, 126)` of one, and ISJSON in a CHECK: `computed_json`, `computed_iso126` in `crates/sqldiff-convert/src/tsql.rs` and `pg_json_path` / `json_path` in `crates/sqldiff-core/src/equiv/canon.rs`. They were run on SQL Server 2022 and PostgreSQL 18.6 on 2026-10-29 (`cases-mssql`) and 2026-10-30 (the rest).

The PostgreSQL form uses the `json` type, not `jsonb`, because `json` keeps the text as written:

- JSON_QUERY: `CASE WHEN json_typeof(CAST(col AS json) -> 'key') IN ('object', 'array') THEN CAST(CAST(col AS json) -> 'key' AS text) END`
- JSON_VALUE: `CASE WHEN json_typeof(CAST(col AS json) -> 'key') NOT IN ('object', 'array') THEN CAST(col AS json) ->> 'key' END`

Files:

- `cases.txt`: 32 JSON texts, one per line. `cases-mssql.sql` (made by the burndown's `gen_ms.py`) reads each with ISJSON, JSON_QUERY(x, '$.Tags'), JSON_VALUE(x, '$.R') and a style-126 TRY_CONVERT. `cases-pg.sql` reads the same texts with the forms above. Outputs are hex: `python3 cmp.py cases.txt cases-mssql.out cases-pg.out q,v` lists the differences.
- `edges-mssql.sql`: escaped keys, 4,000 and 4,001 characters, a 9,000-character fragment, empty and blank documents, a quoted path, and the types SQL Server gives a computed column.
- `concat-types-mssql.sql`: the types SQL Server gives computed CONCAT columns (lengths add up, `max` wins, nvarchar when one part is), which `catalog::ddl::computed_call_type` uses for the history-table check.
- `table-mssql.sql` and `table-pg.sql`: the converted generated columns against SQL Server's computed columns on the same rows, and PostgreSQL's reading of escaped keys.

- `iso126-cases.txt` (1,447 strings: the burndown's 1,113 date texts and generated ISO forms), `iso126-mssql.sql` / `iso126-pg.sql` (TRY_CONVERT(datetime2(7), s, 126) against `canon::ISO126_PG` in a generated column) and `python3 iso126-cmp.py`, which sorts the differences.
- `invoices-mssql.sql` and `invoices-pg.sql`: WWI's Sales.Invoices columns and CHECK against the converted table, on WWI's own `DeliveredWhen` form (a trailing '.'), an ISO time, NULL, an invalid date, an object, and a scalar document the CHECK refuses.

What the runs show:

- Same on both: values and fragments for objects and arrays, string escapes, number text as written (`12.50e1`, `1E400`, `-0`), fragment whitespace, `true`, `null`, a missing key, keys compared case-sensitively, and NULL for the other kind of value (an object for JSON_VALUE, a scalar for JSON_QUERY). A 9,000-character fragment comes back whole on both.
- Different (17 of 64 readings in `cases`, and the edges):
  - duplicate keys: SQL Server reads the first, `json ->` the last;
  - a `\u0000` or unpaired surrogate escape anywhere: SQL Server reads the value, PostgreSQL raises an error;
  - a top-level scalar (`1`, `"str"`, `null`): SQL Server raises an error, PostgreSQL gives NULL;
  - invalid JSON that SQL Server reads far enough (`{"Tags":[1]}trailing`, a trailing comma, a missing `}`): SQL Server gives the value, PostgreSQL raises an error;
  - a key written with escapes (`{"Tags":[1]}`): SQL Server does not match it to `$.Tags`, PostgreSQL does;
  - JSON_VALUE of a string over 4,000 characters: NULL in SQL Server; the PostgreSQL column is varchar(4000), so the write fails.

`ASM-JSON-PATH-VALUES` excludes exactly these. For TRY_CONVERT style 126: every string of the form yyyy-mm-ddThh:mi:ss[.f] reads the same, a 7th fraction digit aside (ASM-FRACTIONAL-SECONDS); 144 other forms SQL Server reads become NULL (ASM-ISO126-VALUES). If you change the rule, add a probe here and run it on both engines first.
