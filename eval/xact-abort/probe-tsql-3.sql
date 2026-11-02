-- Probe 2026-11-02, SQL Server 2022: the conversion error numbers the T-SQL profile maps to the
-- kind `conversion` (245, 8114, 241, 242): each ends the batch with XACT_ABORT OFF and no open
-- transaction; what ran before stays.
SET NOCOUNT ON;
IF OBJECT_ID('dbo.log3') IS NOT NULL DROP TABLE dbo.log3;
CREATE TABLE dbo.log3 (case_name varchar(40), step int);
GO
IF OBJECT_ID('dbo.c3') IS NOT NULL DROP PROCEDURE dbo.c3;
GO
CREATE PROCEDURE dbo.c3 @which varchar(40), @s varchar(30) AS
BEGIN
  SET NOCOUNT ON;
  DECLARE @d decimal(5,2), @dt date, @sd smalldatetime, @i int;
  INSERT dbo.log3 VALUES (@which, 1);
  IF @which = '245'  SET @i = CAST(@s AS int);
  IF @which = '8114' SET @d = CAST(@s AS decimal(5,2));
  IF @which = '241'  SET @dt = CAST(@s AS date);
  IF @which = '242'  SET @sd = CAST(@s AS smalldatetime);
  INSERT dbo.log3 VALUES (@which, 2);
END
GO
EXEC dbo.c3 '245', 'abc';
GO
EXEC dbo.c3 '8114', 'abc';
GO
EXEC dbo.c3 '241', 'abc';
GO
EXEC dbo.c3 '242', '2100-01-01';
GO
SELECT case_name, string_agg(CAST(step AS varchar), ',') WITHIN GROUP (ORDER BY step) AS steps FROM dbo.log3 GROUP BY case_name ORDER BY case_name;
