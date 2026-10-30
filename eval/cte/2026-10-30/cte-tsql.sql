SET NOCOUNT ON;
CREATE TABLE b (x int); INSERT INTO b VALUES (5);
CREATE TABLE t (x int); INSERT INTO t VALUES (7);
GO
PRINT '1 default recursion limit (expect Msg 530):';
WITH N(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM N WHERE i < 200) SELECT COUNT(*) FROM N;
GO
PRINT '2 MAXRECURSION 0:';
WITH N(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM N WHERE i < 200) SELECT COUNT(*) FROM N OPTION (MAXRECURSION 0);
GO
PRINT '3 anchor varchar(10) vs recursive varchar(max):';
WITH N(s) AS (SELECT CAST('a' AS varchar(10)) UNION ALL SELECT CAST(s + 'a' AS varchar(max)) FROM N WHERE LEN(s) < 3) SELECT s FROM N;
GO
PRINT '3b anchor int vs recursive bigint:';
WITH N(i) AS (SELECT CAST(1 AS int) UNION ALL SELECT CAST(i + 1 AS bigint) FROM N WHERE i < 3) SELECT i FROM N;
GO
PRINT '4 a query naming a later CTE (table b holds 5):';
WITH a AS (SELECT x FROM b), b AS (SELECT 1 AS x) SELECT x FROM a;
GO
PRINT '5 CTE name in other letter case:';
WITH Abc AS (SELECT 1 AS x) SELECT x FROM aBC;
GO
PRINT '6 CTE named like table t (holds 7), one-part and dbo.t:';
WITH t AS (SELECT 2 AS x) SELECT (SELECT x FROM t) AS cte, (SELECT x FROM dbo.t) AS tab;
GO
PRINT '7 CTE whose query names itself without UNION ALL:';
WITH t AS (SELECT x FROM t) SELECT x FROM t;
GO
PRINT '8 NEWID() CTE read twice (0 = computed per read):';
WITH c AS (SELECT NEWID() AS g) SELECT CASE WHEN a.g = b.g THEN 1 ELSE 0 END FROM c a CROSS JOIN c b;
GO
PRINT '9 recursive member with ROW_NUMBER:';
WITH N(i, r) AS (SELECT 1, CAST(1 AS bigint) UNION ALL SELECT i + 1, ROW_NUMBER() OVER (ORDER BY i) FROM N WHERE i < 3) SELECT i, r FROM N;
GO
PRINT '10 rows of a split, any order (StartPosition, item):';
DECLARE @List nvarchar(max) = N'a,b,,c', @Delimiter nvarchar(max) = N',';
WITH L (StartPosition, EndPosition, Item) AS
  (
  SELECT 1 AS StartPosition,
         ISNULL(NULLIF(CHARINDEX(@Delimiter, @List, 1), 0), LEN(@List) + 1) AS EndPosition,
         SUBSTRING(@List, 1, ISNULL(NULLIF(CHARINDEX(@Delimiter, @List, 1), 0), LEN(@List) + 1) - 1) AS Item
  WHERE @List IS NOT NULL
  UNION ALL
  SELECT CAST(EndPosition AS int) + 1 AS StartPosition,
         ISNULL(NULLIF(CHARINDEX(@Delimiter, @List, EndPosition + 1), 0), LEN(@List) + 1) AS EndPosition,
         SUBSTRING(@List, EndPosition + 1, ISNULL(NULLIF(CHARINDEX(@Delimiter, @List, EndPosition + 1), 0), LEN(@List) + 1) - EndPosition - 1) AS Item
  FROM L
  WHERE EndPosition < LEN(@List) + 1
  )
SELECT StartPosition, EndPosition, '[' + Item + ']' FROM L ORDER BY StartPosition OPTION (MAXRECURSION 0);
GO
PRINT '11 types of the split columns:';
DECLARE @List nvarchar(max) = N'a,b', @Delimiter nvarchar(max) = N',';
SELECT SQL_VARIANT_PROPERTY(CHARINDEX(@Delimiter, @List, 1), 'BaseType') AS charindex_max,
       SQL_VARIANT_PROPERTY(LEN(@List), 'BaseType') AS len_max,
       SQL_VARIANT_PROPERTY(CHARINDEX(N',', N'a,b', 1), 'BaseType') AS charindex_lit;
GO
