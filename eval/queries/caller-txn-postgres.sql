-- What a caller's transaction keeps when a routine without writes fails
-- (review of C3d; probe Q15), PostgreSQL 18, the converter's shape of the
-- routine. Run: psql -qAt < caller-txn-postgres.sql > caller-txn-postgres.out 2>&1
\set ON_ERROR_STOP 0
\set VERBOSITY terse
DROP DATABASE IF EXISTS probe_c3d_txn;
CREATE DATABASE probe_c3d_txn;
\c probe_c3d_txn
CREATE TABLE a (id integer NOT NULL PRIMARY KEY, n integer NULL, bid integer NULL);
CREATE TABLE b (id integer NOT NULL PRIMARY KEY, m integer NULL);
CREATE TABLE log (n integer NOT NULL);
INSERT INTO a VALUES (1, 10, 1), (3, 30, 2);
INSERT INTO b VALUES (1, 100), (2, 200);
CREATE OR REPLACE PROCEDURE look(p_k integer, INOUT p_v integer) LANGUAGE plpgsql AS $$
BEGIN
  p_v := (SELECT b.m FROM a JOIN b ON b.id = a.bid WHERE a.n > p_k);
  p_v := p_v / (p_k - 20);
END $$;
-- Q15a: the caller's own work, then the call in an exception block.
CREATE OR REPLACE PROCEDURE caller(k integer, label text) LANGUAGE plpgsql AS $$
DECLARE v integer; c integer;
BEGIN
  DELETE FROM log;
  INSERT INTO log VALUES (1);
  BEGIN
    CALL look(k, v);
    RAISE NOTICE 'Q15a %: no error', label;
  EXCEPTION WHEN OTHERS THEN
    RAISE NOTICE 'Q15a %: error %', label, SQLSTATE;
  END;
  SELECT COUNT(*) INTO c FROM log;
  RAISE NOTICE 'Q15a %: the caller''s row survives: %', label, c;
END $$;
CALL caller(5, 'cardinality (21000)');
CALL caller(20, 'division by zero (22012)');
CALL caller(25, 'no error');
-- Q15b: the caller's row committed before a top-level CALL that fails.
DELETE FROM log;
INSERT INTO log VALUES (1);
CALL look(5, NULL);
SELECT 'Q15b top-level CALL: the caller''s row survives: ' || COUNT(*) FROM log;
-- Q15c: in an open transaction block the failed CALL aborts it; the
-- client's ROLLBACK takes the row with it.
DELETE FROM log;
BEGIN;
INSERT INTO log VALUES (1);
CALL look(5, NULL);
ROLLBACK;
SELECT 'Q15c transaction block: the caller''s row survives: ' || COUNT(*) FROM log;
\c postgres
DROP DATABASE probe_c3d_txn;
