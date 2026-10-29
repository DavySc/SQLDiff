# OPENJSON probes

These probes check the PostgreSQL rule for `OPENJSON … WITH` in FROM: `openjson_pg` in `crates/sqldiff-convert/src/tsql.rs` and `pg_json_rows` / `json_rows` in `crates/sqldiff-core/src/equiv/canon.rs`. They were run on SQL Server 2022 and PostgreSQL 18.6 on 2026-10-29.

- **SQL Server** (`sqlcmd -C -I -W -s'|'`):
  - `ms1.sql`: row shapes, lax and strict paths, keys, duplicates and conversions.
  - `ms2.sql` and `ms3.sql`: more of each.
  - `ms4.sql`: conversions at the edges of each type.
- **PostgreSQL** (`psql -At`): `pg1.sql` and `pg4.sql` ask the same questions of the jsonb form.

The outputs are next to the scripts (`*.out`). The "OPENJSON and PostgreSQL jsonb" section of docs/vendor-facts-verification.md lists what the runs show and where the two engines differ: those differences are what `ASM-OPENJSON-VALUES` excludes. If you change the rule, add a probe here and run it on both engines first.
