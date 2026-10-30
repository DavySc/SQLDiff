-- A call spelt in another case than the callee's declaration, PostgreSQL 18
-- (the counterpart of callee-case-mssql.sql).
-- Run: psql -X -q -f callee-case-postgres.sql > callee-case-postgres.out 2>&1 (a scratch database).
-- Unquoted names fold to lower case, so CALL LOOK(...) reaches look in any
-- database: what a case-sensitive SQL Server database refuses with 2812 runs here.
\set VERBOSITY terse
DROP PROCEDURE IF EXISTS look(integer);
CREATE PROCEDURE look(INOUT x integer) LANGUAGE plpgsql AS $$ BEGIN x := 7; END; $$;
DO $$ DECLARE v integer; BEGIN CALL look(v); RAISE NOTICE 'callee look: no error, v %', v; END; $$;
DO $$ DECLARE v integer; BEGIN CALL LOOK(v); RAISE NOTICE 'callee LOOK: no error, v %', v; END; $$;
DO $$ DECLARE v integer; BEGIN CALL PUBLIC.LOOK(v); RAISE NOTICE 'callee PUBLIC.LOOK: no error, v %', v; END; $$;
DROP PROCEDURE look(integer);
