-- Typed values beyond int and strings (phase C2c of docs/verified-conversion.md),
-- PostgreSQL 18: integer widths, boolean, date and timestamp.
-- Run: psql -qAt < postgres.sql > postgres.out 2>&1 on a scratch server; it makes
-- and drops the database probe_c2c. Each probe raises NOTICE "Vn <name>:
-- <observation>"; errors are caught and printed by SQLSTATE. The types are as
-- `sqldiff convert --to postgres` writes them (bit becomes boolean, datetime2(p)
-- becomes timestamp(p)).
\set ON_ERROR_STOP 0
DROP DATABASE IF EXISTS probe_c2c;
CREATE DATABASE probe_c2c;
\c probe_c2c
SET client_min_messages = notice;

-- V1 integer widths: the result type of arithmetic on each width, and the
-- overflow errors. smallint + smallint is smallint (it overflows at 32767,
-- where T-SQL gives an int); integer + bigint is bigint; integer and bigint
-- overflow raise 22003.
CREATE OR REPLACE PROCEDURE v1() LANGUAGE plpgsql AS $$
DECLARE s smallint := 32767; i integer := 2147483647; b bigint := 9223372036854775807; r bigint; s1 smallint := 1; rs smallint; b1 bigint := 1; i1 integer := 1;
BEGIN
  RAISE NOTICE 'V1 smallint + integer: type=%', pg_typeof(s1 + i1);
  RAISE NOTICE 'V1 smallint + bigint: type=%', pg_typeof(s1 + b1);
  RAISE NOTICE 'V1 integer + integer: type=%', pg_typeof(i + 0);
  RAISE NOTICE 'V1 integer + bigint: type=%', pg_typeof(i + b1);
  RAISE NOTICE 'V1 bigint / integer: type=% value=%', pg_typeof(b / i), b / i;
  RAISE NOTICE 'V1 bigint %% integer: type=% value=%', pg_typeof(b % 7), b % 7;
  RAISE NOTICE 'V1 -bigint / -1: value=%', (-b) / -1;
  RAISE NOTICE 'V1 smallint + smallint: type=%', pg_typeof(s1 + s1);
  RAISE NOTICE 'V1 -smallint: type=%', pg_typeof(-s1);
  BEGIN
    r := s + s;
    RAISE NOTICE 'V1 smallint 32767 + 32767: no error, value=%', r;
  EXCEPTION WHEN OTHERS THEN
    RAISE NOTICE 'V1 smallint 32767 + 32767: error %', SQLSTATE;
  END;
  BEGIN
    rs := -32768;
    r := -rs;
    RAISE NOTICE 'V1 -(-32768 smallint): no error, value=%', r;
  EXCEPTION WHEN OTHERS THEN
    RAISE NOTICE 'V1 -(-32768 smallint): error %', SQLSTATE;
  END;
  BEGIN
    rs := -32768;
    r := rs / -1;
    RAISE NOTICE 'V1 smallint min / -1: no error, value=%', r;
  EXCEPTION WHEN OTHERS THEN
    RAISE NOTICE 'V1 smallint min / -1: error %', SQLSTATE;
  END;
  RAISE NOTICE 'V1 smallint / smallint: type=%; smallint %% smallint: type=%; smallint * smallint: type=%', pg_typeof(s1 / s1), pg_typeof(s1 % s1), pg_typeof(s1 * s1);
  BEGIN
    r := i + 1;
    RAISE NOTICE 'V1 int + 1 overflow: no error, value=%', r;
  EXCEPTION WHEN OTHERS THEN
    RAISE NOTICE 'V1 int + 1 overflow: error %', SQLSTATE;
  END;
  BEGIN
    r := b + 1;
    RAISE NOTICE 'V1 bigint + 1 overflow: no error, value=%', r;
  EXCEPTION WHEN OTHERS THEN
    RAISE NOTICE 'V1 bigint + 1 overflow: error %', SQLSTATE;
  END;
  BEGIN
    r := b * 2;
    RAISE NOTICE 'V1 bigint * 2 overflow: no error, value=%', r;
  EXCEPTION WHEN OTHERS THEN
    RAISE NOTICE 'V1 bigint * 2 overflow: error %', SQLSTATE;
  END;
  BEGIN
    b := -9223372036854775807 - 1;
    r := b / -1;
    RAISE NOTICE 'V1 bigint min / -1: no error, value=%', r;
  EXCEPTION WHEN OTHERS THEN
    RAISE NOTICE 'V1 bigint min / -1: error %', SQLSTATE;
  END;
  BEGIN
    r := b % -1;
    RAISE NOTICE 'V1 bigint min %% -1: no error, value=%', r;
  EXCEPTION WHEN OTHERS THEN
    RAISE NOTICE 'V1 bigint min %% -1: error %', SQLSTATE;
  END;
  BEGIN
    r := b / 0;
    RAISE NOTICE 'V1 bigint / 0: no error';
  EXCEPTION WHEN OTHERS THEN
    RAISE NOTICE 'V1 bigint / 0: error %', SQLSTATE;
  END;
  RAISE NOTICE 'V1 bigint / 7 truncates: value=%', (-100000000000)::bigint / 7;
END; $$;
CALL v1();

-- V2 integer literals and stores between widths: a literal beyond the integer
-- range is bigint; a store that narrows raises 22003 when the value does not
-- fit; a store that widens keeps the value; COUNT(*) is bigint and stores
-- into a bigint variable.
CREATE TABLE c (id integer NOT NULL CONSTRAINT pk_c PRIMARY KEY);
CREATE OR REPLACE PROCEDURE v2() LANGUAGE plpgsql AS $$
DECLARE s smallint; i integer; b bigint;
BEGIN
  RAISE NOTICE 'V2 literal 2147483647: type=%', pg_typeof(2147483647);
  RAISE NOTICE 'V2 literal 2147483648: type=%', pg_typeof(2147483648);
  RAISE NOTICE 'V2 literal 32767: type=%', pg_typeof(32767);
  s := 32767;
  b := s;
  i := s;
  RAISE NOTICE 'V2 widening stores keep the value: int=% bigint=%', i, b;
  BEGIN
    i := 40000;
    s := i;
    RAISE NOTICE 'V2 int 40000 into smallint: no error, value=%', s;
  EXCEPTION WHEN OTHERS THEN
    RAISE NOTICE 'V2 int 40000 into smallint: error %', SQLSTATE;
  END;
  BEGIN
    b := 3000000000;
    i := b;
    RAISE NOTICE 'V2 bigint 3000000000 into int: no error, value=%', i;
  EXCEPTION WHEN OTHERS THEN
    RAISE NOTICE 'V2 bigint 3000000000 into int: error %', SQLSTATE;
  END;
  BEGIN
    s := 40000;
    RAISE NOTICE 'V2 literal 40000 into smallint: no error, value=%', s;
  EXCEPTION WHEN OTHERS THEN
    RAISE NOTICE 'V2 literal 40000 into smallint: error %', SQLSTATE;
  END;
  INSERT INTO c (id) VALUES (1), (2), (3);
  SELECT COUNT(*) INTO b FROM c;
  RAISE NOTICE 'V2 COUNT(*) into bigint: value=% count type=%', b, (SELECT pg_typeof(COUNT(*)) FROM c);
END; $$;
CALL v2();

-- V3 boolean: the three-valued outcome of b = TRUE, b = FALSE, b <> TRUE
-- for a boolean that is TRUE, FALSE and NULL (the converter's forms for
-- T-SQL @b = 1, @b = 0, @b <> 1).
CREATE OR REPLACE PROCEDURE v3() LANGUAGE plpgsql AS $$
DECLARE v boolean; k integer := 0; o text := 'V3 three-valued:';
BEGIN
  WHILE k < 3 LOOP
    v := CASE k WHEN 0 THEN TRUE WHEN 1 THEN FALSE ELSE NULL END;
    o := o || ' [' || COALESCE(CASE WHEN v THEN '1' ELSE '0' END, 'NULL') || ':';
    IF v = TRUE THEN o := o || ' =1'; ELSE o := o || ' not=1'; END IF;
    IF v = FALSE THEN o := o || ' =0'; ELSE o := o || ' not=0'; END IF;
    IF v <> TRUE THEN o := o || ' <>1'; ELSE o := o || ' not<>1'; END IF;
    IF v <> FALSE THEN o := o || ' <>0'; ELSE o := o || ' not<>0'; END IF;
    IF v IS NULL THEN o := o || ' isnull'; ELSE o := o || ' notnull'; END IF;
    o := o || ']';
    k := k + 1;
  END LOOP;
  RAISE NOTICE '%', o;
  RAISE NOTICE 'V3 NULL = TRUE is unknown: %', CASE WHEN (v = TRUE) IS NULL THEN 'yes' ELSE 'no' END;
  RAISE NOTICE 'V3 NOT NULL is unknown: %', CASE WHEN (NOT v) IS NULL THEN 'yes' ELSE 'no' END;
END; $$;
CALL v3();

-- V4 date and timestamp literals: 'YYYY-MM-DD' and 'YYYY-MM-DDThh:mm:ss[.f]'
-- read the same under every DateStyle; a timestamp(6) literal with seven
-- fractional digits is rounded.
CREATE OR REPLACE PROCEDURE v4() LANGUAGE plpgsql AS $$
DECLARE d date; t timestamp(6); t3 timestamp(3);
BEGIN
  SET LOCAL DateStyle = 'SQL, DMY';
  d := '2024-01-02';
  t := '2024-01-02T03:04:05';
  RAISE NOTICE 'V4 DateStyle SQL, DMY: date=% timestamp=%', to_char(d, 'YYYY-MM-DD'), to_char(t, 'YYYY-MM-DD"T"HH24:MI:SS.US');
  SET LOCAL DateStyle = 'Postgres, YMD';
  d := '2024-01-02';
  t := '2024-01-02T03:04:05.5';
  RAISE NOTICE 'V4 DateStyle Postgres, YMD: date=% timestamp=%', to_char(d, 'YYYY-MM-DD'), to_char(t, 'YYYY-MM-DD"T"HH24:MI:SS.US');
  SET LOCAL DateStyle = 'German, DMY';
  d := '2024-01-02';
  t := '2024-01-02T03:04:05.123456';
  RAISE NOTICE 'V4 DateStyle German, DMY: date=% timestamp=%', to_char(d, 'YYYY-MM-DD'), to_char(t, 'YYYY-MM-DD"T"HH24:MI:SS.US');
  SET LOCAL DateStyle = 'ISO, MDY';
  t := '2024-01-01T00:00:00.1234567';
  RAISE NOTICE 'V4 seven digits into timestamp(6): %', to_char(t, 'YYYY-MM-DD"T"HH24:MI:SS.US');
  t := '2024-01-01T00:00:00.1234565';
  RAISE NOTICE 'V4 .1234565 into timestamp(6): %', to_char(t, 'YYYY-MM-DD"T"HH24:MI:SS.US');
  t := '2024-01-01T00:00:00.9999995';
  RAISE NOTICE 'V4 .9999995 into timestamp(6): %', to_char(t, 'YYYY-MM-DD"T"HH24:MI:SS.US');
  t3 := '2024-01-01T00:00:00.1239';
  RAISE NOTICE 'V4 four digits into timestamp(3): %', to_char(t3, 'YYYY-MM-DD"T"HH24:MI:SS.US');
  t := '2024-01-01T00:00:00.123456';
  t3 := t;
  RAISE NOTICE 'V4 timestamp(6) .123456 into timestamp(3): %', to_char(t3, 'YYYY-MM-DD"T"HH24:MI:SS.US');
  t := '2024-01-01T00:00:00.1235';
  t3 := t;
  RAISE NOTICE 'V4 timestamp(6) .1235 into timestamp(3): %', to_char(t3, 'YYYY-MM-DD"T"HH24:MI:SS.US');
  t3 := '2024-01-01T00:00:00.123';
  t := t3;
  RAISE NOTICE 'V4 timestamp(3) into timestamp(6): %', to_char(t, 'YYYY-MM-DD"T"HH24:MI:SS.US');
  BEGIN
    d := '2024-02-30';
    RAISE NOTICE 'V4 2024-02-30: no error';
  EXCEPTION WHEN OTHERS THEN
    RAISE NOTICE 'V4 2024-02-30: error %', SQLSTATE;
  END;
  d := '0001-01-01';
  t := '9999-12-31T23:59:59.999999';
  RAISE NOTICE 'V4 range ends: % %', to_char(d, 'YYYY-MM-DD'), to_char(t, 'YYYY-MM-DD"T"HH24:MI:SS.US');
  RAISE NOTICE 'V4 literal types: % %', pg_typeof(CAST('2024-01-02' AS date)), pg_typeof(CAST('2024-01-02T03:04:05' AS timestamp(6)));
END; $$;
CALL v4();

-- V5 date against timestamp: a date stored into timestamp is midnight; a
-- timestamp cast or stored into date keeps the date part; comparisons.
CREATE OR REPLACE PROCEDURE v5() LANGUAGE plpgsql AS $$
DECLARE d date := '2024-03-15'; t timestamp(6); e date;
BEGIN
  t := d;
  RAISE NOTICE 'V5 date into timestamp(6): %', to_char(t, 'YYYY-MM-DD"T"HH24:MI:SS.US');
  t := '2024-03-15T23:59:59.999999';
  e := CAST(t AS date);
  RAISE NOTICE 'V5 CAST(timestamp AS date): %', to_char(e, 'YYYY-MM-DD');
  e := t;
  RAISE NOTICE 'V5 timestamp stored into date: %', to_char(e, 'YYYY-MM-DD');
  RAISE NOTICE 'V5 date = timestamp (same day, 23:59): %; date < timestamp: %',
    CASE WHEN d = t THEN 'true' ELSE 'not true' END, CASE WHEN d < t THEN 'true' ELSE 'not true' END;
  RAISE NOTICE 'V5 date = timestamp midnight: %', CASE WHEN d = CAST('2024-03-15T00:00:00' AS timestamp(6)) THEN 'true' ELSE 'not true' END;
  RAISE NOTICE 'V5 date comparisons: % % %',
    CASE WHEN CAST('2024-03-14' AS date) < d THEN '< true' ELSE '< false' END,
    CASE WHEN d <= CAST('2024-03-15' AS date) THEN '<= true' ELSE '<= false' END,
    CASE WHEN d <> CAST('2024-03-15' AS date) THEN '<> true' ELSE '<> false' END;
  e := NULL;
  RAISE NOTICE 'V5 NULL date comparison: %', CASE WHEN (e = d) IS NULL THEN 'unknown' ELSE 'known' END;
END; $$;
CALL v5();

-- V6 the converter's DATEADD forms: d + n and (d + make_interval(months =>
-- n))::date on a date, t + make_interval(days|months|years => n) on a
-- timestamp: month-end clamping, Feb 29 plus a year, the time part kept, and
-- no error past 9999-12-31 (PostgreSQL's range is wider).
CREATE OR REPLACE PROCEDURE v6() LANGUAGE plpgsql AS $$
DECLARE d date := '2024-01-31'; t timestamp(6) := '2024-01-31T10:20:30.123456'; r date; rt timestamp(6);
BEGIN
  RAISE NOTICE 'V6 2024-01-31 + 1 month: %', to_char((d + make_interval(months => 1))::date, 'YYYY-MM-DD');
  RAISE NOTICE 'V6 2024-01-31 + 13 months: %', to_char((d + make_interval(months => 13))::date, 'YYYY-MM-DD');
  RAISE NOTICE 'V6 2024-03-31 - 1 month: %', to_char((CAST('2024-03-31' AS date) + make_interval(months => -1))::date, 'YYYY-MM-DD');
  RAISE NOTICE 'V6 2024-02-29 + 1 year: %', to_char((CAST('2024-02-29' AS date) + make_interval(years => 1))::date, 'YYYY-MM-DD');
  RAISE NOTICE 'V6 2024-02-29 - 1 year: %', to_char((CAST('2024-02-29' AS date) + make_interval(years => -1))::date, 'YYYY-MM-DD');
  RAISE NOTICE 'V6 2024-02-29 + 4 years: %', to_char((CAST('2024-02-29' AS date) + make_interval(years => 4))::date, 'YYYY-MM-DD');
  RAISE NOTICE 'V6 2024-01-31 + 1 day: %', to_char((d + 1), 'YYYY-MM-DD');
  RAISE NOTICE 'V6 2024-03-01 - 1 day: %', to_char((CAST('2024-03-01' AS date) - 1), 'YYYY-MM-DD');
  RAISE NOTICE 'V6 2023-12-31 + 60 days: %', to_char((CAST('2023-12-31' AS date) + 60), 'YYYY-MM-DD');
  RAISE NOTICE 'V6 timestamp + 1 month keeps the time: %', to_char((t + make_interval(months => 1)), 'YYYY-MM-DD"T"HH24:MI:SS.US');
  RAISE NOTICE 'V6 timestamp + 2 days keeps the time: %', to_char((t + make_interval(days => 2)), 'YYYY-MM-DD"T"HH24:MI:SS.US');
  RAISE NOTICE 'V6 timestamp + 1 year: %', to_char((CAST('2024-02-29T01:02:03' AS timestamp(6)) + make_interval(years => 1)), 'YYYY-MM-DD"T"HH24:MI:SS.US');
  RAISE NOTICE 'V6 result types: % % %', pg_typeof(d + 1), pg_typeof((d + make_interval(months => 1))::date), pg_typeof(t + make_interval(days => 1));
  RAISE NOTICE 'V6 NULL number: %; NULL date: %', COALESCE(to_char(d + NULL::integer, 'YYYY-MM-DD'), 'NULL'), COALESCE(to_char(NULL::date + 1, 'YYYY-MM-DD'), 'NULL');
  BEGIN
    r := CAST('9999-12-31' AS date) + 1;
    RAISE NOTICE 'V6 9999-12-31 + 1 day: no error, %', to_char(r, 'YYYY-MM-DD');
  EXCEPTION WHEN OTHERS THEN
    RAISE NOTICE 'V6 9999-12-31 + 1 day: error %', SQLSTATE;
  END;
  BEGIN
    r := (CAST('0001-06-01' AS date) + make_interval(years => -1))::date;
    RAISE NOTICE 'V6 0001-06-01 - 1 year: no error, %', to_char(r, 'YYYY-MM-DD BC');
  EXCEPTION WHEN OTHERS THEN
    RAISE NOTICE 'V6 0001-06-01 - 1 year: error %', SQLSTATE;
  END;
  BEGIN
    rt := CAST('9999-12-31T00:00:00' AS timestamp(6)) + make_interval(months => 1);
    RAISE NOTICE 'V6 timestamp 9999-12-31 + 1 month: no error, %', to_char(rt, 'YYYY-MM-DD"T"HH24:MI:SS');
  EXCEPTION WHEN OTHERS THEN
    RAISE NOTICE 'V6 timestamp 9999-12-31 + 1 month: error %', SQLSTATE;
  END;
  BEGIN
    r := d + 2147483647;
    RAISE NOTICE 'V6 + 2147483647 days: no error, %', to_char(r, 'YYYY-MM-DD');
  EXCEPTION WHEN OTHERS THEN
    RAISE NOTICE 'V6 + 2147483647 days: error %', SQLSTATE;
  END;
END; $$;
CALL v6();

-- V7 the converter's DATEDIFF(day) form, (b)::date - (a)::date: day
-- boundaries crossed, an integer, negative when the end is earlier.
CREATE OR REPLACE PROCEDURE v7() LANGUAGE plpgsql AS $$
DECLARE a timestamp(6) := '2024-01-01T23:59:00'; b timestamp(6) := '2024-01-02T00:01:00';
BEGIN
  RAISE NOTICE 'V7 23:59 to next 00:01: %; reversed: %; type=%', ((b)::date - (a)::date), ((a)::date - (b)::date), pg_typeof((b)::date - (a)::date);
  RAISE NOTICE 'V7 00:00 to 23:59 same day: %', ((CAST('2024-01-01T23:59:59.999999' AS timestamp(6)))::date - (CAST('2024-01-01T00:00:00' AS timestamp(6)))::date);
  RAISE NOTICE 'V7 dates 2023-12-31 to 2024-03-01: %', ((CAST('2024-03-01' AS date))::date - (CAST('2023-12-31' AS date))::date);
  RAISE NOTICE 'V7 date to timestamp: %', ((b)::date - (CAST('2024-01-01' AS date))::date);
  RAISE NOTICE 'V7 0001-01-01 to 9999-12-31: %', ((CAST('9999-12-31' AS date))::date - (CAST('0001-01-01' AS date))::date);
  RAISE NOTICE 'V7 NULL: %', COALESCE(((b)::date - (NULL::date)::date)::text, 'NULL');
END; $$;
CALL v7();

-- V8 typed columns: a table with bigint, smallint, boolean, date and
-- timestamp(6) columns, as the converter writes it, stores and reads back the
-- values as declared; a date key; boolean stored from TRUE/FALSE; the
-- DATEADD forms on a column in an UPDATE.
CREATE TABLE ev (
  d date NOT NULL CONSTRAINT pk_ev PRIMARY KEY,
  n bigint NULL,
  s smallint NULL CONSTRAINT df_ev_s DEFAULT 0,
  b boolean NOT NULL CONSTRAINT df_ev_b DEFAULT FALSE,
  t timestamp(6) NULL CONSTRAINT df_ev_t DEFAULT '2000-01-01T00:00:00'
);
CREATE OR REPLACE PROCEDURE v8() LANGUAGE plpgsql AS $$
DECLARE vn bigint; vs smallint; vb boolean; vt timestamp(6); c integer;
BEGIN
  INSERT INTO ev (d, n, s, b, t) VALUES ('2024-01-31', 3000000000, -7, TRUE, '2024-01-31T10:20:30.123456');
  INSERT INTO ev (d) VALUES ('2024-02-01');
  DECLARE r record;
  BEGIN
  FOR r IN SELECT ev.n AS c1, ev.s AS c2, ev.b AS c3, ev.t AS c4 FROM ev WHERE d = '2024-01-31' LOOP
    vn := r.c1; vs := r.c2; vb := r.c3; vt := r.c4;
  END LOOP;
  END;
  RAISE NOTICE 'V8 read back: n=% s=% b=% t=%', vn, vs, CASE WHEN vb THEN '1' ELSE '0' END, to_char(vt, 'YYYY-MM-DD"T"HH24:MI:SS.US');
  DECLARE r record;
  BEGIN
  FOR r IN SELECT ev.n AS c1, ev.s AS c2, ev.b AS c3, ev.t AS c4 FROM ev WHERE d = CAST('2024-02-01' AS date) LOOP
    vn := r.c1; vs := r.c2; vb := r.c3; vt := r.c4;
  END LOOP;
  END;
  RAISE NOTICE 'V8 defaults: n=% s=% b=% t=%', COALESCE(vn::text, 'NULL'), vs, CASE WHEN vb THEN '1' ELSE '0' END, to_char(vt, 'YYYY-MM-DD"T"HH24:MI:SS.US');
  UPDATE ev SET d = (d + make_interval(months => 1))::date, b = FALSE, t = (t + make_interval(days => 1)) WHERE d = '2024-01-31';
  DECLARE r record;
  BEGIN
  FOR r IN SELECT ev.b AS c3, ev.t AS c4 FROM ev WHERE d = '2024-02-29' LOOP
    vb := r.c3; vt := r.c4;
  END LOOP;
  END;
  RAISE NOTICE 'V8 after UPDATE: key 2024-02-29 b=% t=%', CASE WHEN vb THEN '1' ELSE '0' END, to_char(vt, 'YYYY-MM-DD"T"HH24:MI:SS.US');
  BEGIN
    INSERT INTO ev (d) VALUES ('2024-02-29');
    RAISE NOTICE 'V8 duplicate date key: no error';
  EXCEPTION WHEN OTHERS THEN
    RAISE NOTICE 'V8 duplicate date key: error %', SQLSTATE;
  END;
  SELECT COUNT(*) INTO c FROM ev WHERE b = FALSE;
  RAISE NOTICE 'V8 rows with b = FALSE: %', c;
  SELECT COUNT(*) INTO c FROM ev WHERE d > '2024-02-01';
  RAISE NOTICE 'V8 rows with d > 2024-02-01: %', c;
END; $$;
CALL v8();

\c postgres
DROP DATABASE probe_c2c;
