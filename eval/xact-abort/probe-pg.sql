-- Probe 2026-11-02, PostgreSQL 18: the SQLSTATE of each error the T-SQL probe (probe-tsql.sql,
-- probe-tsql-2.sql) shows a procedure going on after, and that the exception block named in the
-- conversion catches it, rolls the statement back and lets the procedure go on.
CREATE TEMP TABLE pk (id int PRIMARY KEY);
CREATE TEMP TABLE t (id int PRIMARY KEY, n int NOT NULL, s varchar(3), c int CHECK (c > 0), fk int REFERENCES pk(id));
CREATE TEMP TABLE u2 (id int);
INSERT INTO u2 VALUES (1), (2);
INSERT INTO pk VALUES (1);
DO $$
DECLARE stmts text[] := ARRAY[
  'INSERT INTO t (id, n) VALUES (1, 1), (1, 2)',
  'INSERT INTO t (id, n) VALUES (2, NULL)',
  'INSERT INTO t (id, n, c) VALUES (3, 1, -1)',
  'INSERT INTO t (id, n, fk) VALUES (4, 1, 99)',
  'INSERT INTO t (id, n) VALUES (6, 3000000000)',
  'INSERT INTO t (id, n, s) VALUES (7, 1, ''toolong'')',
  'INSERT INTO t (id, n) VALUES (8, 1/0)',
  'UPDATE t SET n = (SELECT id FROM u2)',
  'INSERT INTO t (id, n) VALUES (5, ''abc''::int)'];
  x text; ran int := 0;
BEGIN
  FOREACH x IN ARRAY stmts LOOP
    BEGIN
      EXECUTE x;
    EXCEPTION WHEN integrity_constraint_violation OR division_by_zero OR numeric_value_out_of_range OR string_data_right_truncation OR cardinality_violation THEN
      RAISE WARNING '% -> caught %: %', left(x, 40), SQLSTATE, SQLERRM;
    END;
    ran := ran + 1;
  END LOOP;
END $$;
-- Error 512's counterpart: a subquery that returns two rows is cardinality_violation (21000).
INSERT INTO t (id, n) VALUES (100, 0);
DO $$ BEGIN
  BEGIN UPDATE t SET n = (SELECT id FROM u2);
  EXCEPTION WHEN cardinality_violation THEN RAISE WARNING 'caught % %', SQLSTATE, SQLERRM; END;
END $$;
