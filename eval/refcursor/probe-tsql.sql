-- Probes of SQL Server 2022 behaviour of a procedure with several result sets (burndown K, D8).
-- Run: sqlcmd -S localhost -U sa -P ... -C -I -W -i probe-tsql.sql
SET NOCOUNT ON;
IF OBJECT_ID('dbo.t') IS NOT NULL DROP TABLE dbo.t;
CREATE TABLE dbo.t (id int PRIMARY KEY, name varchar(20));
INSERT INTO dbo.t VALUES (1,'a'),(2,'b'),(3,'c');
GO
CREATE OR ALTER PROCEDURE dbo.p_rs AS
BEGIN
  SELECT id, name FROM dbo.t ORDER BY id;
  UPDATE dbo.t SET name = 'changed' WHERE id = 1;
  INSERT INTO dbo.t VALUES (4, 'new');
  DELETE FROM dbo.t WHERE id = 3;
  SELECT id, name FROM dbo.t ORDER BY id;
END;
GO
PRINT '== Q1: the first result set shows the table before the later writes, in order';
EXEC dbo.p_rs;
GO
PRINT '== Q2: a runtime error in a SELECT after a write: the write is kept (autocommit, XACT_ABORT OFF), the rows before the error were sent';
TRUNCATE TABLE dbo.t; INSERT INTO dbo.t VALUES (1,'a'),(2,'b'),(3,'c');
GO
CREATE OR ALTER PROCEDURE dbo.p_err AS
BEGIN
  UPDATE dbo.t SET name = 'kept' WHERE id = 1;
  SELECT 1/(id-2) AS q FROM dbo.t ORDER BY id;
END;
GO
EXEC dbo.p_err;
SELECT id, name FROM dbo.t ORDER BY id;
GO
CREATE OR ALTER PROCEDURE dbo.p_if @f int AS
BEGIN
  IF @f = 1 SELECT 'first';
  SELECT 'second';
END;
GO
PRINT '== Q3: a result set that no statement reached is not sent; later numbering shifts';
EXEC dbo.p_if 0;
GO
