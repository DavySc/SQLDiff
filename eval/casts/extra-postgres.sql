\set ON_ERROR_STOP 0
\pset fieldsep '|'
SELECT 'dec38 add', CAST(1.0000000001 AS decimal(38,10)) + CAST(0 AS decimal(38,10));
SELECT 'ts round pre2000', CAST(CAST('1999-12-31 23:59:59.5' AS timestamp(1)) AS timestamp(0));
SELECT 'ts round 1970', CAST(CAST('1970-01-01 00:00:00.5' AS timestamp(1)) AS timestamp(0));
SELECT 'ts round post2000', CAST(CAST('2026-01-01 00:00:00.5' AS timestamp(1)) AS timestamp(0));
SELECT 'ts round 0.25', CAST(CAST('1999-12-31 23:59:59.25' AS timestamp(2)) AS timestamp(1));
SELECT 'time round', CAST(CAST('10:00:00.5' AS time(1)) AS time(0));
SELECT 'time round 23:59:59.5', CAST(CAST('23:59:59.5' AS time(1)) AS time(0));
SELECT 'ts to time', CAST(CAST('2026-01-01 10:00:00.123456' AS timestamp) AS time);
SELECT 'flt2vc 123456.7', CAST(CAST(123456.7 AS float) AS varchar(30));
SELECT 'str2flt empty', CAST('' AS float);
SELECT 'str2flt space', CAST(' 1.5 ' AS float);
SELECT 'dec2int neg', CAST(-0.5 AS int);
SELECT 'trunc type', pg_typeof(trunc(1.57)), trunc(1.57), trunc(-1.57), pg_typeof(trunc(1.5::float8)), trunc(1.5::numeric(5,2));
SELECT 'round half', round(2.5::float8), round(2.5::numeric), CAST(0.5::float8 AS int);
SELECT 'ts typmod default', pg_typeof(CAST('2026-01-01' AS timestamp)), CAST('2026-01-01 10:00:00.1234567' AS timestamp);
