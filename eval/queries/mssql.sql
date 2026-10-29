-- Single-table queries (phase C3b of docs/verified-conversion.md), SQL Server 2022.
-- Run: sqlcmd -I -i mssql.sql > mssql.out (a scratch server, never one holding data).
-- Each probe prints "Qn <name>: <observation>". Messages "(n rows affected)"
-- are part of what Q3 observes, so NOCOUNT is set per probe.
SET NOCOUNT ON;
GO
DROP DATABASE IF EXISTS probe_c3b;
GO
CREATE DATABASE probe_c3b;
GO
USE probe_c3b;
GO
CREATE TABLE dbo.t (id int NOT NULL CONSTRAINT pk_t PRIMARY KEY, n int NULL, m int NOT NULL);
CREATE TABLE dbo.d (id int NOT NULL, n int NULL);  -- no key: several rows can match
INSERT INTO dbo.t (id, n, m) VALUES (1, 10, 100), (2, NULL, 200), (3, 30, 300);
INSERT INTO dbo.d (id, n) VALUES (1, 11), (1, 12), (1, 13);
GO

-- Q1 SELECT @v = col: one row assigns; zero rows leave the variable as it was.
CREATE OR ALTER PROCEDURE dbo.q1 AS
BEGIN
  SET NOCOUNT ON;
  DECLARE @v int = -1, @w int = -1;
  SELECT @v = n FROM dbo.t WHERE id = 1;
  PRINT 'Q1 select-assign one row: v=' + ISNULL(CAST(@v AS varchar(11)), 'NULL');
  SELECT @v = n FROM dbo.t WHERE id = 2;
  PRINT 'Q1 select-assign one row, NULL column: v=' + ISNULL(CAST(@v AS varchar(11)), 'NULL');
  SET @v = -1;
  SELECT @v = n FROM dbo.t WHERE id = 99;
  PRINT 'Q1 select-assign zero rows: v=' + ISNULL(CAST(@v AS varchar(11)), 'NULL');
  SELECT @v = n, @w = m FROM dbo.t WHERE id = 3;
  PRINT 'Q1 select-assign two targets: v=' + CAST(@v AS varchar(11)) + ' w=' + CAST(@w AS varchar(11));
  DECLARE @k int = NULL;
  SET @v = -1;
  SELECT @v = n FROM dbo.t WHERE id = @k;
  PRINT 'Q1 select-assign key = NULL: v=' + ISNULL(CAST(@v AS varchar(11)), 'NULL');
END
GO
EXEC dbo.q1;
GO

-- Q2 several rows: the variable ends with one of them, which one is not specified
-- (here the last the plan delivers); why the slice asks the WHERE to fix the key.
CREATE OR ALTER PROCEDURE dbo.q2 AS
BEGIN
  SET NOCOUNT ON;
  DECLARE @v int = -1;
  SELECT @v = n FROM dbo.d WHERE id = 1;
  PRINT 'Q2 select-assign several rows: v in (11,12,13): '
    + CASE WHEN @v IN (11, 12, 13) THEN 'yes' ELSE 'no' END;
END
GO
EXEC dbo.q2;
GO

-- Q3 which statements send a rows-affected count without NOCOUNT (sqlcmd prints
-- "(n rows affected)" after the statement that sent one); UPDATE is the control.
CREATE OR ALTER PROCEDURE dbo.q3 AS
BEGIN
  SET NOCOUNT OFF;
  DECLARE @v int;
  PRINT 'Q3 counts: UPDATE t SET n = n WHERE id = 1';
  UPDATE dbo.t SET n = n WHERE id = 1;
  PRINT 'Q3 counts: SELECT @v = n FROM t WHERE id = 1';
  SELECT @v = n FROM dbo.t WHERE id = 1;
  PRINT 'Q3 counts: SELECT @v = COUNT(*) FROM t';
  SELECT @v = COUNT(*) FROM dbo.t;
  PRINT 'Q3 counts: IF EXISTS (SELECT 1 FROM t WHERE id = 1)';
  IF EXISTS (SELECT 1 FROM dbo.t WHERE id = 1) SET @v = 1;
  PRINT 'Q3 counts: SET @v = (SELECT n FROM t WHERE id = 1)';
  SET @v = (SELECT n FROM dbo.t WHERE id = 1);
  PRINT 'Q3 counts: end';
END
GO
EXEC dbo.q3;
GO

-- Q4 EXISTS is TRUE or FALSE, never UNKNOWN; its WHERE keeps rows where it is TRUE.
CREATE OR ALTER PROCEDURE dbo.q4 AS
BEGIN
  SET NOCOUNT ON;
  DECLARE @k int = NULL, @s varchar(100) = '';
  IF EXISTS (SELECT 1 FROM dbo.t WHERE n > 15) SET @s += 'a' ELSE SET @s += 'b';
  IF EXISTS (SELECT 1 FROM dbo.t WHERE n > 99) SET @s += 'c' ELSE SET @s += 'd';
  IF EXISTS (SELECT 1 FROM dbo.t WHERE n = @k) SET @s += 'e' ELSE SET @s += 'f';
  IF NOT EXISTS (SELECT 1 FROM dbo.t WHERE n = @k) SET @s += 'g' ELSE SET @s += 'h';
  IF EXISTS (SELECT 1 FROM dbo.t WHERE n IS NULL) SET @s += 'i' ELSE SET @s += 'j';
  PRINT 'Q4 exists: ' + @s;
END
GO
EXEC dbo.q4;
GO

-- Q5 a scalar subquery: NULL on zero rows, the value on one, error 512 on several.
CREATE OR ALTER PROCEDURE dbo.q5 AS
BEGIN
  SET NOCOUNT ON;
  DECLARE @v int = -1;
  SET @v = (SELECT m FROM dbo.t WHERE id = 3);
  PRINT 'Q5 scalar one row: v=' + ISNULL(CAST(@v AS varchar(11)), 'NULL');
  SET @v = -1;
  SET @v = (SELECT m FROM dbo.t WHERE id = 99);
  PRINT 'Q5 scalar zero rows: v=' + ISNULL(CAST(@v AS varchar(11)), 'NULL');
  SET @v = -1;
  BEGIN TRY
    SET @v = (SELECT n FROM dbo.d WHERE id = 1);
    PRINT 'Q5 scalar several rows: no error';
  END TRY
  BEGIN CATCH
    PRINT 'Q5 scalar several rows: error ' + CAST(ERROR_NUMBER() AS varchar(11))
      + ' sev ' + CAST(ERROR_SEVERITY() AS varchar(11)) + ', v=' + CAST(@v AS varchar(11));
  END CATCH
END
GO
EXEC dbo.q5;
GO

-- Q6 COUNT(*): an int, 0 on no rows; SELECT @c = COUNT(*) always assigns.
CREATE OR ALTER PROCEDURE dbo.q6 AS
BEGIN
  SET NOCOUNT ON;
  DECLARE @c int = -1;
  SELECT @c = COUNT(*) FROM dbo.t WHERE n > 99;
  PRINT 'Q6 count no rows: c=' + CAST(@c AS varchar(11));
  SELECT @c = COUNT(*) FROM dbo.t WHERE n > 15;
  PRINT 'Q6 count where: c=' + CAST(@c AS varchar(11));
  SET @c = (SELECT COUNT(*) FROM dbo.t);
  PRINT 'Q6 count scalar: c=' + CAST(@c AS varchar(11));
  DECLARE @sv sql_variant;
  SELECT @sv = COUNT(*) FROM dbo.t;
  PRINT 'Q6 count type: ' + CAST(SQL_VARIANT_PROPERTY(@sv, 'BaseType') AS varchar(20));
END
GO
EXEC dbo.q6;
GO

-- Q7 EXISTS does not evaluate its select list: SELECT 1/0 inside raises nothing
-- (PostgreSQL alike, see postgres.sql; the slice takes only SELECT 1 and SELECT *).
CREATE OR ALTER PROCEDURE dbo.q7 AS
BEGIN
  SET NOCOUNT ON;
  BEGIN TRY
    IF EXISTS (SELECT 1/0 FROM dbo.t WHERE id = 1) PRINT 'Q7 exists 1/0: true, no error';
  END TRY
  BEGIN CATCH
    PRINT 'Q7 exists 1/0: error ' + CAST(ERROR_NUMBER() AS varchar(11));
  END CATCH
END
GO
EXEC dbo.q7;
GO

-- Q8 SELECT @v = col with the key fixed after a write in the same procedure: the
-- read sees the procedure's own write.
CREATE OR ALTER PROCEDURE dbo.q8 AS
BEGIN
  SET NOCOUNT ON;
  DECLARE @v int = -1;
  UPDATE dbo.t SET n = 31 WHERE id = 3;
  SELECT @v = n FROM dbo.t WHERE id = 3;
  PRINT 'Q8 read own write: v=' + CAST(@v AS varchar(11));
  UPDATE dbo.t SET n = 30 WHERE id = 3;
END
GO
EXEC dbo.q8;
GO

USE master;
GO
DROP DATABASE probe_c3b;
GO
