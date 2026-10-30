# Geography point constants

The probe behind the converter's geography rule (`crates/sqldiff-convert/src/tsql/geography.rs`) and the engine's decoding of the same constants (`crates/sqldiff-core/src/equiv/geography.rs`). It was run on SQL Server 2022 (RTM-CU27-GDR) and PostgreSQL 18 with PostGIS 3.6 (`mirror.gcr.io/postgis/postgis:18-3.6`) on 2026-10-30 (the bound rows the same day, after review of #221).

- `sample-mssql.sql`: a geography table and 18 `INSERT … VALUES (…, 0x…)`. Rows 1-12 are points taken from WWI `pds150-ins-app-cities-k.sql`. Row 13 is a point with Z, row 14 a point with Z and M, and row 15 is in the southern and eastern hemispheres. Rows 19-23 are the bounds: longitude 180 and -180, latitude 90 and -90, and -0.0 for both. Rows 16-18 and 24 are values the converter leaves alone: a point with SRID 4269, a polygon, a multipolygon and a point at longitude 180.5. Run it with `sqlcmd -I -W -s"|" -h -1`; the output is `sample-mssql.out`.
- `sample-postgres.sql`: the same rows as `sqldiff convert --to postgres` writes them, with the same query. Run it with `psql -At -F"|"`; the output is `sample-postgres.out`.

The queries print each point's SRID, its text, and the IEEE 754 bits of longitude, latitude, Z and M (`CONVERT(binary(8), …)` in SQL Server, `float8send(…)` in PostgreSQL).

What the runs show:

- For rows 1-15 and 19-23, SRID and all four coordinates are bit-identical, including -0.0 and the bounds themselves, on both sides. The converter's EWKB carries the doubles as SQL Server stores them, so no decimal is rendered again.
- The text differs only in spelling: `POINT (x y z)` in SQL Server and `POINT Z (x y z)` in PostGIS.
- PostGIS rejects SQL Server's own serialization (`ERROR: Invalid endian flag value encountered`) for every shape, including the points it used to receive as bytea before the converter rule.
- PostGIS moves a longitude outside ±180 (`POINT(180.5 0)` reads back as `POINT(-179.5 0)`), while SQL Server keeps 180.5.
- The converter writes a point only when it has SRID 4326, finite coordinates, a longitude within ±180 and a latitude within ±90. PostGIS would move or accept other values differently, so those points, other SRIDs and every other shape (polygons, multipolygons, lines, collections) are left as they were. The engine does not prove them, so they stay unproven.
