# Bit and boolean probes

These are the engine checks behind the bit rules: `crates/sqldiff-convert/src/tsql/bits.rs` (the converter) and the "bit and boolean" section of `crates/sqldiff-core/src/equiv/canon.rs` (the engine). They were run on SQL Server 2022 and PostgreSQL 18 on 2026-10-26.

- `mssql.sql`: how SQL Server reads and stores a bit. It covers strings and numbers cast to bit, stored values, comparisons, `~ & | ^`, result types, ISNULL against COALESCE, aggregates, defaults and CHECK. Run it with `sqlcmd -I -W -s"|" -h -1`. Some batches fail on purpose. The output is in `mssql.out`.
- `postgres.sql`: the same questions for boolean. It covers which comparisons, casts, assignments and operators PostgreSQL rejects, PL/pgSQL assignment, and boolean as text. Run it with `psql -At`. The output is in `postgres.out`.
- `sample-mssql.sql` and `sample-postgres.sql`: a table, 14 views and 4 functions, with the source on one side and the `sqldiff convert --to postgres` output on the other, each followed by the same data and queries. Their outputs (`sample-*.out`) give the same 48 rows once `t`/`f` are read as 1/0.

What the runs show, and what the rules rely on:

- PostgreSQL rejects `boolean = integer`, `IN (0, 1)` on a boolean, `boolean + int`, `COALESCE(boolean, 0)`, a CASE mixing boolean and integer, `max` or `sum` of a boolean, `smallint::boolean`, `numeric::boolean`, `boolean::smallint`, `& | ~ #` on booleans, and an integer stored in a boolean column or passed to a boolean parameter. Only `integer::boolean` (non-zero is true) and `boolean::integer` exist.
- PL/pgSQL assigns an integer to a boolean variable through text, so 1 and 0 work and 2 fails.
- SQL Server stores any non-zero number in a bit as 1, so `CAST(0.4 AS bit)` is 1 and `bit = 2` is never true. It reads `'true'`, `'1'`, `' 1 '`, `'2'` and `'01'` as 1, and `'false'`, `'0'` and `''` as 0, but rejects `'yes'`, `'t'` and `'on'`. PostgreSQL rejects `'2'` and `''` as booleans, and accepts `'yes'` and `' true '`.
- In SQL Server, `~b` returns a bit (NOT). `b & c`, `b | c` and `b ^ c` of two bits return bits, `b | 1` returns an int, and NULL propagates through all of them. `ISNULL(b, 5)` is 1, while `COALESCE(b, 5)` is 5.
- A bit cast to text is `'1'` in SQL Server, while a boolean cast to text is `'true'` (and `'t'` in `concat`). False sorts before true, as 0 before 1.

If you change a bit rule, add the probe here and run it on both engines first. The Db2 for i questions are in `probes/db2i` (fact V28).
