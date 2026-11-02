SET NOCOUNT ON;
-- money with bigint: type and range
DROP TABLE IF EXISTS dbo.ct;
CREATE TABLE dbo.ct (bi bigint NOT NULL, m money NOT NULL, c1 AS bi + m PERSISTED, c2 AS bi * m PERSISTED, c3 AS m - bi PERSISTED);
SELECT c.name, t.name AS type, c.precision, c.scale FROM sys.columns c JOIN sys.types t ON t.user_type_id=c.user_type_id WHERE c.object_id=OBJECT_ID('dbo.ct') AND c.is_computed=1 ORDER BY c.column_id;
BEGIN TRY INSERT INTO dbo.ct (bi, m) VALUES (1000000000000000, 0); SELECT 'bigint 1e15 * money 0 inserted', c2 FROM dbo.ct; END TRY BEGIN CATCH SELECT 'bigint 1e15 * money 0', ERROR_MESSAGE(); END CATCH
DROP TABLE dbo.ct;
-- chain: intermediate overflow, final in range
DROP TABLE IF EXISTS dbo.ch;
CREATE TABLE dbo.ch (a money NOT NULL, b money NOT NULL, c money NOT NULL, s AS a + b - c PERSISTED);
BEGIN TRY INSERT INTO dbo.ch VALUES (600000000000000, 600000000000000, 500000000000000); SELECT 'chain inserted', s FROM dbo.ch; END TRY BEGIN CATCH SELECT 'chain 6e14+6e14-5e14', ERROR_MESSAGE(); END CATCH
DROP TABLE dbo.ch;
-- money to decimal rounding
SELECT 'todec 2.5->dec(18,0)', CAST(CAST('2.5' AS money) AS decimal(18,0));
SELECT 'todec -2.5->dec(18,0)', CAST(CAST('-2.5' AS money) AS decimal(18,0));
SELECT 'todec 1.2345->dec(10,2)', CAST(CAST('1.2345' AS money) AS decimal(10,2));
SELECT 'todec 1.2350->dec(10,2)', CAST(CAST('1.2350' AS money) AS decimal(10,2));
SELECT 'todec -1.2350->dec(10,2)', CAST(CAST('-1.2350' AS money) AS decimal(10,2));
SELECT 'tosmallint 2.5', CAST(CAST('2.5' AS money) AS smallint);
BEGIN TRY SELECT 'todec overflow', CAST(CAST('123456789.5' AS money) AS decimal(10,2)); END TRY BEGIN CATCH SELECT 'todec overflow', 'ERR'; END CATCH
-- money with decimal arithmetic value
SELECT 'm+d', CAST(CAST('1.2345' AS money) + CAST(0.01 AS decimal(9,2)) AS varchar(40));
SELECT 'm*d', CAST(CAST('1.2345' AS money) * CAST(1.25 AS decimal(9,2)) AS varchar(40));
SELECT 'coalesce(m,0.00001)', CAST(COALESCE(CAST('1.2345' AS money), 0.00001) AS varchar(40));
SELECT 'isnull(m,0.5)', CAST(ISNULL(CAST(NULL AS money), 0.5) AS varchar(40));
