# Literal-conversion probes

These are the engine checks behind `crates/sqldiff-core/src/equiv/literal.rs`, which models how a string literal converts when it is compared with, or cast to, a typed value.

- `mssql.sql`: SQL Server 2022. Run it with `sqlcmd -I -W -h -1`. Some lines fail on purpose, to show that a conversion is rejected.
- `postgres.sql`: PostgreSQL 18.6. Run it with `psql -At`.
- `dateformat.sql`: every literal form under all six SQL Server DATEFORMAT settings. Its output on SQL Server 2022 is in `dateformat.out`.

If you change `convert()`, add the probe here and run it on both engines first.
