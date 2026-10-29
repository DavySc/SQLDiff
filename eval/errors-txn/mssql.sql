-- Errors and transactions (phase C4 of docs/verified-conversion.md), SQL Server 2022.
-- Run: sqlcmd -I -i mssql.sql > mssql.out (a scratch server, never one holding data).
-- C4 probes, SQL Server. Each probe prints "Pn <name>: <observation>".
SET NOCOUNT ON;
GO
DROP DATABASE IF EXISTS probe_c4;
GO
CREATE DATABASE probe_c4;
GO
USE probe_c4;
GO
CREATE TABLE dbo.t (id int NOT NULL CONSTRAINT pk_t PRIMARY KEY, n int NULL, m int NOT NULL DEFAULT 0);
GO

-- P1 control: an error skips the rest of TRY and runs CATCH; no error skips CATCH.
CREATE OR ALTER PROCEDURE dbo.p1 AS
BEGIN
  DECLARE @s varchar(100) = '';
  BEGIN TRY SET @s += 'a'; SET @s += CAST(1/0 AS varchar(9)); SET @s += 'b'; END TRY
  BEGIN CATCH SET @s += 'c'; END CATCH
  SET @s += '|';
  BEGIN TRY SET @s += 'd'; END TRY BEGIN CATCH SET @s += 'e'; END CATCH
  PRINT 'P1 control: ' + @s;
END
GO
EXEC dbo.p1;
GO

-- P2 an error in CATCH leaves it and reaches an enclosing TRY.
CREATE OR ALTER PROCEDURE dbo.p2 AS
BEGIN
  DECLARE @s varchar(100) = '';
  BEGIN TRY
    BEGIN TRY SELECT 1/0; END TRY
    BEGIN CATCH SET @s += 'inner-catch;'; SELECT 1/0; SET @s += 'after-error-in-catch;'; END CATCH
  END TRY
  BEGIN CATCH SET @s += 'outer-catch ' + CAST(ERROR_NUMBER() AS varchar(9)); END CATCH
  PRINT 'P2 error-in-catch: ' + @s;
END
GO
EXEC dbo.p2;
GO

-- P3 variables keep the value the error left.
CREATE OR ALTER PROCEDURE dbo.p3 AS
BEGIN
  DECLARE @x int = 0;
  BEGIN TRY SET @x = 1; SET @x = 2147483647 + @x; END TRY
  BEGIN CATCH PRINT 'P3 variables: x=' + CAST(@x AS varchar(9)); END CATCH
END
GO
EXEC dbo.p3;
GO

-- P4 the database after a caught error, autocommit, XACT_ABORT OFF and ON.
CREATE OR ALTER PROCEDURE dbo.p4 @abort bit AS
BEGIN
  IF @abort = 1 SET XACT_ABORT ON ELSE SET XACT_ABORT OFF;
  DELETE FROM dbo.t;
  BEGIN TRY
    INSERT INTO dbo.t (id, n) VALUES (1, 1);
    INSERT INTO dbo.t (id, n) VALUES (1, 2);   -- 2627
  END TRY
  BEGIN CATCH
    DECLARE @st int = XACT_STATE(), @tc int = @@TRANCOUNT;
    INSERT INTO dbo.t (id, n) VALUES (9, 9);   -- a write in CATCH
    PRINT 'P4 db-after-catch xact_abort=' + CAST(@abort AS char(1)) + ': caught '
      + CAST(ERROR_NUMBER() AS varchar(9)) + ' sev ' + CAST(ERROR_SEVERITY() AS varchar(9))
      + ', XACT_STATE=' + CAST(@st AS varchar(9)) + ', TRANCOUNT=' + CAST(@tc AS varchar(9));
  END CATCH
  DECLARE @rows varchar(100) = '';
  SELECT @rows += CAST(id AS varchar(9)) + ' ' FROM dbo.t ORDER BY id;
  PRINT 'P4 db-after-catch xact_abort=' + CAST(@abort AS char(1)) + ': rows ' + @rows;
END
GO
EXEC dbo.p4 0;
EXEC dbo.p4 1;
GO

-- P5 a caller transaction around a caught error under XACT_ABORT ON: doomed.
CREATE OR ALTER PROCEDURE dbo.p5 AS
BEGIN
  SET XACT_ABORT ON;
  BEGIN TRY SELECT 1/0; END TRY
  BEGIN CATCH
    PRINT 'P5 caller-txn: XACT_STATE in CATCH=' + CAST(XACT_STATE() AS varchar(9));
    BEGIN TRY INSERT INTO dbo.t (id, n) VALUES (50, 50); PRINT 'P5 caller-txn: write in CATCH ok'; END TRY
    BEGIN CATCH PRINT 'P5 caller-txn: write in CATCH failed ' + CAST(ERROR_NUMBER() AS varchar(9)); END CATCH
  END CATCH
END
GO
DELETE FROM dbo.t;
BEGIN TRAN;
EXEC dbo.p5;
PRINT 'P5 caller-txn: after call XACT_STATE=' + CAST(XACT_STATE() AS varchar(9)) + ' TRANCOUNT=' + CAST(@@TRANCOUNT AS varchar(9));
IF XACT_STATE() <> 0 ROLLBACK;
GO
-- P5b the same caller transaction, XACT_ABORT OFF in the procedure.
CREATE OR ALTER PROCEDURE dbo.p5b AS
BEGIN
  SET XACT_ABORT OFF;
  BEGIN TRY SELECT 1/0; END TRY
  BEGIN CATCH
    PRINT 'P5b caller-txn abort-off: XACT_STATE in CATCH=' + CAST(XACT_STATE() AS varchar(9));
  END CATCH
END
GO
BEGIN TRAN;
EXEC dbo.p5b;
IF XACT_STATE() <> 0 ROLLBACK;
GO

-- P6 which errors CATCH catches, with number and severity.
CREATE OR ALTER PROCEDURE dbo.p6 @k int AS
BEGIN
  DECLARE @i int;
  DELETE FROM dbo.t; INSERT INTO dbo.t (id, n) VALUES (1, 1);
  BEGIN TRY
    IF @k = 1 SET @i = 1 / 0;
    IF @k = 2 SET @i = 2147483647 + 1;
    IF @k = 3 INSERT INTO dbo.t (id, n, m) VALUES (2, 1, NULL);
    IF @k = 4 INSERT INTO dbo.t (id, n) VALUES (1, 1);
    IF @k = 5 THROW 50001, 'm', 1;
    PRINT 'P6 catch k=' + CAST(@k AS varchar(9)) + ': no error';
  END TRY
  BEGIN CATCH
    PRINT 'P6 catch k=' + CAST(@k AS varchar(9)) + ': caught ' + CAST(ERROR_NUMBER() AS varchar(9))
      + ' sev ' + CAST(ERROR_SEVERITY() AS varchar(9)) + ' state ' + CAST(ERROR_STATE() AS varchar(9));
  END CATCH
END
GO
EXEC dbo.p6 1; EXEC dbo.p6 2; EXEC dbo.p6 3; EXEC dbo.p6 4; EXEC dbo.p6 5;
GO

-- P7 THROW without TRY ends the procedure, XACT_ABORT OFF.
CREATE OR ALTER PROCEDURE dbo.p7 AS
BEGIN
  SET XACT_ABORT OFF;
  DELETE FROM dbo.t;
  INSERT INTO dbo.t (id, n) VALUES (1, 1);
  THROW 50002, 'stop', 3;
  INSERT INTO dbo.t (id, n) VALUES (2, 2);
END
GO
BEGIN TRY EXEC dbo.p7; END TRY
BEGIN CATCH PRINT 'P7 throw-ends: caller caught ' + CAST(ERROR_NUMBER() AS varchar(9)) + ' sev ' + CAST(ERROR_SEVERITY() AS varchar(9)); END CATCH
DECLARE @rows varchar(100) = ''; SELECT @rows += CAST(id AS varchar(9)) + ' ' FROM dbo.t ORDER BY id;
PRINT 'P7 throw-ends: rows ' + @rows;
GO
-- P7b for contrast: a statement error without TRY, XACT_ABORT OFF, does not end the procedure.
CREATE OR ALTER PROCEDURE dbo.p7b AS
BEGIN
  SET XACT_ABORT OFF;
  DELETE FROM dbo.t;
  INSERT INTO dbo.t (id, n) VALUES (1, 1);
  INSERT INTO dbo.t (id, n) VALUES (1, 1);   -- 2627
  INSERT INTO dbo.t (id, n) VALUES (2, 2);
END
GO
BEGIN TRY EXEC dbo.p7b; END TRY
BEGIN CATCH PRINT 'P7b statement-error: caller caught ' + CAST(ERROR_NUMBER() AS varchar(9)); END CATCH
DECLARE @rows varchar(100) = ''; SELECT @rows += CAST(id AS varchar(9)) + ' ' FROM dbo.t ORDER BY id;
PRINT 'P7b statement-error abort-off: rows ' + @rows;
GO
-- P7c THROW with arguments out of range.
BEGIN TRY THROW 49999, 'x', 1; END TRY
BEGIN CATCH PRINT 'P7c throw 49999: ' + CAST(ERROR_NUMBER() AS varchar(9)) + ' ' + ERROR_MESSAGE(); END CATCH
GO

-- P8 THROW; re-raises the same error.
CREATE OR ALTER PROCEDURE dbo.p8 AS
BEGIN
  BEGIN TRY SELECT 1/0; END TRY
  BEGIN CATCH THROW; END CATCH
END
GO
BEGIN TRY EXEC dbo.p8; END TRY
BEGIN CATCH PRINT 'P8 rethrow: caller caught ' + CAST(ERROR_NUMBER() AS varchar(9)) + ' sev ' + CAST(ERROR_SEVERITY() AS varchar(9)); END CATCH
GO

-- P9 BEGIN TRAN ... COMMIT under XACT_ABORT ON, error inside: all rolled back, procedure ends.
CREATE OR ALTER PROCEDURE dbo.p9 AS
BEGIN
  SET XACT_ABORT ON;
  DELETE FROM dbo.t;
  INSERT INTO dbo.t (id, n) VALUES (100, 0);
  BEGIN TRAN;
    INSERT INTO dbo.t (id, n) VALUES (1, 1);
    INSERT INTO dbo.t (id, n) VALUES (1, 1);   -- 2627
    INSERT INTO dbo.t (id, n) VALUES (2, 2);
  COMMIT;
  INSERT INTO dbo.t (id, n) VALUES (3, 3);
END
GO
BEGIN TRY EXEC dbo.p9; END TRY
BEGIN CATCH PRINT 'P9 bracket: caller caught ' + CAST(ERROR_NUMBER() AS varchar(9)) + ', TRANCOUNT=' + CAST(@@TRANCOUNT AS varchar(9)); END CATCH
DECLARE @rows varchar(100) = ''; SELECT @rows += CAST(id AS varchar(9)) + ' ' FROM dbo.t ORDER BY id;
PRINT 'P9 bracket: rows ' + @rows;
GO

-- P10 BEGIN TRAN without COMMIT: error 266 at the end of the procedure.
CREATE OR ALTER PROCEDURE dbo.p10 AS
BEGIN
  BEGIN TRAN;
  INSERT INTO dbo.t (id, n) VALUES (77, 7);
END
GO
BEGIN TRY EXEC dbo.p10; PRINT 'P10 unclosed: no error'; END TRY
BEGIN CATCH PRINT 'P10 unclosed: caught ' + CAST(ERROR_NUMBER() AS varchar(9)) + ', TRANCOUNT=' + CAST(@@TRANCOUNT AS varchar(9)); END CATCH
IF @@TRANCOUNT > 0 ROLLBACK;
GO

-- P11 the rollback-and-rethrow idiom.
CREATE OR ALTER PROCEDURE dbo.p11 AS
BEGIN
  SET XACT_ABORT ON;
  BEGIN TRY
    BEGIN TRAN;
      INSERT INTO dbo.t (id, n) VALUES (1, 1);
      INSERT INTO dbo.t (id, n) VALUES (1, 1);   -- 2627
    COMMIT;
  END TRY
  BEGIN CATCH
    IF @@TRANCOUNT > 0 ROLLBACK;
    THROW;
  END CATCH
END
GO
DELETE FROM dbo.t;
BEGIN TRY EXEC dbo.p11; END TRY
BEGIN CATCH PRINT 'P11 idiom: caller caught ' + CAST(ERROR_NUMBER() AS varchar(9)) + ', TRANCOUNT=' + CAST(@@TRANCOUNT AS varchar(9)); END CATCH
DECLARE @rows varchar(100) = ''; SELECT @rows += CAST(id AS varchar(9)) + ' ' FROM dbo.t ORDER BY id;
PRINT 'P11 idiom: rows [' + @rows + ']';
GO

-- P12 statement atomicity: multi-row INSERT with a duplicate in row 2; UPDATE overflowing on row 2.
DELETE FROM dbo.t;
BEGIN TRY INSERT INTO dbo.t (id, n) VALUES (1, 1), (1, 2), (3, 3); END TRY BEGIN CATCH END CATCH
DECLARE @rows varchar(100) = ''; SELECT @rows += CAST(id AS varchar(9)) + ' ' FROM dbo.t ORDER BY id;
PRINT 'P12 atomic insert: rows [' + @rows + ']';
GO
DELETE FROM dbo.t; INSERT INTO dbo.t (id, n) VALUES (1, 1), (2, 2147483647);
BEGIN TRY UPDATE dbo.t SET n = n + 1; END TRY BEGIN CATCH PRINT 'P12 atomic update: caught ' + CAST(ERROR_NUMBER() AS varchar(9)); END CATCH
DECLARE @rows varchar(100) = ''; SELECT @rows += CAST(n AS varchar(12)) + ' ' FROM dbo.t ORDER BY id;
PRINT 'P12 atomic update: n [' + @rows + ']';
GO

-- P13 C3a "reviewers check": SET reads the old values (swap), T-SQL columns.
DELETE FROM dbo.t; INSERT INTO dbo.t (id, n, m) VALUES (1, 10, 20);
UPDATE dbo.t SET n = m, m = n;
DECLARE @r varchar(100); SELECT @r = CAST(n AS varchar(9)) + ',' + CAST(m AS varchar(9)) FROM dbo.t;
PRINT 'P13 update-old-values: n,m=' + @r;
GO
-- P9c the bracket, caller without TRY: the batch ends and the transaction is rolled back.
EXEC dbo.p9;
PRINT 'P9c not reached in this batch';
GO
DECLARE @rows varchar(100) = ''; SELECT @rows += CAST(id AS varchar(9)) + ' ' FROM dbo.t ORDER BY id;
PRINT 'P9c bracket, caller without TRY: next batch TRANCOUNT=' + CAST(@@TRANCOUNT AS varchar(9)) + ' rows [' + @rows + ']';
GO
-- P9d the caller catches, then writes, then looks again.
BEGIN TRY EXEC dbo.p9; END TRY
BEGIN CATCH
  PRINT 'P9d caller caught ' + CAST(ERROR_NUMBER() AS varchar(9)) + ' XACT_STATE=' + CAST(XACT_STATE() AS varchar(9));
END CATCH
BEGIN TRY INSERT INTO dbo.t (id, n) VALUES (500, 5); PRINT 'P9d caller write ok'; END TRY
BEGIN CATCH PRINT 'P9d caller write failed ' + CAST(ERROR_NUMBER() AS varchar(9)); END CATCH
IF XACT_STATE() <> 0 ROLLBACK;
DECLARE @rows varchar(100) = ''; SELECT @rows += CAST(id AS varchar(9)) + ' ' FROM dbo.t ORDER BY id;
PRINT 'P9d after caller ROLLBACK: rows [' + @rows + ']';
GO
-- P7d a statement error, XACT_ABORT OFF, caller without TRY: the procedure carries on.
EXEC dbo.p7b;
GO
DECLARE @rows varchar(100) = ''; SELECT @rows += CAST(id AS varchar(9)) + ' ' FROM dbo.t ORDER BY id;
PRINT 'P7d statement-error abort-off, caller without TRY: rows [' + @rows + ']';
GO
-- P4c XACT_ABORT ON in autocommit, error outside any TRY: earlier statements kept?
CREATE OR ALTER PROCEDURE dbo.p4c AS
BEGIN
  SET XACT_ABORT ON;
  DELETE FROM dbo.t;
  INSERT INTO dbo.t (id, n) VALUES (1, 1);
  INSERT INTO dbo.t (id, n) VALUES (1, 1);
  INSERT INTO dbo.t (id, n) VALUES (2, 2);
END
GO
EXEC dbo.p4c;
GO
DECLARE @rows varchar(100) = ''; SELECT @rows += CAST(id AS varchar(9)) + ' ' FROM dbo.t ORDER BY id;
PRINT 'P4c xact_abort on, autocommit, no TRY: rows [' + @rows + ']';
GO
