-- Implicit conversion on assignment, PostgreSQL 18.
\set ON_ERROR_STOP 0
\pset fieldsep '|'
CREATE TEMP TABLE a (i int, s smallint, d decimal(5,1), f float, r real, v varchar(3), c char(3), dt timestamp(3), dt2 timestamp(0), t date);
INSERT INTO a (i) VALUES (1.5); INSERT INTO a (i) VALUES (-2.5); INSERT INTO a (i) VALUES (CAST(2.5 AS float));
SELECT 'ins dec->int', i FROM a WHERE i IS NOT NULL;
DELETE FROM a;
INSERT INTO a (d) VALUES (1.25); INSERT INTO a (d) VALUES (-1.25); INSERT INTO a (d) VALUES (CAST(2.675 AS float));
SELECT 'ins dec->dec(5,1)', d FROM a;
DELETE FROM a;
INSERT INTO a (dt2) VALUES (CAST('2026-01-01 10:00:00.6' AS timestamp));
SELECT 'ins ts->ts0', dt2 FROM a;
DELETE FROM a;
INSERT INTO a (t) VALUES (CAST('2026-01-01 23:59:59.9' AS timestamp));
SELECT 'ins ts->date', t FROM a;
DELETE FROM a;
INSERT INTO a (r) VALUES (0.1);
SELECT 'ins dec->real', CAST(r AS float) FROM a;
INSERT INTO a (v) VALUES ('abcdef');
INSERT INTO a (i) VALUES ('');
DO $$ DECLARE i int := 1.5; d decimal(5,1) := 1.25; j int := CAST(2.5 AS float); BEGIN RAISE NOTICE 'var % % %', i, d, j; END $$;
DO $$ DECLARE v varchar(3) := 'abcdef'; BEGIN RAISE NOTICE 'var str %', v; END $$;
DO $$ DECLARE i int; BEGIN i := 2.7; RAISE NOTICE 'set var %', i; END $$;
CREATE TEMP TABLE u (i int); INSERT INTO u VALUES (0); UPDATE u SET i = 2.7; SELECT 'upd', i FROM u;
CREATE FUNCTION pg_temp.p(x int) RETURNS int LANGUAGE sql AS 'SELECT x';
SELECT 'param', pg_temp.p(2.7);
CREATE PROCEDURE pg_temp.q(x int) LANGUAGE plpgsql AS $$ BEGIN RAISE NOTICE 'proc param %', x; END $$;
CALL pg_temp.q(2.7);
CALL pg_temp.q(CAST(2.7 AS numeric));
