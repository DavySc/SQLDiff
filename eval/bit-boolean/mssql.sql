SET NOCOUNT ON;
SELECT 'castlit', v, TRY_CAST(v AS bit) AS b FROM (VALUES ('true'),('TRUE'),('True'),('false'),('FALSE'),('1'),('0'),('2'),('-1'),(' true'),('true '),(' 1 '),('yes'),('t'),('on'),(''),('1.5'),('0.0'),('01')) x(v);
GO
SELECT 'castint', CAST(5 AS bit), CAST(-1 AS bit), CAST(0 AS bit), CAST(0.4 AS bit), CAST(2.7 AS bit), CAST(0.0 AS bit), CAST(CAST(0.4 AS float) AS bit);
GO
CREATE TABLE #f (id int, b bit NULL, c bit NULL);
INSERT INTO #f VALUES (1, 1, 0), (2, 0, 0), (3, NULL, 1), (4, 5, -3);
UPDATE #f SET c = 7 WHERE id = 2;
SELECT 'stored', id, b, c FROM #f ORDER BY id;
SELECT 'eq2', id FROM #f WHERE b = 2;
SELECT 'eqtrue', id FROM #f WHERE b = 'true';
SELECT 'eqTRUE', id FROM #f WHERE b = 'TRUE';
SELECT 'eq1str', id FROM #f WHERE b = '1';
SELECT 'eqfalse', id FROM #f WHERE b = 'False';
GO
SELECT 'eqyes', id FROM #f WHERE b = 'yes';
GO
SELECT 'eq2str', id FROM #f WHERE b = '2';
GO
SELECT 'ops', id, ~b AS nb, b & c AS bc, b | 1 AS b1, b ^ 1 AS bx, b ^ c AS bxc, b + 0 AS plus, -b AS neg FROM #f ORDER BY id;
SELECT 'types', SQL_VARIANT_PROPERTY(~b,'BaseType'), SQL_VARIANT_PROPERTY(b & c,'BaseType'), SQL_VARIANT_PROPERTY(b | 1,'BaseType'), SQL_VARIANT_PROPERTY(b+0,'BaseType'), SQL_VARIANT_PROPERTY(ISNULL(b,5),'BaseType'), SQL_VARIANT_PROPERTY(COALESCE(b,5),'BaseType'), SQL_VARIANT_PROPERTY(CASE WHEN id>1 THEN b ELSE 0 END,'BaseType'), SQL_VARIANT_PROPERTY(IIF(id>1,b,c),'BaseType') FROM #f WHERE id = 1;
SELECT 'isnull', id, ISNULL(b, 5), COALESCE(b, 5), ISNULL(b, 0), COALESCE(b, 0) FROM #f ORDER BY id;
SELECT 'order', id FROM #f ORDER BY b, id;
SELECT 'neg', -CAST(1 AS bit);
GO
SELECT 'sum', SUM(b) FROM #f;
GO
SELECT 'max', MAX(b) FROM #f;
GO
SELECT 'minb', MIN(b) FROM #f;
GO
SELECT 'avg', AVG(b) FROM #f;
GO
SELECT 'sumcast', SUM(CAST(b AS int)), MAX(CAST(b AS int)) FROM #f;
GO
SELECT 'cnt', COUNT(b), COUNT(DISTINCT b) FROM #f;
GO
DECLARE @x bit = 2, @y bit = 'true', @z bit = -0.0001, @i int = 9; DECLARE @w bit = @i;
SELECT 'decl', @x, @y, @z, @w;
GO
DECLARE @s bit = 'yes';
GO
SELECT 'in', id FROM #f WHERE b IN (0, 1);
SELECT 'not', id FROM #f WHERE NOT (b = 1);
SELECT 'ne0', id FROM #f WHERE b <> 0;
SELECT 'gt0', id FROM #f WHERE b > 0;
GO
CREATE TABLE #c (b bit CHECK (b IN (0,1)), d bit DEFAULT 'true', e bit DEFAULT 2);
INSERT INTO #c (b) VALUES (1);
SELECT 'deflt', b, d, e FROM #c;
GO
CREATE TABLE #f (id int, b bit NULL, c bit NULL);
INSERT INTO #f VALUES (1, 1, 0), (2, 0, 0), (3, NULL, 1), (4, 1, 1);
SELECT 'ops', id, ~b AS nb, b & c AS bc, b | 1 AS b1, b ^ 1 AS bx, b ^ c AS bxc, b + 0 AS plus, b & 2 AS b2, ~CAST(1 AS int) AS ni FROM #f ORDER BY id;
SELECT 'types', SQL_VARIANT_PROPERTY(~b,'BaseType'), SQL_VARIANT_PROPERTY(b & c,'BaseType'), SQL_VARIANT_PROPERTY(b | 1,'BaseType'), SQL_VARIANT_PROPERTY(b+0,'BaseType'), SQL_VARIANT_PROPERTY(ISNULL(b,5),'BaseType'), SQL_VARIANT_PROPERTY(COALESCE(b,5),'BaseType'), SQL_VARIANT_PROPERTY(CASE WHEN id>1 THEN b ELSE 0 END,'BaseType'), SQL_VARIANT_PROPERTY(IIF(id>1,b,c),'BaseType'), SQL_VARIANT_PROPERTY(b ^ c,'BaseType') FROM #f WHERE id = 1;
SELECT 'isnull', id, ISNULL(b, 5), COALESCE(b, 5), ISNULL(b, 0), COALESCE(b, 0) FROM #f ORDER BY id;
SELECT 'order', id FROM #f ORDER BY b, id;
SELECT 'case', id, CASE WHEN b = 1 THEN 1 ELSE 0 END FROM #f ORDER BY id;
SELECT 'concat', 'x' + CAST(b AS varchar(5)), CONCAT('x', b), CAST(b AS varchar(5)) FROM #f WHERE id IN (1,2);
GO
DECLARE @v varchar(10) = 'x';
SELECT 'bitplusstr', CAST(1 AS bit) + '1';
GO
SELECT 'bitcmpstr', CASE WHEN CAST(1 AS bit) = 'true' THEN 'y' ELSE 'n' END;
GO
