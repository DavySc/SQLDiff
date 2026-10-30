-- Session settings a caller passes to the routines it calls (ASM-TSQL-ROWCOUNT-0),
-- SQL Server 2022.
-- Run: sqlcmd -I -i mssql.sql > mssql.out (a scratch server, never one holding data).
-- Each probe prints "Rn <name>: <observation>", so the output names no server.
-- "SET ROWCOUNT (Transact-SQL)": the setting limits the rows INSERT, UPDATE,
-- DELETE and SELECT statements touch or return; a procedure runs under the
-- setting of the session that calls it, and a SET inside the procedure lasts
-- until the procedure returns.
SET NOCOUNT ON;
GO
DROP DATABASE IF EXISTS probe_rowcount;
GO
CREATE DATABASE probe_rowcount;
GO
USE probe_rowcount;
GO
CREATE TABLE dbo.t (
  id int NOT NULL CONSTRAINT pk_t PRIMARY KEY,
  v int NULL
);
GO
CREATE OR ALTER PROCEDURE dbo.reset AS
BEGIN
  SET NOCOUNT ON;
  SET ROWCOUNT 0;
  DELETE FROM dbo.t;
  INSERT INTO dbo.t (id, v) VALUES (1, 0), (2, 0), (3, 0);
END
GO

-- R1 the finding: the caller's SET ROWCOUNT 1 limits the routine's UPDATE to
-- one row of three, and the count the routine reads afterwards sees it.
CREATE OR ALTER PROCEDURE dbo.r1 @n int OUTPUT AS
BEGIN
  SET NOCOUNT ON;
  UPDATE dbo.t SET v = 1;
  SELECT @n = COUNT(*) FROM dbo.t WHERE v = 1;
END
GO
EXEC dbo.reset;
DECLARE @n int;
EXEC dbo.r1 @n OUTPUT;
PRINT 'R1 ROWCOUNT 0 (default): rows updated, as the routine counts them: ' + CAST(@n AS varchar(11));
GO
EXEC dbo.reset;
DECLARE @n int, @all int;
SET ROWCOUNT 1;
EXEC dbo.r1 @n OUTPUT;
SET ROWCOUNT 0;
SELECT @all = COUNT(*) FROM dbo.t WHERE v = 1;
PRINT 'R1 caller SET ROWCOUNT 1: rows updated, as the routine counts them: ' + CAST(@n AS varchar(11))
  + ', as the caller counts them afterwards: ' + CAST(@all AS varchar(11));
GO

-- R2 DELETE and a multi-row INSERT in the routine, under the caller's
-- SET ROWCOUNT 1.
CREATE OR ALTER PROCEDURE dbo.r2 AS
BEGIN
  SET NOCOUNT ON;
  DELETE FROM dbo.t WHERE v = 0;
END
GO
CREATE OR ALTER PROCEDURE dbo.r2i AS
BEGIN
  SET NOCOUNT ON;
  INSERT INTO dbo.t (id, v) VALUES (11, 5), (12, 5), (13, 5);
END
GO
EXEC dbo.reset;
DECLARE @left int;
SET ROWCOUNT 1;
EXEC dbo.r2;
SET ROWCOUNT 0;
SELECT @left = COUNT(*) FROM dbo.t;
PRINT 'R2 caller SET ROWCOUNT 1: DELETE of 3 rows leaves ' + CAST(@left AS varchar(11));
GO
EXEC dbo.reset;
DECLARE @ins int;
SET ROWCOUNT 1;
EXEC dbo.r2i;
SET ROWCOUNT 0;
SELECT @ins = COUNT(*) FROM dbo.t WHERE v = 5;
PRINT 'R2 caller SET ROWCOUNT 1: INSERT of 3 rows inserts ' + CAST(@ins AS varchar(11));
GO

-- R3 SELECT in the routine under the caller's SET ROWCOUNT 1: the C3b shapes
-- (a pick on the whole key, COUNT(*), EXISTS, a scalar subquery), and a pick
-- over several rows (outside C3b) whose last assigned row the setting moves.
CREATE OR ALTER PROCEDURE dbo.r3 AS
BEGIN
  SET NOCOUNT ON;
  DECLARE @k int, @c int, @e int = 0, @s int, @last int;
  SELECT @k = v FROM dbo.t WHERE id = 3;
  SELECT @c = COUNT(*) FROM dbo.t;
  IF EXISTS (SELECT 1 FROM dbo.t WHERE id = 3) SET @e = 1;
  SET @s = (SELECT v FROM dbo.t WHERE id = 2);
  SELECT @last = id FROM dbo.t ORDER BY id;
  PRINT 'R3 key pick=' + CAST(@k AS varchar(11)) + ' count=' + CAST(@c AS varchar(11))
    + ' exists=' + CAST(@e AS varchar(11)) + ' subquery=' + CAST(@s AS varchar(11))
    + ' last of several=' + CAST(@last AS varchar(11));
END
GO
EXEC dbo.reset;
UPDATE dbo.t SET v = id * 10;
PRINT 'R3 under ROWCOUNT 0:';
EXEC dbo.r3;
SET ROWCOUNT 1;
PRINT 'R3 under the caller''s ROWCOUNT 1:';
EXEC dbo.r3;
SET ROWCOUNT 0;
GO

-- R4 a routine that begins with SET ROWCOUNT 0 runs with all rows whatever
-- the caller set, and the caller's setting is back when it returns.
CREATE OR ALTER PROCEDURE dbo.r4 @n int OUTPUT AS
BEGIN
  SET NOCOUNT ON;
  SET ROWCOUNT 0;
  UPDATE dbo.t SET v = 1;
  SELECT @n = COUNT(*) FROM dbo.t WHERE v = 1;
END
GO
EXEC dbo.reset;
DECLARE @n int, @after int;
SET ROWCOUNT 1;
EXEC dbo.r4 @n OUTPUT;
UPDATE dbo.t SET v = 2;
SET ROWCOUNT 0;
SELECT @after = COUNT(*) FROM dbo.t WHERE v = 2;
PRINT 'R4 routine SET ROWCOUNT 0 first: rows updated ' + CAST(@n AS varchar(11))
  + '; the caller''s next UPDATE, still under its ROWCOUNT 1, updates ' + CAST(@after AS varchar(11));
GO
USE master;
GO
DROP DATABASE probe_rowcount;
GO
