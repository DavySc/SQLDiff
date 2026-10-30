-- Query blocks: several tables, inner and cross joins, aliases, qualified names
-- (phase C3d part 1 of docs/verified-conversion.md), PostgreSQL 18.
-- Run: psql -qAt < blocks-postgres.sql > blocks-postgres.out 2>&1 (it creates and
-- drops the scratch database probe_c3d). Each probe raises NOTICE "Qn <name>:
-- <observation>". The procedures use the shapes `sqldiff convert --to postgres`
-- writes (the FOR block for SELECT @x = ..., x := (SELECT ...), SELECT COUNT(*)
-- INTO x, IF EXISTS).
\set ON_ERROR_STOP 0
\set VERBOSITY terse
DROP DATABASE IF EXISTS probe_c3d;
CREATE DATABASE probe_c3d;
\c probe_c3d
CREATE TABLE a (id integer NOT NULL CONSTRAINT pk_a PRIMARY KEY, n integer NULL, bid integer NULL);
CREATE TABLE b (id integer NOT NULL CONSTRAINT pk_b PRIMARY KEY, m integer NULL);
CREATE TABLE c (k1 integer NOT NULL, k2 integer NOT NULL, v integer NULL, CONSTRAINT pk_c PRIMARY KEY (k1, k2));
INSERT INTO a (id, n, bid) VALUES (1, 10, 1), (2, 20, NULL), (3, 30, 2), (4, NULL, 2);
INSERT INTO b (id, m) VALUES (1, 100), (2, 200), (3, NULL);
INSERT INTO c (k1, k2, v) VALUES (1, 1, 11), (1, 2, 12), (2, 1, 21);

-- Q11 inner and cross joins: the rows are those of the product where every ON
-- and the WHERE are TRUE; a NULL join key matches nothing; a self-join through
-- two aliases; picks by key across a join.
CREATE OR REPLACE PROCEDURE q11() LANGUAGE plpgsql AS $$
DECLARE c1 integer; c2 integer; c3 integer; c4 integer; n integer := -1; m integer := -1;
BEGIN
  SELECT COUNT(*) INTO c1 FROM a JOIN b ON a.bid = b.id WHERE b.m > 100;
  SELECT COUNT(*) INTO c2 FROM a, b WHERE a.bid = b.id AND b.m > 100;
  SELECT COUNT(*) INTO c3 FROM a CROSS JOIN b WHERE a.bid = b.id AND b.m > 100;
  SELECT COUNT(*) INTO c4 FROM a INNER JOIN b ON a.bid = b.id AND b.m > 100;
  RAISE NOTICE 'Q11 on-pooling: join-on-where=% comma=% cross=% all-in-on=%', c1, c2, c3, c4;
  SELECT COUNT(*) INTO c1 FROM a CROSS JOIN b;
  SELECT COUNT(*) INTO c2 FROM a, b, c;
  RAISE NOTICE 'Q11 product: a x b=% a x b x c=%', c1, c2;
  SELECT COUNT(*) INTO c1 FROM a JOIN b ON a.bid = b.id WHERE a.id = 2;
  SELECT COUNT(*) INTO c2 FROM a AS x JOIN a AS y ON x.bid = y.bid WHERE x.id = 2;
  RAISE NOTICE 'Q11 null join key: a.id=2 joins=% self on NULL=%', c1, c2;
  SELECT COUNT(*) INTO c1 FROM a AS x JOIN a AS y ON x.bid = y.id;
  SELECT COUNT(*) INTO c2 FROM a AS x JOIN a AS y ON y.id = x.id;
  RAISE NOTICE 'Q11 self-join: x.bid=y.id rows=% y.id=x.id rows=%', c1, c2;
  DECLARE
  sqldiff_r record;
  BEGIN
  FOR sqldiff_r IN SELECT a.n AS sqldiff_c1, b.m AS sqldiff_c2 FROM a JOIN b ON b.id = a.bid WHERE a.id = 3 LOOP
    n := sqldiff_r.sqldiff_c1;
    m := sqldiff_r.sqldiff_c2;
  END LOOP;
  END;
  RAISE NOTICE 'Q11 pick by key across a join: n=% m=%', coalesce(n::text, 'NULL'), coalesce(m::text, 'NULL');
  n := -1; m := -1;
  DECLARE
  sqldiff_r record;
  BEGIN
  FOR sqldiff_r IN SELECT a.n AS sqldiff_c1, b.m AS sqldiff_c2 FROM a JOIN b ON b.id = a.bid WHERE a.id = 2 LOOP
    n := sqldiff_r.sqldiff_c1;
    m := sqldiff_r.sqldiff_c2;
  END LOOP;
  END;
  RAISE NOTICE 'Q11 pick, no row (NULL join key): n=% m=%', coalesce(n::text, 'NULL'), coalesce(m::text, 'NULL');
  n := -1;
  DECLARE
  sqldiff_r record;
  BEGIN
  FOR sqldiff_r IN SELECT y.n AS sqldiff_c1 FROM a AS x JOIN a AS y ON y.id = x.bid WHERE x.id = 4 LOOP
    n := sqldiff_r.sqldiff_c1;
  END LOOP;
  END;
  RAISE NOTICE 'Q11 pick through a self-join: n=%', coalesce(n::text, 'NULL');
  n := -1;
  DECLARE
  sqldiff_r record;
  BEGIN
  FOR sqldiff_r IN SELECT c.v AS sqldiff_c1 FROM a, c WHERE c.k1 = a.bid AND c.k2 = 1 AND a.id = 3 LOOP
    n := sqldiff_r.sqldiff_c1;
  END LOOP;
  END;
  RAISE NOTICE 'Q11 pick, two-column key fixed from another table: v=%', coalesce(n::text, 'NULL');
  IF EXISTS (SELECT 1 FROM a JOIN b ON a.bid = b.id WHERE b.m IS NULL) THEN RAISE NOTICE 'Q11 exists over a join: TRUE';
  ELSE RAISE NOTICE 'Q11 exists over a join: FALSE'; END IF;
  IF EXISTS (SELECT * FROM a CROSS JOIN b WHERE a.id = b.id) THEN RAISE NOTICE 'Q11 exists over a cross join: TRUE';
  ELSE RAISE NOTICE 'Q11 exists over a cross join: FALSE'; END IF;
END $$;
CALL q11();

-- Q12 names: an unqualified name in two tables (42702), an aliased table's own
-- name as a qualifier (42P01), two sources with one name (42712), an ON that
-- names a table joined later or a table of another FROM item (42P01); the alias
-- hides the table name. Then the PL/pgSQL cases: a qualifier that is also a
-- variable, the loop's record, or the routine's name, and an unqualified name
-- that is a column of the second table and a variable (42702).
CREATE OR REPLACE PROCEDURE q12_try(sql text, label text) LANGUAGE plpgsql AS $$
BEGIN
  EXECUTE sql;
  RAISE NOTICE 'Q12 %: no error', label;
EXCEPTION WHEN OTHERS THEN
  RAISE NOTICE 'Q12 %: error %', label, SQLSTATE;
END $$;
CALL q12_try('SELECT id FROM a JOIN b ON a.bid = b.id', 'unqualified name in two tables');
CALL q12_try('SELECT x.n FROM a AS x WHERE a.id = 1', 'aliased table''s own name');
CALL q12_try('SELECT COUNT(*) FROM a JOIN a ON a.id = a.bid', 'one exposed name twice');
CALL q12_try('SELECT COUNT(*) FROM a JOIN b ON c.k1 = a.id JOIN c ON c.k2 = b.id', 'ON names a later table');
CALL q12_try('SELECT COUNT(*) FROM a, b JOIN c ON c.k1 = a.id', 'ON names a comma table');
CALL q12_try('SELECT COUNT(*) FROM a AS b, b AS a WHERE a.m = 100 AND b.n = 10', 'aliases swap the table names');
CALL q12_try('SELECT COUNT(*) FROM a AS X WHERE x.id = 1', 'alias folded');

CREATE OR REPLACE PROCEDURE q12() LANGUAGE plpgsql AS $$
DECLARE v integer := -1; w integer := -1;
BEGIN
  DECLARE
  sqldiff_r record;
  BEGIN
  FOR sqldiff_r IN SELECT b.n AS sqldiff_c1, a.m AS sqldiff_c2 FROM a AS b, b AS a WHERE b.id = 1 AND a.id = 2 LOOP
    v := sqldiff_r.sqldiff_c1;
    w := sqldiff_r.sqldiff_c2;
  END LOOP;
  END;
  RAISE NOTICE 'Q12 alias hides the table name: b.n=% a.m=%', v, w;
END $$;
CALL q12();

-- A qualifier that is an integer variable of the routine.
CREATE OR REPLACE PROCEDURE q12_var() LANGUAGE plpgsql AS $$
DECLARE x integer := 5; v integer := -1;
BEGIN
  DECLARE
  sqldiff_r record;
  BEGIN
  FOR sqldiff_r IN SELECT x.n AS sqldiff_c1 FROM a AS x WHERE x.id = 1 LOOP
    v := sqldiff_r.sqldiff_c1;
  END LOOP;
  END;
  RAISE NOTICE 'Q12 qualifier that is an integer variable: v=%', v;
EXCEPTION WHEN OTHERS THEN
  RAISE NOTICE 'Q12 qualifier that is an integer variable: error %', SQLSTATE;
END $$;
CALL q12_var();

-- A qualifier that is a record variable with a field of that name.
CREATE OR REPLACE PROCEDURE q12_rec() LANGUAGE plpgsql AS $$
DECLARE x record; v integer := -1;
BEGIN
  SELECT 7 AS n, 99 AS id INTO x;
  DECLARE
  sqldiff_r record;
  BEGIN
  FOR sqldiff_r IN SELECT x.n AS sqldiff_c1 FROM a AS x WHERE x.id = 1 LOOP
    v := sqldiff_r.sqldiff_c1;
  END LOOP;
  END;
  RAISE NOTICE 'Q12 qualifier that is a record variable: v=%', v;
EXCEPTION WHEN OTHERS THEN
  RAISE NOTICE 'Q12 qualifier that is a record variable: error %', SQLSTATE;
END $$;
CALL q12_rec();

-- A qualifier that is the loop's own record.
CREATE OR REPLACE PROCEDURE q12_loop() LANGUAGE plpgsql AS $$
DECLARE v integer := -1;
BEGIN
  DECLARE
  sqldiff_r record;
  BEGIN
  FOR sqldiff_r IN SELECT sqldiff_r.n AS sqldiff_c1 FROM a AS sqldiff_r WHERE sqldiff_r.id = 1 LOOP
    v := sqldiff_r.sqldiff_c1;
  END LOOP;
  END;
  RAISE NOTICE 'Q12 qualifier that is the loop record: v=%', v;
EXCEPTION WHEN OTHERS THEN
  RAISE NOTICE 'Q12 qualifier that is the loop record: error %', SQLSTATE;
END $$;
CALL q12_loop();

-- A qualifier that is the routine's name, with a parameter named like the column.
CREATE OR REPLACE PROCEDURE q12_fn(id integer, INOUT v integer) LANGUAGE plpgsql AS $$
BEGIN
  DECLARE
  sqldiff_r record;
  BEGIN
  FOR sqldiff_r IN SELECT q12_fn.n AS sqldiff_c1 FROM a AS q12_fn WHERE q12_fn.id = 1 LOOP
    v := sqldiff_r.sqldiff_c1;
  END LOOP;
  END;
  RAISE NOTICE 'Q12 qualifier that is the routine name (id is a parameter, called with 3): v=%', v;
EXCEPTION WHEN OTHERS THEN
  RAISE NOTICE 'Q12 qualifier that is the routine name: error %', SQLSTATE;
END $$;
CALL q12_fn(3, -1);

-- An unqualified name that is a column of the second table and a variable.
CREATE OR REPLACE PROCEDURE q12_cv() LANGUAGE plpgsql AS $$
DECLARE m integer := 5; v integer := -1;
BEGIN
  SELECT COUNT(*) INTO v FROM a JOIN b ON a.bid = b.id WHERE m > 100;
  RAISE NOTICE 'Q12 column of the second table and a variable: v=%', v;
EXCEPTION WHEN OTHERS THEN
  RAISE NOTICE 'Q12 column of the second table and a variable: error %', SQLSTATE;
END $$;
CALL q12_cv();

-- Q13 a scalar subquery over a join: NULL on zero rows, the value on one,
-- 21000 on two (the variable kept); the error ends the procedure.
CREATE OR REPLACE PROCEDURE q13(k integer) LANGUAGE plpgsql AS $$
DECLARE v integer := -1;
BEGIN
  BEGIN
    v := (SELECT b.m FROM a JOIN b ON b.id = a.bid WHERE a.n > k);
    RAISE NOTICE 'Q13 scalar over a join, n > %: v=%', k, coalesce(v::text, 'NULL');
  EXCEPTION WHEN OTHERS THEN
    RAISE NOTICE 'Q13 scalar over a join, n > %: error %, v=%', k, SQLSTATE, v;
  END;
END $$;
CALL q13(30);
CALL q13(20);
CALL q13(5);
CREATE OR REPLACE PROCEDURE q13x(k integer, INOUT v integer) LANGUAGE plpgsql AS $$
BEGIN
  v := (SELECT b.m FROM a JOIN b ON b.id = a.bid WHERE a.n > k);
  RAISE NOTICE 'Q13 after the scalar subquery (not reached on error)';
END $$;
DO $$
DECLARE v integer := -1;
BEGIN
  CALL q13x(5, v);
EXCEPTION WHEN OTHERS THEN
  RAISE NOTICE 'Q13 two rows: error % reaches the caller, v=%', SQLSTATE, v;
END $$;

\c postgres
DROP DATABASE probe_c3d;
