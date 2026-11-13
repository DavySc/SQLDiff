SET NOCOUNT ON;
DROP TABLE IF EXISTS dbo.ct;
CREATE TABLE dbo.ct (si smallint NOT NULL, i int NOT NULL, bi bigint NOT NULL, d51 decimal(5,1) NOT NULL, d92 decimal(9,2) NOT NULL, d3810 decimal(38,10) NULL,
 k01 AS d51 + 1, k02 AS d51 * 2, k03 AS d92 - 100000, k04 AS (si + 1) * d51, k05 AS si + 1, k06 AS d51 * 2147483647, k07 AS 2 * si, k08 AS si * si,
 k09 AS d92 * 10, k10 AS ISNULL(d3810 * 1, 0), k11 AS bi * d51, k12 AS d51 - 0, k13 AS (1 + 2) * d51);
SELECT c.name, t.name AS type, c.precision, c.scale, c.is_nullable FROM sys.columns c JOIN sys.types t ON t.user_type_id=c.user_type_id
 WHERE c.object_id=OBJECT_ID('dbo.ct') AND c.is_computed=1 ORDER BY c.column_id;
DROP TABLE dbo.ct;
GO
-- non-persisted overflow: insert vs read
DROP TABLE IF EXISTS dbo.ov;
CREATE TABLE dbo.ov (i int NOT NULL, c AS i * i);
INSERT dbo.ov (i) VALUES (100000);
SELECT 'insert ok', COUNT(*) FROM dbo.ov;
SELECT i FROM dbo.ov;
SELECT c FROM dbo.ov;
GO
DROP TABLE IF EXISTS dbo.ov2;
CREATE TABLE dbo.ov2 (i int NOT NULL, c AS i * i PERSISTED);
INSERT dbo.ov2 (i) VALUES (100000);
SELECT 'persisted rows', COUNT(*) FROM dbo.ov2;
GO
DROP TABLE IF EXISTS dbo.ov3;
CREATE TABLE dbo.ov3 (s smallint NOT NULL, c AS s * s);
INSERT dbo.ov3 (s) VALUES (200);
SELECT c FROM dbo.ov3;
GO
