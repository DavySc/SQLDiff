-- Session settings a caller passes to the routines it calls (ASM-TSQL-ROWCOUNT-0),
-- PostgreSQL 18: the counterparts of mssql.sql, as `sqldiff convert --to postgres`
-- writes the procedures.
-- Run: psql -qAt < postgres.sql > postgres.out 2>&1 in an empty scratch database.
-- Each probe raises NOTICE "Rn <name>: <observation>"; errors are caught and
-- printed by SQLSTATE. PostgreSQL has no setting that limits the rows a
-- statement touches: every probe touches all rows.
\set ON_ERROR_STOP 0
SET client_min_messages = notice;
DROP TABLE IF EXISTS t;
CREATE TABLE t (
  id integer NOT NULL CONSTRAINT pk_t PRIMARY KEY,
  v integer NULL
);
CREATE OR REPLACE PROCEDURE reset() LANGUAGE plpgsql AS $$
BEGIN
  DELETE FROM t;
  INSERT INTO t (id, v) VALUES (1, 0), (2, 0), (3, 0);
END $$;

-- R0 there is no ROWCOUNT setting to inherit.
DO $$
BEGIN
  EXECUTE 'SET rowcount = 1';
  RAISE NOTICE 'R0 SET rowcount = 1: no error';
EXCEPTION WHEN OTHERS THEN
  RAISE NOTICE 'R0 SET rowcount = 1: error %', SQLSTATE;
END $$;

-- R1 the finding's counterpart: the UPDATE updates all three rows.
CREATE OR REPLACE PROCEDURE r1(INOUT n integer) LANGUAGE plpgsql AS $$
BEGIN
  UPDATE t SET v = 1;
  SELECT COUNT(*) INTO n FROM t WHERE v = 1;
END $$;
CALL reset();
DO $$
DECLARE n integer; a integer;
BEGIN
  CALL r1(n);
  a := (SELECT COUNT(*) FROM t WHERE v = 1);
  RAISE NOTICE 'R1 rows updated, as the routine counts them: %, as the caller counts them afterwards: %', n, a;
END $$;

-- R2 DELETE and a multi-row INSERT touch every row.
CREATE OR REPLACE PROCEDURE r2() LANGUAGE plpgsql AS $$
BEGIN
  DELETE FROM t WHERE v = 0;
END $$;
CREATE OR REPLACE PROCEDURE r2i() LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO t (id, v) VALUES (11, 5), (12, 5), (13, 5);
END $$;
CALL reset();
CALL r2();
DO $$ BEGIN RAISE NOTICE 'R2 DELETE of 3 rows leaves %', (SELECT COUNT(*) FROM t); END $$;
CALL reset();
CALL r2i();
DO $$ BEGIN RAISE NOTICE 'R2 INSERT of 3 rows inserts %', (SELECT COUNT(*) FROM t WHERE v = 5); END $$;

-- R3 the C3b shapes.
CREATE OR REPLACE PROCEDURE r3() LANGUAGE plpgsql AS $$
DECLARE k integer; c integer; e integer := 0; s integer;
BEGIN
  DECLARE r record;
  BEGIN
  FOR r IN SELECT t.v AS c1 FROM t WHERE id = 3 LOOP
    k := r.c1;
  END LOOP;
  END;
  SELECT COUNT(*) INTO c FROM t;
  IF EXISTS (SELECT 1 FROM t WHERE id = 3) THEN e := 1; END IF;
  s := (SELECT t.v FROM t WHERE id = 2);
  RAISE NOTICE 'R3 key pick=% count=% exists=% subquery=%', k, c, e, s;
END $$;
CALL reset();
UPDATE t SET v = id * 10;
CALL r3();
DROP PROCEDURE reset, r1, r2, r2i, r3;
DROP TABLE t;
