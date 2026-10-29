-- Wider plain tables (phase C3c of docs/verified-conversion.md), PostgreSQL 18.
-- Run: psql -qAt < postgres.sql > postgres.out 2>&1 in an empty scratch database.
-- Each probe raises NOTICE "Tn <name>: <observation>"; errors are caught and
-- printed by SQLSTATE. The tables are as `sqldiff convert --to postgres` writes
-- them (T-SQL varchar(n) and nvarchar(n) both become varchar(n)).
\set ON_ERROR_STOP 0
SET client_min_messages = notice;
DROP TABLE IF EXISTS s, d, dl, k, i, w, wi;
CREATE TABLE s (
  id integer NOT NULL CONSTRAINT pk_s PRIMARY KEY,
  v varchar(5) NULL,
  w varchar(5) NULL
);

-- T1 a string that fits is stored as it is, trailing spaces included, and read
-- back unchanged.
CREATE OR REPLACE PROCEDURE t1() LANGUAGE plpgsql AS $$
DECLARE v varchar(5); w varchar(5);
BEGIN
  INSERT INTO s (id, v, w) VALUES (1, 'ab  ', 'ab  ');
  INSERT INTO s (id, v, w) VALUES (2, 'abcde', 'abcde');
  INSERT INTO s (id, v, w) VALUES (3, '', '');
  DECLARE r record;
  BEGIN
  FOR r IN SELECT s.v AS c1, s.w AS c2 FROM s WHERE id = 1 LOOP
    v := r.c1; w := r.c2;
  END LOOP;
  END;
  RAISE NOTICE 'T1 fits, trailing spaces: v=[%] chars=% w=[%] chars=%', v, length(v), w, length(w);
  DECLARE r record;
  BEGIN
  FOR r IN SELECT s.v AS c1, s.w AS c2 FROM s WHERE id = 2 LOOP
    v := r.c1; w := r.c2;
  END LOOP;
  END;
  RAISE NOTICE 'T1 fits exactly: v=[%] w=[%]', v, w;
  v := (SELECT s.v FROM s WHERE id = 3);
  RAISE NOTICE 'T1 empty string: v=[%] null=%', v, CASE WHEN v IS NULL THEN 'yes' ELSE 'no' END;
  UPDATE s SET v = 'xy ' WHERE id = 3;
  RAISE NOTICE 'T1 update fits: chars=%', (SELECT length(s.v) FROM s WHERE id = 3);
END $$;
CALL t1();

-- T2 a string too long for the column: INSERT and UPDATE raise 22001 and write
-- nothing; when the excess is spaces only, the value is truncated silently.
CREATE OR REPLACE PROCEDURE t2() LANGUAGE plpgsql AS $$
BEGIN
  BEGIN
    INSERT INTO s (id, v) VALUES (10, 'abcdef');
    RAISE NOTICE 'T2 insert too long: no error';
  EXCEPTION WHEN OTHERS THEN
    RAISE NOTICE 'T2 insert too long: error %, rows=%', SQLSTATE, (SELECT count(*) FROM s WHERE id = 10);
  END;
  BEGIN
    UPDATE s SET v = 'abcdef' WHERE id = 2;
    RAISE NOTICE 'T2 update too long: no error';
  EXCEPTION WHEN OTHERS THEN
    RAISE NOTICE 'T2 update too long: error %', SQLSTATE;
  END;
  BEGIN
    INSERT INTO s (id, v) VALUES (11, 'abcde   ');
    RAISE NOTICE 'T2 insert too long by spaces: no error, chars=%', (SELECT length(s.v) FROM s WHERE id = 11);
  EXCEPTION WHEN OTHERS THEN
    RAISE NOTICE 'T2 insert too long by spaces: error %', SQLSTATE;
  END;
END $$;
CALL t2();

-- T4 constant defaults, as the converter writes them.
CREATE TABLE d (
  id integer NOT NULL CONSTRAINT pk_d PRIMARY KEY,
  q integer NOT NULL CONSTRAINT df_d_q DEFAULT 0,
  r integer NULL CONSTRAINT df_d_r DEFAULT (-1),
  c varchar(10) NULL CONSTRAINT df_d_c DEFAULT 'Paris',
  n varchar(10) NOT NULL CONSTRAINT df_d_n DEFAULT ('x'),
  z integer NULL CONSTRAINT df_d_z DEFAULT NULL,
  u integer NULL
);
CREATE OR REPLACE PROCEDURE t4() LANGUAGE plpgsql AS $$
DECLARE r0 record;
BEGIN
  INSERT INTO d (id) VALUES (1);
  INSERT INTO d (id, q, c) VALUES (2, 5, NULL);
  SELECT * INTO r0 FROM d WHERE id = 1;
  RAISE NOTICE 'T4 defaults: q=% r=% c=% n=% z=% u=%', r0.q, r0.r, r0.c, r0.n,
    coalesce(r0.z::text, 'NULL'), coalesce(r0.u::text, 'NULL');
  SELECT * INTO r0 FROM d WHERE id = 2;
  RAISE NOTICE 'T4 listed columns win: q=% c=%', r0.q, coalesce(r0.c, 'NULL');
  UPDATE d SET u = 7 WHERE id = 2;
  RAISE NOTICE 'T4 update leaves defaults alone: c=%', coalesce((SELECT d.c FROM d WHERE id = 2), 'NULL');
END $$;
CALL t4();
-- A default too long for its column: CREATE TABLE accepts it, the INSERT that
-- uses it raises 22001.
CREATE TABLE dl (id integer NOT NULL CONSTRAINT pk_dl PRIMARY KEY, c varchar(3) NULL CONSTRAINT df_dl_c DEFAULT 'abcdef');
DO $$
BEGIN
  INSERT INTO dl (id) VALUES (1);
  RAISE NOTICE 'T4 default too long: no error';
EXCEPTION WHEN OTHERS THEN
  RAISE NOTICE 'T4 default too long: error %', SQLSTATE;
END $$;

-- T5 comparisons on string columns under a deterministic collation: exact,
-- trailing spaces and case count.
INSERT INTO s (id, v) VALUES (20, 'A'), (21, 'a '), (22, 'a');
DO $$
BEGIN
  RAISE NOTICE 'T5 deterministic collation: rows with v = ''a'': %', (SELECT count(*) FROM s WHERE v = 'a');
  RAISE NOTICE 'T5 rows with rtrim(v) = rtrim(''a''): %', (SELECT count(*) FROM s WHERE rtrim(v) = rtrim('a'));
END $$;

-- T6 a string key: 'a' and 'a ' are two key values.
CREATE TABLE k (s varchar(5) NOT NULL CONSTRAINT pk_k PRIMARY KEY);
DO $$
BEGIN
  INSERT INTO k (s) VALUES ('a');
  INSERT INTO k (s) VALUES ('a ');
  RAISE NOTICE 'T6 string key a, a-space: no error';
EXCEPTION WHEN OTHERS THEN
  RAISE NOTICE 'T6 string key a, a-space: error %', SQLSTATE;
END $$;

-- T7 a non-unique index changes no result.
CREATE TABLE i (id integer NOT NULL CONSTRAINT pk_i PRIMARY KEY, v varchar(10) NULL, n integer NULL);
CREATE INDEX ix_i_v ON i (v);
CREATE INDEX ix_i_n ON i (n) INCLUDE (v);
CREATE OR REPLACE PROCEDURE t7() LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO i (id, v, n) VALUES (1, 'x', 1), (2, 'x', 1), (3, NULL, NULL), (4, NULL, NULL);
  UPDATE i SET v = 'y', n = 2 WHERE id = 1;
  DELETE FROM i WHERE id = 4;
  RAISE NOTICE 'T7 non-unique index: rows=% v1=%', (SELECT count(*) FROM i), (SELECT i.v FROM i WHERE id = 1);
END $$;
CALL t7();

-- T8 a btree index entry has a size limit too (about a third of a page): a long
-- value that does not compress fails the write. The values are md5 digests one
-- after the other, which the index's compression cannot shrink by the quarter
-- it asks for.
CREATE TABLE w (id integer NOT NULL CONSTRAINT pk_w PRIMARY KEY, v varchar(4000) NULL);
CREATE INDEX ix_w_v ON w (v);
CREATE OR REPLACE PROCEDURE t8(k integer, n integer) LANGUAGE plpgsql AS $$
DECLARE x text := (SELECT string_agg(md5(g::text), '') FROM generate_series(1, n) g);
BEGIN
  INSERT INTO w (id, v) VALUES (k, x);
  RAISE NOTICE 'T8 index entry of % bytes: no error', octet_length(x);
EXCEPTION WHEN OTHERS THEN
  RAISE NOTICE 'T8 index entry of % bytes: error %', octet_length(x), SQLSTATE;
END $$;
CALL t8(1, 84);
CALL t8(2, 85);
-- INCLUDE columns are stored in the index entry and count towards the limit.
CREATE TABLE wi (id integer NOT NULL CONSTRAINT pk_wi PRIMARY KEY, n integer NULL, v varchar(4000) NULL);
CREATE INDEX ix_wi_n ON wi (n) INCLUDE (v);
DO $$
DECLARE x text := (SELECT string_agg(md5(g::text), '') FROM generate_series(1, 85) g);
BEGIN
  INSERT INTO wi (id, n, v) VALUES (1, 1, x);
  RAISE NOTICE 'T8 INCLUDE entry of % bytes: no error', octet_length(x);
EXCEPTION WHEN OTHERS THEN
  RAISE NOTICE 'T8 INCLUDE entry of % bytes: error %', octet_length(x), SQLSTATE;
END $$;

-- T9 a column value stored into a local shorter than the column raises 22001
-- (T-SQL truncates: why reads are stored only where they fit).
DO $$
DECLARE x varchar(2);
BEGIN
  x := (SELECT s.v FROM s WHERE id = 2);
  RAISE NOTICE 'T9 read into a shorter variable: x=[%]', x;
EXCEPTION WHEN OTHERS THEN
  RAISE NOTICE 'T9 read into a shorter variable: error %', SQLSTATE;
END $$;
DROP TABLE s, d, dl, k, i, w, wi;
DROP PROCEDURE t1, t2, t4, t7, t8;
