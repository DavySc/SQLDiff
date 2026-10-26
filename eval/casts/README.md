# Cast and literal-type probes

These are the engine checks behind the "casts and implicit conversions" section of `crates/sqldiff-core/src/equiv/canon.rs`, the literal types in `Canon::num_lit`, the cast explanations in `equiv/compare.rs` (`explain_conv`) and the converter's `trunc()` rewrite in `crates/sqldiff-convert/src/tsql/bits.rs` (`cast_pg`). They were run on SQL Server 2022 and PostgreSQL 18 on 2026-10-26.

- `mssql.sql` / `postgres.sql`: explicit casts. Decimal, float, money and strings to integers and decimals; numbers and times to text; literal types (`1234567e0`, `2147483648`, `0x1F`, `N'..'`, `$12.34`); date and time casts; integer and decimal arithmetic; comparisons that mix types. Outputs in `mssql.out` and `postgres.out`.
- `assign-mssql.sql` / `assign-postgres.sql`: the same conversions when a value is stored in a column, assigned to a variable or passed to a parameter.
- `extra-mssql.sql` / `extra-postgres.sql`: decimal precision caps, timestamp and time rounding (before 2000 and at midnight), float text, tinyint, `ROUND(x, 0, 1)` and `trunc`.

Run the SQL Server files with `sqlcmd -I -W -h -1 -s"|"` and the PostgreSQL files with `psql -At`. Some lines fail on purpose.

What the runs show:

- To an integer, SQL Server drops a decimal's or float's fraction (1.5 → 1, -1.5 → -1, also on INSERT, UPDATE, SET and parameters). PostgreSQL rounds numeric half away from zero (1.5 → 2, -2.5 → -3) and float half to even (2.5 → 2). `trunc(x)` in PostgreSQL gives SQL Server's result. Money rounds on both sides.
- Decimal to a smaller scale rounds half away from zero on both sides (1.555 → 1.56, -2.5 → -3). Float to decimal does not: SQL Server rounds the binary value (2.675 → 2.67), PostgreSQL the shortest decimal form (2.68).
- `1234567e0` is float in SQL Server and numeric in PostgreSQL, so `CAST(1234567e0 AS varchar(30))` is '1.23457e+006' against '1234567'. A float as text keeps 6 significant digits in SQL Server (123456.7 → '123457').
- `2147483648` is numeric(10,0) in SQL Server and bigint in PostgreSQL: `2147483648 / 7` is 306783378.285714 against 306783378. `0x1F` is varbinary in SQL Server and 31 in PostgreSQL. PostgreSQL's `N'ab '` is character(n): `N'ab ' = 'ab'` is true.
- SQL Server turns '' into 0 as an integer or float but rejects it as a decimal, and rejects '1e3' as a decimal; PostgreSQL rejects '' and reads '1e3' as 1000.
- Decimals print with their scale on both sides ('1.50', '0.50', '-0.50'). Too short a varchar: SQL Server gives '*' for an integer and an error for a decimal, PostgreSQL truncates.
- Times: SQL Server rounds '1999-12-31 23:59:59.5' to 2000-01-01 00:00:00 in datetime2(0); PostgreSQL gives 1999-12-31 23:59:59 (it rounds away from 2000-01-01). time(0) of 23:59:59.5 is 23:59:59 against 24:00:00. datetime2 as text keeps every digit ('…05.5000000') where PostgreSQL drops trailing zeros; datetime as text is 'Jan  2 2026  3:04AM'.
- A real compared with a decimal: SQL Server converts the decimal to real (`CAST(0.1 AS real) = 0.1` is true), PostgreSQL both to double precision (false).
- tinyint + tinyint is tinyint in SQL Server (200 + 100 overflows). smallint + smallint is smallint on both sides.
- Storing too long a string fails on both sides; an explicit cast truncates silently on both; a SQL Server variable truncates where a PL/pgSQL variable fails.

`castmut.py` is the mutation set: 25 T-SQL expressions with 54 PostgreSQL forms, each labelled equal or different and checked row by row on both engines (`ground`, results in `ground.json`), then diffed with sqldiff (`run`). No different form may be reported equal and no equal form different.

```sh
python3 eval/casts/castmut.py ground eval/casts/ground.json
python3 eval/casts/castmut.py run target/release/sqldiff eval/casts/ground.json /tmp/casts.json
```

| | equal forms: equal | equal forms: unknown | different forms: different | different forms: unknown | false equal | false different |
|---|---|---|---|---|---|---|
| main before (6af78ea) | 12 | 9 | 1 | 17 | 15 | 0 |
| this change | 17 | 4 | 14 | 19 | 0 | 0 |

If you change a cast rule, add the probe here and run it on both engines first. The Db2 for i questions are in `probes/db2i` (fact V31).
