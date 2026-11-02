# Geography from WKB and from decimal coordinates (burndown N)

Run 2026-11-02 on SQL Server 2022 (mcr.microsoft.com/mssql/server:2022-latest) and PostGIS 3.5.2 on PostgreSQL 17 (postgis/postgis:17-3.5; the WKB parser is the same code in PostgreSQL 18's PostGIS 3.6).

Question: when does `geography::STGeomFromWKB(wkb, 4326)` (T-SQL) mean the same as `ST_GeomFromWKB(wkb, 4326)::geography` (PostGIS)?

Results (`cases.txt`: name, WKB hex, `same` when both engines accepted it and `STAsBinary()` / `ST_AsBinary()` returned the input byte for byte):

- Both accept little-endian, two-dimensional WKB of types 1 to 7 and write it back unchanged (the 18 `StateProvinces` borders of WideWorldImporters, 2 to 37 KB, included).
- Neither validates the shape: a self-intersecting polygon, a hole outside its shell, a clockwise exterior ring (SQL Server `STIsValid()` = 0 afterwards) are accepted unchanged by both.
- They differ on: latitude beyond ±90 (SQL Server raises 6522, PostGIS moves it into range), longitude beyond ±180 (SQL Server keeps, PostGIS moves), NaN and infinity (SQL Server raises, PostGIS keeps), trailing bytes (SQL Server raises, PostGIS reads), big-endian, EWKB flags, ISO Z/M codes (SQL Server drops Z and M, PostGIS keeps them), CircularString (SQL Server accepts, PostGIS rejects).
- Both raise on a ring that is not closed or has fewer than four points, a line string with one point, a wrong child type in a multi geometry, a short buffer.
- `sqldiff_core::equiv::geography::wkb` accepts exactly a subset of the "same" set (unit test `wkb_as_probed_on_both_engines` replays `cases.txt`).

Fuzz (`fuzz.py`, seeds 1 to 5, 9,000+ mutated cases: flipped bytes, truncation, appended bytes, replaced doubles, changed counts and types): zero cases where the Rust reader accepted and either engine rejected or changed a byte (`cmp.py` prints `FALSE EQUAL: []`). About 2,300 cases accepted.

Points from decimal literals (`pointprobe.py`): `geography::Point(lat, lon, 4326)` and `ST_MakePoint(lon, lat)` give the same doubles as the nearest double to the literal for 606 literals of up to 27 digits (the converter accepts up to 15 significant digits, review of #248); `-0.0` is `+0.0` on both.

Run: start both containers (see local-db-engines notes), `MS=./ms.sh PG=./pg.sh ./runms.sh cases.txt`, `python3 runpg.py cases.txt`, and `cargo test -p sqldiff-core --lib wkb_decisions -- --ignored` with `SQLDIFF_WKB_CASES` / `SQLDIFF_WKB_OUT` for the Rust side.

Polygon regions (`regions.py`, `polyfuzz.py`, `tinyfuzz.py`, `chk.sh`): the same bytes are not the same polygon. SQL Server follows the left-hand rule, so a clockwise exterior ring is the rest of the globe; PostGIS takes the smaller side. `regions.py` compares `STArea()` / `ST_Area()` (1 % tolerance) and `STIntersects` / `ST_Intersects` of about 46 sample points per polygon. `sqldiff_core::equiv::geography_area::same_region` accepts a polygon only for a counter-clockwise exterior, clockwise holes, a longitude window under 180 degrees, simple rings, small extent (see its module doc). Seeds 1-5 and 21-23 of `polyfuzz.py` (about 1,800 polygons) and seeds 11-12 of `tinyfuzz.py` (500 polygons with edges of 1e-10 to 1e-5 radians): zero cases accepted by Rust where the engines differ (`chk.sh` prints `FALSE EQUAL: []`); about 15 % of the polygons differ between the engines, clockwise rings and wrong-way holes among them. All 18 WWI borders are accepted and agree. The fuzz needs both containers and `cargo test -p sqldiff-core --lib wkb_decisions -- --ignored`.
