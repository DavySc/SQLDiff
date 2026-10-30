-- A call spelt in another case than the callee's declaration (callee
-- spelling, after the review of C3d), SQL Server 2022.
-- Run: sqlcmd -I -i callee-case-mssql.sql > callee-case-mssql.out (a scratch server).
-- dbo.look is declared in lower case; the caller writes dbo.LOOK or DBO.look.
-- In a case-sensitive database neither reaches it (2812); in a
-- case-insensitive one both do. PostgreSQL folds the unquoted names
-- (callee-case-postgres.sql), so the call always reaches look there.
SET NOCOUNT ON;
GO
DROP DATABASE IF EXISTS probe_callee_cs;
DROP DATABASE IF EXISTS probe_callee_ci;
GO
CREATE DATABASE probe_callee_cs COLLATE Latin1_General_100_CS_AS;
CREATE DATABASE probe_callee_ci COLLATE Latin1_General_100_CI_AS;
GO
USE probe_callee_cs;
GO
CREATE PROCEDURE dbo.look @x int OUTPUT AS BEGIN SET @x = 7; END;
GO
CREATE PROCEDURE dbo.try_call @sql nvarchar(max), @label varchar(100) AS
BEGIN
  SET NOCOUNT ON;
  DECLARE @v int;
  BEGIN TRY
    EXEC sp_executesql @sql, N'@v int OUTPUT', @v OUTPUT;
    PRINT 'callee ' + @label + ': no error, @v ' + ISNULL(CAST(@v AS varchar(10)), 'NULL');
  END TRY
  BEGIN CATCH
    PRINT 'callee ' + @label + ': error ' + CAST(ERROR_NUMBER() AS varchar(10));
  END CATCH
END;
GO
EXEC dbo.try_call N'EXEC dbo.look @v OUTPUT', 'case-sensitive, dbo.look';
EXEC dbo.try_call N'EXEC dbo.LOOK @v OUTPUT', 'case-sensitive, dbo.LOOK';
EXEC dbo.try_call N'EXEC DBO.look @v OUTPUT', 'case-sensitive, DBO.look';
GO
USE probe_callee_ci;
GO
CREATE PROCEDURE dbo.look @x int OUTPUT AS BEGIN SET @x = 7; END;
GO
CREATE PROCEDURE dbo.try_call @sql nvarchar(max), @label varchar(100) AS
BEGIN
  SET NOCOUNT ON;
  DECLARE @v int;
  BEGIN TRY
    EXEC sp_executesql @sql, N'@v int OUTPUT', @v OUTPUT;
    PRINT 'callee ' + @label + ': no error, @v ' + ISNULL(CAST(@v AS varchar(10)), 'NULL');
  END TRY
  BEGIN CATCH
    PRINT 'callee ' + @label + ': error ' + CAST(ERROR_NUMBER() AS varchar(10));
  END CATCH
END;
GO
EXEC dbo.try_call N'EXEC dbo.LOOK @v OUTPUT', 'case-insensitive, dbo.LOOK';
EXEC dbo.try_call N'EXEC DBO.look @v OUTPUT', 'case-insensitive, DBO.look';
GO
USE master;
GO
DROP DATABASE probe_callee_cs;
DROP DATABASE probe_callee_ci;
GO
