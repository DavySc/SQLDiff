-- Implicit conversion on assignment (INSERT, UPDATE, SET of a variable), SQL Server 2022.
SET NOCOUNT ON;
GO
CREATE TABLE #a (i int, s smallint, d decimal(5,1), f float, r real, v varchar(3), c char(3), dt datetime, dt2 datetime2(0), t date);
INSERT INTO #a (i) VALUES (1.5); INSERT INTO #a (i) VALUES (-2.5); INSERT INTO #a (i) VALUES (CAST(2.5 AS float));
SELECT 'ins dec->int', i FROM #a WHERE i IS NOT NULL;
DELETE FROM #a;
INSERT INTO #a (d) VALUES (1.25); INSERT INTO #a (d) VALUES (-1.25); INSERT INTO #a (d) VALUES (CAST(2.675 AS float));
SELECT 'ins dec->dec(5,1)', d FROM #a;
DELETE FROM #a;
INSERT INTO #a (dt) VALUES (CAST('2026-01-01 10:00:00.0049999' AS datetime2));
INSERT INTO #a (dt2) VALUES (CAST('2026-01-01 10:00:00.6' AS datetime2));
SELECT 'ins dt2->datetime', dt, dt2 FROM #a;
DELETE FROM #a;
INSERT INTO #a (t) VALUES (CAST('2026-01-01 23:59:59.9' AS datetime2));
SELECT 'ins dt2->date', t FROM #a;
DELETE FROM #a;
INSERT INTO #a (r) VALUES (0.1);
SELECT 'ins dec->real', CAST(r AS float) FROM #a;
GO
INSERT INTO #a (v) VALUES ('abcdef');
GO
INSERT INTO #a (i) VALUES ('');
SELECT 'ins ''''->int', i FROM #a;
GO
DECLARE @i int = 1.5, @v varchar(3) = 'abcdef', @d decimal(5,1) = 1.25, @j int = CAST(2.5 AS float);
SELECT 'var', @i, @v, @d, @j;
GO
DECLARE @i int; SET @i = 2.7; SELECT 'set var', @i;
GO
CREATE TABLE #u (i int); INSERT INTO #u VALUES (0); UPDATE #u SET i = 2.7; SELECT 'upd', i FROM #u;
GO
CREATE PROCEDURE #p @x int AS SELECT 'param', @x;
GO
EXEC #p 2.7;
GO
CREATE PROCEDURE #q @x varchar(3) AS SELECT 'param str', @x;
GO
EXEC #q 'abcdef';
GO
