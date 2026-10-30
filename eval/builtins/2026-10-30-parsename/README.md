# PARSENAME against the converter's pattern form

Run 2026-10-30 on SQL Server 2022 and PostgreSQL 18.

- `gen.py` writes `pn-tsql.sql` (T-SQL `PARSENAME(s, k)`) and `pn-pg2.sql`
  (the form `sqldiff_core::equiv::canon::parsename_pg(k)` writes) for all
  19,608 strings over the alphabet `ab.[]" ` up to length 5, parts 1 to 4.
  `pn-tsql.out` and `pn-pg2.out` are the same lines once sorted.
- `pn-edge-tsql.sql` / `pn-edge-pg.sql` (and `pn-edge2-tsql.sql`): parts of
  128 and 129 characters, plain, bracketed and quoted, with doubled closers;
  tabs, newlines, quotes, backslashes, `@`, `|`, five parts, NULL, empty
  strings, and characters outside the Basic Multilingual Plane, which T-SQL
  counts twice toward the 128 limit (`sur64`/`sur65`; `ASM-TSQL-LEN-BMP`).
  The last block of the T-SQL run gives NULL for a part number of 0, 5, -1
  or NULL, and the result type nvarchar(128) (256 bytes).
