-- Read-only catalog tables (phase C3d part 2 of docs/verified-conversion.md),
-- PostgreSQL 18. Run: psql -qAt < tables-postgres.sql > tables-postgres.out 2>&1
-- (it creates and drops the scratch database probe_c3d2 and the role
-- probe_owner). Each probe raises NOTICE "Qn <name>: <observation>". The
-- procedures use the shapes `sqldiff convert --to postgres` writes (the FOR
-- block for SELECT @x = ..., x := (SELECT ...), SELECT COUNT(*) INTO x, IF
-- EXISTS).
\set ON_ERROR_STOP 0
\set VERBOSITY terse
DROP DATABASE IF EXISTS probe_c3d2;
DROP ROLE IF EXISTS probe_owner;
CREATE DATABASE probe_c3d2;
\c probe_c3d2
-- A table with what reads do not see: identity, UNIQUE, a foreign key, CHECK,
-- an expression default, numeric and timestamp columns, a stored generated
-- column and a virtual one (PostgreSQL 18's default kind) that raises on every
-- row; and its plain twin with the same rows in the columns the reads name.
CREATE TABLE item (id integer GENERATED ALWAYS AS IDENTITY CONSTRAINT pk_item PRIMARY KEY,
  name varchar(20) NOT NULL CONSTRAINT uq_item_name UNIQUE, stock numeric(12,2) NOT NULL);
CREATE TABLE line (ord_id integer NOT NULL, line_no integer NOT NULL,
  item_id integer NOT NULL CONSTRAINT fk_line_item REFERENCES item (id),
  qty integer NOT NULL CONSTRAINT ck_line_qty CHECK (qty > 0), price numeric(12,2) NOT NULL,
  added timestamp NOT NULL DEFAULT statement_timestamp()::timestamp,
  boom integer GENERATED ALWAYS AS (1 / (qty - qty)) VIRTUAL,
  twice integer GENERATED ALWAYS AS (qty * 2) STORED,
  CONSTRAINT pk_line PRIMARY KEY (ord_id, line_no));
CREATE INDEX ix_line_item ON line (item_id) INCLUDE (price);
CREATE TABLE item_plain (id integer NOT NULL PRIMARY KEY, name varchar(20) NOT NULL);
CREATE TABLE line_plain (ord_id integer NOT NULL, line_no integer NOT NULL, item_id integer NOT NULL,
  qty integer NOT NULL, CONSTRAINT pk_line_plain PRIMARY KEY (ord_id, line_no));
INSERT INTO item (name, stock) VALUES ('a', 1.50), ('b', 2.25), ('c', 0);
INSERT INTO line (ord_id, line_no, item_id, qty, price) VALUES
  (1, 1, 1, 5, 9.99), (1, 2, 2, 7, 1.00), (2, 1, 1, 9, 3.50);
INSERT INTO item_plain (id, name) VALUES (1, 'a'), (2, 'b'), (3, 'c');
INSERT INTO line_plain (ord_id, line_no, item_id, qty) VALUES (1, 1, 1, 5), (1, 2, 2, 7), (2, 1, 1, 9);

-- Q16 reads of a table with constraints, identity, defaults, generated and
-- out-of-slice columns give what the plain table with the same rows gives:
-- COUNT, EXISTS (SELECT 1 and SELECT *), a pick by the whole key across a
-- join, a scalar subquery; the raising virtual column raises only where a
-- query names it; two rows share the visible half of a key.
CREATE OR REPLACE PROCEDURE q16() LANGUAGE plpgsql AS $$
DECLARE c1 integer; c2 integer; q1 integer := -1; q2 integer := -1; s1 integer := -1; s2 integer := -1;
BEGIN
  SELECT COUNT(*) INTO c1 FROM line WHERE item_id = 1;
  SELECT COUNT(*) INTO c2 FROM line_plain WHERE item_id = 1;
  RAISE NOTICE 'Q16 count: table=% plain=%', c1, c2;
  DECLARE
  sqldiff_r record;
  BEGIN
  FOR sqldiff_r IN SELECT l.qty AS sqldiff_c1 FROM line l JOIN item i ON i.id = l.item_id WHERE l.ord_id = 1 AND l.line_no = 2 LOOP
    q1 := sqldiff_r.sqldiff_c1;
  END LOOP;
  FOR sqldiff_r IN SELECT l.qty AS sqldiff_c1 FROM line_plain l JOIN item_plain i ON i.id = l.item_id WHERE l.ord_id = 1 AND l.line_no = 2 LOOP
    q2 := sqldiff_r.sqldiff_c1;
  END LOOP;
  END;
  RAISE NOTICE 'Q16 pick by key across a join: table=% plain=%', q1, q2;
  s1 := (SELECT l.item_id FROM line AS l WHERE l.ord_id = 2 AND l.line_no = 1);
  s2 := (SELECT l.item_id FROM line_plain AS l WHERE l.ord_id = 2 AND l.line_no = 1);
  RAISE NOTICE 'Q16 scalar subquery: table=% plain=%', s1, s2;
  IF EXISTS (SELECT 1 FROM line WHERE qty > 8) THEN RAISE NOTICE 'Q16 exists select 1: TRUE';
  ELSE RAISE NOTICE 'Q16 exists select 1: FALSE'; END IF;
  IF EXISTS (SELECT * FROM line WHERE qty > 8) THEN RAISE NOTICE 'Q16 exists select * (raising virtual column): TRUE';
  ELSE RAISE NOTICE 'Q16 exists select * (raising virtual column): FALSE'; END IF;
  SELECT COUNT(*) INTO c1 FROM line WHERE ord_id = 1;
  RAISE NOTICE 'Q16 rows sharing the key part ord_id=1: %', c1;
END $$;
CALL q16();
CREATE OR REPLACE PROCEDURE q16_boom() LANGUAGE plpgsql AS $$
DECLARE b integer := -1;
BEGIN
  BEGIN
    b := (SELECT l.boom FROM line l WHERE l.ord_id = 1 AND l.line_no = 1);
    RAISE NOTICE 'Q16 the raising column named: no error, b=%', b;
  EXCEPTION WHEN OTHERS THEN
    RAISE NOTICE 'Q16 the raising column named: error %', SQLSTATE;
  END;
  b := (SELECT l.twice FROM line l WHERE l.ord_id = 1 AND l.line_no = 1);
  RAISE NOTICE 'Q16 the stored generated column: %', b;
END $$;
CALL q16_boom();

-- Q17 (T-SQL only: system versioning.)

-- Q18 row-level security changes what a read returns: why such a table stays
-- out. A table owned by a role that is no superuser (superusers bypass row
-- security); under ENABLE and FORCE ROW LEVEL SECURITY the owner's reads are
-- filtered too.
CREATE ROLE probe_owner NOLOGIN;
GRANT CREATE ON SCHEMA public TO probe_owner;
SET ROLE probe_owner;
CREATE TABLE doc (id integer NOT NULL PRIMARY KEY, owner name NOT NULL);
INSERT INTO doc (id, owner) VALUES (1, 'probe_owner'), (2, 'probe_u'), (3, 'x');
CREATE OR REPLACE PROCEDURE q18_count(label text) LANGUAGE plpgsql AS $$
DECLARE c integer;
BEGIN
  SELECT COUNT(*) INTO c FROM doc;
  RAISE NOTICE 'Q18 %: count=%', label, c;
END $$;
CALL q18_count('no policy, owner');
CREATE POLICY doc_owner ON doc USING (owner = current_user);
ALTER TABLE doc ENABLE ROW LEVEL SECURITY;
CALL q18_count('policy enabled, owner (not forced)');
ALTER TABLE doc FORCE ROW LEVEL SECURITY;
CALL q18_count('policy enabled and forced, owner');
RESET ROLE;

-- Q19 names: an unqualified name that is a numeric (opaque) column of one
-- table and an integer column of the other is ambiguous (42702); qualified it
-- reads the integer column; a PL/pgSQL variable spelt as a numeric column of
-- the queried table conflicts with it (42702), as with an integer column.
CREATE OR REPLACE PROCEDURE q19_try(sql text, label text) LANGUAGE plpgsql AS $$
BEGIN
  EXECUTE sql;
  RAISE NOTICE 'Q19 %: no error', label;
EXCEPTION WHEN OTHERS THEN
  RAISE NOTICE 'Q19 %: error %', label, SQLSTATE;
END $$;
CREATE TABLE price_int (item_id integer NOT NULL PRIMARY KEY, price integer NULL);
INSERT INTO price_int (item_id, price) VALUES (1, 10), (2, 20);
CALL q19_try('SELECT COUNT(*) FROM line l JOIN price_int p ON p.item_id = l.item_id WHERE price > 5', 'numeric and integer column, unqualified');
CALL q19_try('SELECT COUNT(*) FROM line l JOIN price_int p ON p.item_id = l.item_id WHERE p.price > 5', 'numeric and integer column, qualified');
CREATE OR REPLACE PROCEDURE q19_var(price integer) LANGUAGE plpgsql AS $$
DECLARE v integer := -1;
BEGIN
  DECLARE
  sqldiff_r record;
  BEGIN
  FOR sqldiff_r IN SELECT l.qty AS sqldiff_c1 FROM line l WHERE l.ord_id = 1 AND l.line_no = 1 AND l.qty > price LOOP
    v := sqldiff_r.sqldiff_c1;
  END LOOP;
  END;
  RAISE NOTICE 'Q19 variable spelt as a numeric column: no error, v=%', v;
EXCEPTION WHEN OTHERS THEN
  RAISE NOTICE 'Q19 variable spelt as a numeric column: error %', SQLSTATE;
END $$;
CALL q19_var(1);

-- Q20 system columns (review of C3d part 2): every table has the hidden
-- columns tableoid, xmin, cmin, xmax, cmax and ctid. A PL/pgSQL variable
-- spelt as one, used in a query over a table (the converter's SELECT ... INTO
-- shape) or in an UPDATE's WHERE, is the column-variable conflict too.
CREATE OR REPLACE PROCEDURE q20_try(sql text, label text) LANGUAGE plpgsql AS $$
BEGIN
  EXECUTE sql;
  RAISE NOTICE 'Q20 %: no error', label;
EXCEPTION WHEN OTHERS THEN
  RAISE NOTICE 'Q20 %: error %', label, SQLSTATE;
END $$;
CREATE OR REPLACE PROCEDURE q20_sel_tableoid(tableoid integer) LANGUAGE plpgsql AS $$
DECLARE v integer;
BEGIN
  SELECT COUNT(*) INTO v FROM line_plain WHERE ord_id = 1 AND tableoid IS NOT NULL;
END $$;
CREATE OR REPLACE PROCEDURE q20_upd_tableoid(tableoid integer) LANGUAGE plpgsql AS $$
BEGIN
  UPDATE line_plain SET qty = qty WHERE ord_id = -1 AND tableoid IS NOT NULL;
END $$;
CALL q20_try('CALL q20_sel_tableoid(1)', 'variable tableoid in SELECT INTO');
CALL q20_try('CALL q20_upd_tableoid(1)', 'variable tableoid in UPDATE WHERE');
CREATE OR REPLACE PROCEDURE q20_sel_xmin(xmin integer) LANGUAGE plpgsql AS $$
DECLARE v integer;
BEGIN
  SELECT COUNT(*) INTO v FROM line_plain WHERE ord_id = 1 AND xmin IS NOT NULL;
END $$;
CREATE OR REPLACE PROCEDURE q20_upd_xmin(xmin integer) LANGUAGE plpgsql AS $$
BEGIN
  UPDATE line_plain SET qty = qty WHERE ord_id = -1 AND xmin IS NOT NULL;
END $$;
CALL q20_try('CALL q20_sel_xmin(1)', 'variable xmin in SELECT INTO');
CALL q20_try('CALL q20_upd_xmin(1)', 'variable xmin in UPDATE WHERE');
CREATE OR REPLACE PROCEDURE q20_sel_cmin(cmin integer) LANGUAGE plpgsql AS $$
DECLARE v integer;
BEGIN
  SELECT COUNT(*) INTO v FROM line_plain WHERE ord_id = 1 AND cmin IS NOT NULL;
END $$;
CREATE OR REPLACE PROCEDURE q20_upd_cmin(cmin integer) LANGUAGE plpgsql AS $$
BEGIN
  UPDATE line_plain SET qty = qty WHERE ord_id = -1 AND cmin IS NOT NULL;
END $$;
CALL q20_try('CALL q20_sel_cmin(1)', 'variable cmin in SELECT INTO');
CALL q20_try('CALL q20_upd_cmin(1)', 'variable cmin in UPDATE WHERE');
CREATE OR REPLACE PROCEDURE q20_sel_xmax(xmax integer) LANGUAGE plpgsql AS $$
DECLARE v integer;
BEGIN
  SELECT COUNT(*) INTO v FROM line_plain WHERE ord_id = 1 AND xmax IS NOT NULL;
END $$;
CREATE OR REPLACE PROCEDURE q20_upd_xmax(xmax integer) LANGUAGE plpgsql AS $$
BEGIN
  UPDATE line_plain SET qty = qty WHERE ord_id = -1 AND xmax IS NOT NULL;
END $$;
CALL q20_try('CALL q20_sel_xmax(1)', 'variable xmax in SELECT INTO');
CALL q20_try('CALL q20_upd_xmax(1)', 'variable xmax in UPDATE WHERE');
CREATE OR REPLACE PROCEDURE q20_sel_cmax(cmax integer) LANGUAGE plpgsql AS $$
DECLARE v integer;
BEGIN
  SELECT COUNT(*) INTO v FROM line_plain WHERE ord_id = 1 AND cmax IS NOT NULL;
END $$;
CREATE OR REPLACE PROCEDURE q20_upd_cmax(cmax integer) LANGUAGE plpgsql AS $$
BEGIN
  UPDATE line_plain SET qty = qty WHERE ord_id = -1 AND cmax IS NOT NULL;
END $$;
CALL q20_try('CALL q20_sel_cmax(1)', 'variable cmax in SELECT INTO');
CALL q20_try('CALL q20_upd_cmax(1)', 'variable cmax in UPDATE WHERE');
CREATE OR REPLACE PROCEDURE q20_sel_ctid(ctid integer) LANGUAGE plpgsql AS $$
DECLARE v integer;
BEGIN
  SELECT COUNT(*) INTO v FROM line_plain WHERE ord_id = 1 AND ctid IS NOT NULL;
END $$;
CREATE OR REPLACE PROCEDURE q20_upd_ctid(ctid integer) LANGUAGE plpgsql AS $$
BEGIN
  UPDATE line_plain SET qty = qty WHERE ord_id = -1 AND ctid IS NOT NULL;
END $$;
CALL q20_try('CALL q20_sel_ctid(1)', 'variable ctid in SELECT INTO');
CALL q20_try('CALL q20_upd_ctid(1)', 'variable ctid in UPDATE WHERE');
\c postgres
DROP DATABASE probe_c3d2;
DROP ROLE probe_owner;
