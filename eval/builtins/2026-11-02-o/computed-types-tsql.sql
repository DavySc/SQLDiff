-- Types SQL Server 2022 CU27 derives for arithmetic computed columns (sys.columns), 2026-11-02
SET NOCOUNT ON;
DROP TABLE IF EXISTS dbo.ct;
CREATE TABLE dbo.ct (ti tinyint NOT NULL, si smallint NOT NULL, i int NOT NULL, bi bigint NOT NULL, m money NOT NULL, sm smallmoney NOT NULL,
  d92 decimal(9,2) NOT NULL, d82 decimal(8,2) NOT NULL, d51 decimal(5,1) NOT NULL, d3810 decimal(38,10) NOT NULL,
  c01 AS ti + ti, c02 AS si * si, c03 AS i - si, c04 AS bi + i, c05 AS si * m, c06 AS m + m, c07 AS m * m, c08 AS sm + sm,
  c09 AS d92 - d82, c10 AS d82 * d51, c11 AS d3810 * d3810, c12 AS si * d51, c13 AS m + d92, c14 AS ISNULL(si * m, 0.00),
  c15 AS ISNULL(d92 - d82, 0.00), c16 AS ISNULL(i - si, 0), c17 AS i + 1, c18 AS d51 + 1.25, c19 AS sm + m, c20 AS m - 1, c21 AS d92 + i,
  c22 AS ISNULL(si * m, 0), c23 AS si + 100000, c24 AS m * 2.5);
SELECT c.name, t.name AS type, c.max_length, c.precision, c.scale, c.is_nullable FROM sys.columns c JOIN sys.types t ON t.user_type_id=c.user_type_id
 WHERE c.object_id=OBJECT_ID('dbo.ct') AND c.is_computed=1 ORDER BY c.column_id;
DROP TABLE dbo.ct;
