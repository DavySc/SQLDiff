-- Single-table queries (phase C3b of docs/verified-conversion.md), PostgreSQL 18.
-- Run: psql -qAt < postgres.sql > postgres.out 2>&1 in an empty scratch database.
-- Each probe raises NOTICE "Qn <name>: <observation>". The procedures use the
-- shapes `sqldiff convert --to postgres` writes.
\set ON_ERROR_STOP 0
DROP TABLE IF EXISTS t;
DROP TABLE IF EXISTS d;
CREATE TABLE t (id integer NOT NULL CONSTRAINT pk_t PRIMARY KEY, n integer NULL, m integer NOT NULL);
CREATE TABLE d (id integer NOT NULL, n integer NULL);  -- no key: several rows can match
INSERT INTO t (id, n, m) VALUES (1, 10, 100), (2, NULL, 200), (3, 30, 300);
INSERT INTO d (id, n) VALUES (1, 11), (1, 12), (1, 13);

-- Q1 the FOR loop the converter writes for SELECT @v = col: one row assigns;
-- zero rows leave the variable as it was.
CREATE OR REPLACE PROCEDURE q1() LANGUAGE plpgsql AS $$
DECLARE v integer := -1; w integer := -1; k integer := NULL;
BEGIN
  DECLARE sqldiff_r record;
  BEGIN
  FOR sqldiff_r IN SELECT n AS sqldiff_c1 FROM t WHERE id = 1 LOOP
    v := sqldiff_r.sqldiff_c1;
  END LOOP;
  END;
  RAISE NOTICE 'Q1 select-assign one row: v=%', coalesce(v::text, 'NULL');
  DECLARE sqldiff_r record;
  BEGIN
  FOR sqldiff_r IN SELECT n AS sqldiff_c1 FROM t WHERE id = 2 LOOP
    v := sqldiff_r.sqldiff_c1;
  END LOOP;
  END;
  RAISE NOTICE 'Q1 select-assign one row, NULL column: v=%', coalesce(v::text, 'NULL');
  v := -1;
  DECLARE sqldiff_r record;
  BEGIN
  FOR sqldiff_r IN SELECT n AS sqldiff_c1 FROM t WHERE id = 99 LOOP
    v := sqldiff_r.sqldiff_c1;
  END LOOP;
  END;
  RAISE NOTICE 'Q1 select-assign zero rows: v=%', coalesce(v::text, 'NULL');
  DECLARE sqldiff_r record;
  BEGIN
  FOR sqldiff_r IN SELECT n AS sqldiff_c1, m AS sqldiff_c2 FROM t WHERE id = 3 LOOP
    v := sqldiff_r.sqldiff_c1;
    w := sqldiff_r.sqldiff_c2;
  END LOOP;
  END;
  RAISE NOTICE 'Q1 select-assign two targets: v=% w=%', v, w;
  v := -1;
  DECLARE sqldiff_r record;
  BEGIN
  FOR sqldiff_r IN SELECT n AS sqldiff_c1 FROM t WHERE id = k LOOP
    v := sqldiff_r.sqldiff_c1;
  END LOOP;
  END;
  RAISE NOTICE 'Q1 select-assign key = NULL: v=%', coalesce(v::text, 'NULL');
END $$;
CALL q1();

-- Q2 several rows: the loop runs once per row and the variable ends with the
-- last one delivered, in an order not specified.
CREATE OR REPLACE PROCEDURE q2() LANGUAGE plpgsql AS $$
DECLARE v integer := -1; i integer := 0;
BEGIN
  DECLARE sqldiff_r record;
  BEGIN
  FOR sqldiff_r IN SELECT n AS sqldiff_c1 FROM d WHERE id = 1 LOOP
    v := sqldiff_r.sqldiff_c1;
    i := i + 1;
  END LOOP;
  END;
  RAISE NOTICE 'Q2 select-assign several rows: iterations=%, v in (11,12,13): %', i,
    CASE WHEN v IN (11, 12, 13) THEN 'yes' ELSE 'no' END;
END $$;
CALL q2();

-- Q3 (T-SQL only: rows-affected counts.)

-- Q4 EXISTS is TRUE or FALSE, never UNKNOWN; its WHERE keeps rows where it is TRUE.
CREATE OR REPLACE PROCEDURE q4() LANGUAGE plpgsql AS $$
DECLARE k integer := NULL; s text := '';
BEGIN
  IF EXISTS (SELECT 1 FROM t WHERE n > 15) THEN s := s || 'a'; ELSE s := s || 'b'; END IF;
  IF EXISTS (SELECT 1 FROM t WHERE n > 99) THEN s := s || 'c'; ELSE s := s || 'd'; END IF;
  IF EXISTS (SELECT 1 FROM t WHERE n = k) THEN s := s || 'e'; ELSE s := s || 'f'; END IF;
  IF NOT EXISTS (SELECT 1 FROM t WHERE n = k) THEN s := s || 'g'; ELSE s := s || 'h'; END IF;
  IF EXISTS (SELECT 1 FROM t WHERE n IS NULL) THEN s := s || 'i'; ELSE s := s || 'j'; END IF;
  RAISE NOTICE 'Q4 exists: %', s;
END $$;
CALL q4();

-- Q5 a scalar subquery: NULL on zero rows, the value on one, error 21000 on several.
CREATE OR REPLACE PROCEDURE q5() LANGUAGE plpgsql AS $$
DECLARE v integer := -1;
BEGIN
  v := (SELECT m FROM t WHERE id = 3);
  RAISE NOTICE 'Q5 scalar one row: v=%', coalesce(v::text, 'NULL');
  v := -1;
  v := (SELECT m FROM t WHERE id = 99);
  RAISE NOTICE 'Q5 scalar zero rows: v=%', coalesce(v::text, 'NULL');
  v := -1;
  BEGIN
    v := (SELECT n FROM d WHERE id = 1);
    RAISE NOTICE 'Q5 scalar several rows: no error';
  EXCEPTION WHEN OTHERS THEN
    RAISE NOTICE 'Q5 scalar several rows: error % (%), v=%', SQLSTATE, SQLERRM, v;
  END;
END $$;
CALL q5();

-- Q6 count(*): bigint, 0 on no rows; SELECT count(*) INTO always assigns.
CREATE OR REPLACE PROCEDURE q6() LANGUAGE plpgsql AS $$
DECLARE c integer := -1;
BEGIN
  SELECT COUNT(*) INTO c FROM t WHERE n > 99;
  RAISE NOTICE 'Q6 count no rows: c=%', c;
  SELECT COUNT(*) INTO c FROM t WHERE n > 15;
  RAISE NOTICE 'Q6 count where: c=%', c;
  c := (SELECT COUNT(*) FROM t);
  RAISE NOTICE 'Q6 count scalar: c=%', c;
  RAISE NOTICE 'Q6 count type: %', pg_typeof((SELECT COUNT(*) FROM t));
END $$;
CALL q6();

-- Q7 EXISTS does not evaluate its select list, not even a constant one while
-- planning: SELECT 1/0 inside raises nothing (SQL Server alike). The slice
-- takes only SELECT 1 and SELECT * there all the same.
CREATE OR REPLACE PROCEDURE q7() LANGUAGE plpgsql AS $$
BEGIN
  IF EXISTS (SELECT 1/0 FROM t WHERE id = 1) THEN RAISE NOTICE 'Q7 exists 1/0: true, no error'; END IF;
EXCEPTION WHEN OTHERS THEN
  RAISE NOTICE 'Q7 exists 1/0: error %', SQLSTATE;
END $$;
CALL q7();

-- Q8 the read sees the procedure's own earlier write.
CREATE OR REPLACE PROCEDURE q8() LANGUAGE plpgsql AS $$
DECLARE v integer := -1;
BEGIN
  UPDATE t SET n = 31 WHERE id = 3;
  DECLARE sqldiff_r record;
  BEGIN
  FOR sqldiff_r IN SELECT n AS sqldiff_c1 FROM t WHERE id = 3 LOOP
    v := sqldiff_r.sqldiff_c1;
  END LOOP;
  END;
  RAISE NOTICE 'Q8 read own write: v=%', v;
  UPDATE t SET n = 30 WHERE id = 3;
END $$;
CALL q8();

-- Q9 a name that is both a column of the queried table and a variable is an
-- error in the query (the refusal of resolveName), in a FOR query and in EXISTS.
CREATE OR REPLACE PROCEDURE q9(id integer) LANGUAGE plpgsql AS $$
DECLARE v integer := -1;
BEGIN
  BEGIN
    DECLARE sqldiff_r record;
    BEGIN
    FOR sqldiff_r IN SELECT n AS sqldiff_c1 FROM t WHERE id = 1 LOOP
      v := sqldiff_r.sqldiff_c1;
    END LOOP;
    END;
    RAISE NOTICE 'Q9 ambiguous name in FOR query: no error';
  EXCEPTION WHEN OTHERS THEN
    RAISE NOTICE 'Q9 ambiguous name in FOR query: error %', SQLSTATE;
  END;
  BEGIN
    IF EXISTS (SELECT 1 FROM t WHERE id = 1) THEN v := 1; END IF;
    RAISE NOTICE 'Q9 ambiguous name in EXISTS: no error';
  EXCEPTION WHEN OTHERS THEN
    RAISE NOTICE 'Q9 ambiguous name in EXISTS: error %', SQLSTATE;
  END;
END $$;
CALL q9(1);

-- Q10 why the converter does not write SELECT ... INTO for a row query:
-- without STRICT, zero rows set the target to NULL (T-SQL keeps it).
CREATE OR REPLACE PROCEDURE q10() LANGUAGE plpgsql AS $$
DECLARE v integer := -1;
BEGIN
  SELECT n INTO v FROM t WHERE id = 99;
  RAISE NOTICE 'Q10 select into zero rows: v=%', coalesce(v::text, 'NULL');
END $$;
CALL q10();

DROP TABLE t;
DROP TABLE d;
