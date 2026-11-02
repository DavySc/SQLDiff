-- Probe 2026-11-02, SQL Server 2022: error 512 (a subquery returned more than one value) and
-- the other statement-level errors the behaviour model lists for UPDATE and SELECT: does the
-- procedure go on (XACT_ABORT OFF, no open transaction)?
SET NOCOUNT ON;
IF OBJECT_ID('dbo.log2') IS NOT NULL DROP TABLE dbo.log2;
IF OBJECT_ID('dbo.u1') IS NOT NULL DROP TABLE dbo.u1;
IF OBJECT_ID('dbo.u2') IS NOT NULL DROP TABLE dbo.u2;
CREATE TABLE dbo.log2 (case_name varchar(40), step int);
CREATE TABLE dbo.u1 (id int NOT NULL, n int NULL);
CREATE TABLE dbo.u2 (id int NOT NULL);
INSERT dbo.u1 VALUES (1, 0);
INSERT dbo.u2 VALUES (1), (2);
GO
IF OBJECT_ID('dbo.c2') IS NOT NULL DROP PROCEDURE dbo.c2;
GO
CREATE PROCEDURE dbo.c2 @which varchar(40) AS
BEGIN
  SET NOCOUNT ON;
  INSERT dbo.log2 VALUES (@which, 1);
  IF @which = 'subq-update' UPDATE dbo.u1 SET n = (SELECT id FROM dbo.u2);
  IF @which = 'subq-assign' BEGIN DECLARE @v int; SET @v = (SELECT id FROM dbo.u2); END
  INSERT dbo.log2 VALUES (@which, 2);
END
GO
EXEC dbo.c2 'subq-update';
GO
EXEC dbo.c2 'subq-assign';
GO
SELECT case_name, string_agg(CAST(step AS varchar), ',') WITHIN GROUP (ORDER BY step) AS steps FROM dbo.log2 GROUP BY case_name ORDER BY case_name;
SELECT 'u1.n', n FROM dbo.u1;
