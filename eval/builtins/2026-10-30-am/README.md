# CHARINDEX with a start position, @@ROWCOUNT against FOUND (burndown AM)

Run 2026-10-30 on SQL Server 2022 (mcr.microsoft.com/mssql/server:2022-latest) and PostgreSQL 18.

## CHARINDEX(x, s, start)

`python3 grid.py tsql > t.sql` and `python3 grid.py pg > p.sql` write 576 queries: 8 needles (`''`, `'a'`, `'b'`, `'ab'`, `'A'`, `'x'`, `'b '`, NULL) × 6 strings (`''`, `'abcab'`, `'aaa'`, NULL, `'ab '`, `'xAbx'`) × 12 starts (NULL, -5, 0, 1 to 6, 100, 2147483647, -2147483648). T-SQL runs `CHARINDEX` with the string under `Latin1_General_100_CS_AS`; PostgreSQL runs the converter's form

    (strpos(substr(s, start), x) - 1 + greatest(start, 1)) * (strpos(substr(s, start), x) > 0 AND length(x) > 0)::int

`charindex-tsql.out` and `charindex-pg.out` are identical (NULL start gives NULL, start < 1 searches from 1, an empty needle gives 0, a start past the end gives 0). Under the default case-insensitive collation the only differences are matches up to letter case (ASM-CI-COLLATION). The first form tried, `strpos(...) + greatest(start, 1) - 1`, overflowed (`integer out of range`) for an empty needle at start 2147483647; the written form subtracts 1 first.

## @@ROWCOUNT and FOUND

- `rc-t.sql` / `rowcount-tsql.out`: `SELECT TOP 1 @x = …` gives 0 or 1 and leaves @x when no row; `SELECT @x = v FROM` 3 rows gives 3; an UPDATE of 2 rows with an AFTER trigger inserting 5 rows gives 2.
- `rc-t2.sql` / `rowcount-tsql-2.out`: `IF @@ROWCOUNT = 2` right after an UPDATE of 2 rows reads that UPDATE; an UPDATE through a view with an INSTEAD OF trigger writing 5 other rows gives 2 (hence ASM-ROWCOUNT-FOUND).
- `rc-p.sql` / `rowcount-pg.out`: FOUND after the converter's `FOR r IN SELECT … LIMIT 1 LOOP … END LOOP` block is whether it ran (also with a `PERFORM` finding nothing inside the loop); after an UPDATE with an AFTER statement trigger that writes and runs `PERFORM` it is the UPDATE's; GET DIAGNOSTICS ROW_COUNT gives 2.
- `commit-pg.out`: a PL/pgSQL `COMMIT` after the UPDATE keeps FOUND (t for 1 row, f for none).
