# LEFT and RIGHT against substr

Run 2026-10-30 on SQL Server 2022 and PostgreSQL 18.

- `lr-tsql.sql` runs LEFT(s, n) and RIGHT(s, n) for strings with and without
  trailing and leading spaces, empty and NULL, and counts from -2 to past the
  length and NULL (in variables, so a negative count fails at run time as in
  a routine). `lr-pg.sql` runs the converter's forms `substr(s, 1, n)` and
  `substr(s, length(s) - (n) + 1, n)`. The 176 lines of `lr-tsql.out` and
  `lr-pg.out` are identical, errors for a negative count included.
- `lr-pg-lit.sql`: PostgreSQL's left/right with a count from 0 to 6 give the
  same as those forms on 12 strings (no row differs). A negative count is
  where they part: PostgreSQL's left/right return all but |n| characters,
  T-SQL raises (the ERR lines above), so the converter keeps PostgreSQL's
  function only for a literal count from 0.
