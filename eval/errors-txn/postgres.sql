-- Errors and transactions (phase C4 of docs/verified-conversion.md), PostgreSQL 18.
-- Run: psql -qAt -f postgres.sql > postgres.out 2>&1 in an empty scratch database.
-- C4 probes, PostgreSQL. Each probe raises NOTICE "Pn <name>: <observation>".
\set ON_ERROR_STOP 0
DROP TABLE IF EXISTS t;
CREATE TABLE t (id int NOT NULL PRIMARY KEY, n int NULL, m int NOT NULL DEFAULT 0);

-- P1 control.
DO $$
DECLARE s text := ''; i int;
BEGIN
  BEGIN s := s || 'a'; i := 1/0; s := s || 'b'; EXCEPTION WHEN OTHERS THEN s := s || 'c'; END;
  s := s || '|';
  BEGIN s := s || 'd'; EXCEPTION WHEN OTHERS THEN s := s || 'e'; END;
  RAISE NOTICE 'P1 control: %', s;
END $$;

-- P2 an error in the handler is not caught by it; it reaches the enclosing block.
DO $$
DECLARE s text := ''; i int;
BEGIN
  BEGIN
    BEGIN i := 1/0;
    EXCEPTION WHEN OTHERS THEN s := s || 'inner-handler;'; i := 1/0; s := s || 'after-error-in-handler;';
    END;
  EXCEPTION WHEN OTHERS THEN s := s || 'outer-handler ' || SQLSTATE;
  END;
  RAISE NOTICE 'P2 error-in-handler: %', s;
END $$;

-- P3 variables keep the value the error left.
DO $$
DECLARE x int := 0;
BEGIN
  BEGIN x := 1; x := 2147483647 + x;
  EXCEPTION WHEN OTHERS THEN RAISE NOTICE 'P3 variables: x=%', x;
  END;
END $$;

-- P4 the database after a caught error: the block's writes are rolled back; a write in the handler stays.
CREATE OR REPLACE PROCEDURE p4() LANGUAGE plpgsql AS $$
DECLARE r text;
BEGIN
  DELETE FROM t;
  INSERT INTO t (id, n) VALUES (100, 0);
  BEGIN
    INSERT INTO t (id, n) VALUES (1, 1);
    INSERT INTO t (id, n) VALUES (1, 2);
  EXCEPTION WHEN OTHERS THEN
    INSERT INTO t (id, n) VALUES (9, 9);
    RAISE NOTICE 'P4 db-after-handler: caught %', SQLSTATE;
  END;
  SELECT string_agg(id::text, ' ' ORDER BY id) INTO r FROM t;
  RAISE NOTICE 'P4 db-after-handler: rows %', r;
END $$;
CALL p4();

-- P6 which errors WHEN OTHERS catches.
CREATE OR REPLACE PROCEDURE p6(k int) LANGUAGE plpgsql AS $$
DECLARE i int;
BEGIN
  DELETE FROM t; INSERT INTO t (id, n) VALUES (1, 1);
  BEGIN
    IF k = 1 THEN i := 1 / 0; END IF;
    IF k = 2 THEN i := 2147483647 + 1; END IF;
    IF k = 3 THEN INSERT INTO t (id, n, m) VALUES (2, 1, NULL); END IF;
    IF k = 4 THEN INSERT INTO t (id, n) VALUES (1, 1); END IF;
    IF k = 5 THEN RAISE EXCEPTION 'm'; END IF;
    IF k = 6 THEN RAISE 'm'; END IF;
    IF k = 7 THEN RAISE EXCEPTION USING MESSAGE = 'm', ERRCODE = 'P0001'; END IF;
    IF k = 8 THEN ASSERT false, 'assert'; END IF;
    RAISE NOTICE 'P6 catch k=%: no error', k;
  EXCEPTION WHEN OTHERS THEN
    RAISE NOTICE 'P6 catch k=%: caught % %', k, SQLSTATE, SQLERRM;
  END;
END $$;
CALL p6(1); CALL p6(2); CALL p6(3); CALL p6(4); CALL p6(5); CALL p6(6); CALL p6(7);
DO $$ BEGIN CALL p6(8); EXCEPTION WHEN OTHERS THEN RAISE NOTICE 'P6 catch k=8: not caught by WHEN OTHERS, outer sees %', SQLSTATE; END $$;
-- an outer handler naming assert_failure:
DO $$ BEGIN CALL p6(8); EXCEPTION WHEN assert_failure THEN RAISE NOTICE 'P6 catch k=8: reached the caller as assert_failure %', SQLSTATE; END $$;

-- P8 RAISE; re-raises the same error.
CREATE OR REPLACE PROCEDURE p8() LANGUAGE plpgsql AS $$
DECLARE i int;
BEGIN
  BEGIN i := 1/0; EXCEPTION WHEN OTHERS THEN RAISE; END;
END $$;
DO $$ BEGIN CALL p8(); EXCEPTION WHEN OTHERS THEN RAISE NOTICE 'P8 reraise: caller caught %', SQLSTATE; END $$;

-- P9 an error ends a top-level CALL and rolls back all its writes.
CREATE OR REPLACE PROCEDURE p9() LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO t (id, n) VALUES (100, 0);
  INSERT INTO t (id, n) VALUES (1, 1);
  INSERT INTO t (id, n) VALUES (1, 1);
END $$;
DELETE FROM t;
CALL p9();
SELECT 'P9 call-fails: rows [' || coalesce(string_agg(id::text, ' ' ORDER BY id), '') || ']' AS probe FROM t;

-- P9b COMMIT in the procedure keeps what came before it, top-level CALL.
CREATE OR REPLACE PROCEDURE p9b() LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO t (id, n) VALUES (100, 0);
  COMMIT;
  INSERT INTO t (id, n) VALUES (1, 1);
  INSERT INTO t (id, n) VALUES (1, 1);
END $$;
DELETE FROM t;
CALL p9b();
SELECT 'P9b commit-then-fail: rows [' || coalesce(string_agg(id::text, ' ' ORDER BY id), '') || ']' AS probe FROM t;

-- P13 COMMIT inside a block with an exception handler; SECURITY DEFINER; CALL inside a transaction block.
CREATE OR REPLACE PROCEDURE p13a() LANGUAGE plpgsql AS $$
BEGIN
  BEGIN INSERT INTO t (id, n) VALUES (5, 5); COMMIT; EXCEPTION WHEN division_by_zero THEN NULL; END;
END $$;
CALL p13a();
CREATE OR REPLACE PROCEDURE p13b() LANGUAGE plpgsql SECURITY DEFINER AS $$
BEGIN INSERT INTO t (id, n) VALUES (6, 6); COMMIT; END $$;
CALL p13b();
CREATE OR REPLACE PROCEDURE p13c() LANGUAGE plpgsql AS $$
BEGIN INSERT INTO t (id, n) VALUES (7, 7); COMMIT; END $$;
BEGIN; CALL p13c(); ROLLBACK;

-- P12 statement atomicity.
DELETE FROM t;
DO $$ BEGIN INSERT INTO t (id, n) VALUES (1, 1), (1, 2), (3, 3); EXCEPTION WHEN OTHERS THEN NULL; END $$;
SELECT 'P12 atomic insert: rows [' || coalesce(string_agg(id::text, ' ' ORDER BY id), '') || ']' AS probe FROM t;
DELETE FROM t; INSERT INTO t (id, n) VALUES (1, 1), (2, 2147483647);
UPDATE t SET n = n + 1;
SELECT 'P12 atomic update: n [' || string_agg(n::text, ' ' ORDER BY id) || ']' AS probe FROM t;

-- P13 C3a: SET reads the old values (swap).
DELETE FROM t; INSERT INTO t (id, n, m) VALUES (1, 10, 20);
UPDATE t SET n = m, m = n;
SELECT 'P13 update-old-values: n,m=' || n || ',' || m AS probe FROM t;
