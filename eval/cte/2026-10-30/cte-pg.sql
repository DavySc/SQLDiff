\set ON_ERROR_STOP 0
CREATE TABLE b (x int); INSERT INTO b VALUES (5);
CREATE TABLE t (x int); INSERT INTO t VALUES (7);
\echo 1 no recursion limit:
WITH RECURSIVE N(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM N WHERE i < 200) SELECT COUNT(*) FROM N;
\echo 3 anchor varchar(10) vs recursive text:
WITH RECURSIVE N(s) AS (SELECT CAST('a' AS varchar(10)) UNION ALL SELECT CAST(s || 'a' AS text) FROM N WHERE length(s) < 3) SELECT s FROM N;
\echo 3b anchor int vs recursive bigint:
WITH RECURSIVE N(i) AS (SELECT CAST(1 AS int) UNION ALL SELECT CAST(i + 1 AS bigint) FROM N WHERE i < 3) SELECT i FROM N;
\echo 4 RECURSIVE list, a query naming a later CTE (table b holds 5):
WITH RECURSIVE a AS (SELECT x FROM b), b AS (SELECT 1 AS x) SELECT x FROM a;
\echo 4b without RECURSIVE:
WITH a AS (SELECT x FROM b), b AS (SELECT 1 AS x) SELECT x FROM a;
\echo 7 non-recursive CTE naming itself (table t holds 7):
WITH t AS (SELECT x + 1 AS x FROM t) SELECT x FROM t;
\echo 7b RECURSIVE CTE naming itself without UNION:
WITH RECURSIVE t AS (SELECT x FROM t) SELECT x FROM t;
\echo 8 random() CTE read twice (1 = computed once):
WITH c AS (SELECT random() AS g) SELECT CASE WHEN a.g = b.g THEN 1 ELSE 0 END FROM c a CROSS JOIN c b;
\echo 9 recursive member with row_number:
WITH RECURSIVE N(i, r) AS (SELECT 1, CAST(1 AS bigint) UNION ALL SELECT i + 1, row_number() OVER (ORDER BY i) FROM N WHERE i < 3) SELECT i, r FROM N;
\echo 10 rows of a split:
WITH RECURSIVE L (StartPosition, EndPosition, Item) AS
  (
  SELECT 1 AS StartPosition,
         COALESCE(NULLIF(((strpos(substr('a,b,,c', 1), ',') - 1 + greatest(1, 1)) * (strpos(substr('a,b,,c', 1), ',') > 0 AND length(',') > 0)::int), 0), length(rtrim('a,b,,c')) + 1) AS EndPosition,
         SUBSTRING('a,b,,c', 1, COALESCE(NULLIF(((strpos(substr('a,b,,c', 1), ',') - 1 + greatest(1, 1)) * (strpos(substr('a,b,,c', 1), ',') > 0 AND length(',') > 0)::int), 0), length(rtrim('a,b,,c')) + 1) - 1) AS Item
  UNION ALL
  SELECT CAST(EndPosition AS integer) + 1 AS StartPosition,
         COALESCE(NULLIF(((strpos(substr('a,b,,c', EndPosition + 1), ',') - 1 + greatest(EndPosition + 1, 1)) * (strpos(substr('a,b,,c', EndPosition + 1), ',') > 0 AND length(',') > 0)::int), 0), length(rtrim('a,b,,c')) + 1) AS EndPosition,
         SUBSTRING('a,b,,c', EndPosition + 1, COALESCE(NULLIF(((strpos(substr('a,b,,c', EndPosition + 1), ',') - 1 + greatest(EndPosition + 1, 1)) * (strpos(substr('a,b,,c', EndPosition + 1), ',') > 0 AND length(',') > 0)::int), 0), length(rtrim('a,b,,c')) + 1) - EndPosition - 1) AS Item
  FROM L
  WHERE EndPosition < length(rtrim('a,b,,c')) + 1
  )
SELECT StartPosition, EndPosition, '[' || Item || ']' FROM L ORDER BY StartPosition;
