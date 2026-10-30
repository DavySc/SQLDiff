-- What a caller's transaction keeps when a routine without writes fails under
-- SET XACT_ABORT ON (review of C3d; probe Q15), SQL Server 2022.
-- Run: sqlcmd -I -i caller-txn-mssql.sql > caller-txn-mssql.out (a scratch server).
-- The routine only reads; it fails with 512 (a scalar subquery over a join
-- with two rows) or 8134 (division by zero). The caller wrote a row before
-- the call: does the row survive?
SET NOCOUNT ON;
GO
DROP DATABASE IF EXISTS probe_c3d_txn;
GO
CREATE DATABASE probe_c3d_txn;
GO
USE probe_c3d_txn;
GO
CREATE TABLE dbo.a (id int NOT NULL PRIMARY KEY, n int NULL, bid int NULL);
CREATE TABLE dbo.b (id int NOT NULL PRIMARY KEY, m int NULL);
CREATE TABLE dbo.log (n int NOT NULL);
INSERT INTO dbo.a VALUES (1, 10, 1), (3, 30, 2);
INSERT INTO dbo.b VALUES (1, 100), (2, 200);
GO
CREATE OR ALTER PROCEDURE dbo.look @k int, @v int OUTPUT AS
BEGIN
  SET XACT_ABORT ON;
  SET NOCOUNT ON;
  SET @v = (SELECT b.m FROM dbo.a JOIN dbo.b ON b.id = a.bid WHERE a.n > @k);
  SET @v = @v / (@k - 20);
END
GO
-- Q15a: the caller's own transaction, the error caught in the caller.
CREATE OR ALTER PROCEDURE dbo.caller @k int, @label varchar(40) AS
BEGIN
  SET NOCOUNT ON;
  DELETE FROM dbo.log;
  DECLARE @v int;
  BEGIN TRAN;
  INSERT INTO dbo.log VALUES (1);
  BEGIN TRY
    EXEC dbo.look @k, @v OUTPUT;
    PRINT 'Q15a ' + @label + ': no error';
  END TRY
  BEGIN CATCH
    PRINT 'Q15a ' + @label + ': error ' + CAST(ERROR_NUMBER() AS varchar(11))
      + ', XACT_STATE ' + CAST(XACT_STATE() AS varchar(11)) + ', @@TRANCOUNT ' + CAST(@@TRANCOUNT AS varchar(11));
  END CATCH
  BEGIN TRY
    IF @@TRANCOUNT > 0 COMMIT;
    PRINT 'Q15a ' + @label + ': COMMIT ok';
  END TRY
  BEGIN CATCH
    PRINT 'Q15a ' + @label + ': COMMIT error ' + CAST(ERROR_NUMBER() AS varchar(11));
    IF @@TRANCOUNT > 0 ROLLBACK;
  END CATCH
  DECLARE @c int = (SELECT COUNT(*) FROM dbo.log);
  PRINT 'Q15a ' + @label + ': the caller''s row survives: ' + CAST(@c AS varchar(11));
END
GO
EXEC dbo.caller 5, 'cardinality (512)';
EXEC dbo.caller 20, 'division by zero (8134)';
EXEC dbo.caller 25, 'no error';
GO
-- Q15b: no transaction open in the caller (autocommit): the row survives.
DELETE FROM dbo.log;
DECLARE @v int;
INSERT INTO dbo.log VALUES (1);
BEGIN TRY
  EXEC dbo.look 5, @v OUTPUT;
END TRY
BEGIN CATCH
  PRINT 'Q15b autocommit: error ' + CAST(ERROR_NUMBER() AS varchar(11)) + ', @@TRANCOUNT ' + CAST(@@TRANCOUNT AS varchar(11));
END CATCH
DECLARE @c int = (SELECT COUNT(*) FROM dbo.log);
PRINT 'Q15b autocommit: the caller''s row survives: ' + CAST(@c AS varchar(11));
GO
USE master;
GO
DROP DATABASE probe_c3d_txn;
GO
