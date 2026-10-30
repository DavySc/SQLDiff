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
- SQL Server rejects arithmetic between two bits (`b + c` is Msg 402, `b * c` Msg 8117), but takes `b + 0`. `b BETWEEN 0 AND 1` works on a bit, and on a boolean only as `BETWEEN FALSE AND TRUE`. A bit read through a derived table (`d.a = 1`) is still a boolean in PostgreSQL.
- A bit cast to text is `'1'` in SQL Server, while a boolean cast to text is `'true'` (and `'t'` in `concat`). False sorts before true, as 0 before 1.

If you change a bit rule, add the probe here and run it on both engines first. The Db2 for i questions are in `probes/db2i` (fact V28).

The samples were recorded on 2026-10-26, when BIT → boolean was the `pg.bit-boolean` decision; since 2026-10-30 it is the default mapping and the converter writes `ASM-BIT-BOOLEAN` in that comment. The SQL is unchanged.
