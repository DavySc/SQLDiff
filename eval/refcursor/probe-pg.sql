-- Probes of PostgreSQL 18 behaviour of a procedure that returns result sets as
-- INOUT refcursor parameters (burndown K, D8). Run: psql -U postgres -At -f probe-pg.sql
\set ON_ERROR_STOP off
DROP TABLE IF EXISTS t CASCADE;
CREATE TABLE t (id int PRIMARY KEY, name varchar(20));
INSERT INTO t VALUES (1,'a'),(2,'b'),(3,'c');
CREATE OR REPLACE PROCEDURE p_rs(INOUT result_set_1 refcursor DEFAULT NULL, INOUT result_set_2 refcursor DEFAULT NULL)
LANGUAGE plpgsql AS $$
BEGIN
  OPEN result_set_1 FOR SELECT id, name FROM t ORDER BY id;
  UPDATE t SET name = 'changed' WHERE id = 1;
  INSERT INTO t VALUES (4, 'new');
  DELETE FROM t WHERE id = 3;
  OPEN result_set_2 FOR SELECT id, name FROM t ORDER BY id;
END;
$$;
\echo == P1/P2: explicit transaction, cursors named by the caller, later DML invisible to the first cursor
BEGIN;
CALL p_rs('rs1', 'rs2');
FETCH ALL FROM rs1;
\echo -- second
FETCH ALL FROM rs2;
COMMIT;
TRUNCATE t; INSERT INTO t VALUES (1,'a'),(2,'b'),(3,'c');
\echo == P1b: names left NULL come back as portal names
BEGIN;
CALL p_rs();
FETCH ALL FROM "<unnamed portal 1>";
COMMIT;
TRUNCATE t; INSERT INTO t VALUES (1,'a'),(2,'b'),(3,'c');
\echo == P1c: no transaction block: the cursors are gone when CALL returns
CALL p_rs('x1', 'x2');
FETCH ALL FROM x1;
\echo == P3: DROP TABLE of a table an open cursor reads
CREATE OR REPLACE PROCEDURE p_drop(INOUT result_set_1 refcursor DEFAULT NULL)
LANGUAGE plpgsql AS $$
BEGIN
  CREATE TEMP TABLE tt AS SELECT 1 AS a;
  OPEN result_set_1 FOR SELECT a FROM tt;
  DROP TABLE tt;
END;
$$;
BEGIN;
CALL p_drop('d1');
ROLLBACK;
\echo == P3b: TRUNCATE
CREATE OR REPLACE PROCEDURE p_trunc(INOUT result_set_1 refcursor DEFAULT NULL)
LANGUAGE plpgsql AS $$
BEGIN
  OPEN result_set_1 FOR SELECT id FROM t;
  TRUNCATE t;
END;
$$;
BEGIN;
CALL p_trunc('d2');
ROLLBACK;
\echo == P4: SECURITY DEFINER: the caller reads a table it has no privilege on through the cursor
DROP ROLE IF EXISTS caller_r;
CREATE ROLE caller_r LOGIN;
DROP TABLE IF EXISTS secret; CREATE TABLE secret (v int); INSERT INTO secret VALUES (42);
CREATE OR REPLACE PROCEDURE p_sec(INOUT result_set_1 refcursor DEFAULT NULL)
LANGUAGE plpgsql SECURITY DEFINER AS $$
BEGIN
  OPEN result_set_1 FOR SELECT v FROM secret;
END;
$$;
GRANT EXECUTE ON PROCEDURE p_sec(refcursor) TO caller_r;
SET ROLE caller_r;
BEGIN;
CALL p_sec('s1');
FETCH ALL FROM s1;
COMMIT;
RESET ROLE;
TRUNCATE t; INSERT INTO t VALUES (1,'a'),(2,'b'),(3,'c');
\echo == P5: same cursor name twice in one transaction
BEGIN;
CALL p_rs('dup', 'dup2');
CALL p_rs('dup', 'dup3');
ROLLBACK;
\echo == P6: a volatile/clock function is evaluated when the rows are fetched, not at OPEN
CREATE SEQUENCE sq;
CREATE OR REPLACE PROCEDURE p_vol(INOUT result_set_1 refcursor DEFAULT NULL)
LANGUAGE plpgsql AS $$
BEGIN
  OPEN result_set_1 FOR SELECT nextval('sq') AS n, statement_timestamp() > clock_timestamp() - interval '1 hour' AS ok FROM generate_series(1,2);
  RAISE NOTICE 'after OPEN, sq last_value is_called: %', (SELECT is_called FROM sq);
END;
$$;
BEGIN;
CALL p_vol('v1');
FETCH ALL FROM v1;
COMMIT;
TRUNCATE t; INSERT INTO t VALUES (1,'a'),(2,'b'),(3,'c');
\echo == P7: a runtime error in the query surfaces on FETCH, after CALL returned
CREATE OR REPLACE PROCEDURE p_err(INOUT result_set_1 refcursor DEFAULT NULL)
LANGUAGE plpgsql AS $$
BEGIN
  OPEN result_set_1 FOR SELECT 1/(id-2) AS q FROM t ORDER BY id;
END;
$$;
BEGIN;
CALL p_err('e1');
\echo -- CALL returned; now FETCH
FETCH ALL FROM e1;
ROLLBACK;
