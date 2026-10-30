-- Query blocks: several tables, inner and cross joins, aliases, qualified names
-- (phase C3d part 1 of docs/verified-conversion.md), SQL Server 2022.
-- Run: sqlcmd -I -i blocks-mssql.sql > blocks-mssql.out (a scratch server, never one
-- holding data). Each probe prints "Qn <name>: <observation>". The statements are
-- the T-SQL sides of the shapes `sqldiff convert --to postgres` writes.
SET NOCOUNT ON;
GO
DROP DATABASE IF EXISTS probe_c3d;
GO
CREATE DATABASE probe_c3d;
GO
USE probe_c3d;
GO
CREATE TABLE dbo.a (id int NOT NULL CONSTRAINT pk_a PRIMARY KEY, n int NULL, bid int NULL);
CREATE TABLE dbo.b (id int NOT NULL CONSTRAINT pk_b PRIMARY KEY, m int NULL);
CREATE TABLE dbo.c (k1 int NOT NULL, k2 int NOT NULL, v int NULL, CONSTRAINT pk_c PRIMARY KEY (k1, k2));
INSERT INTO dbo.a (id, n, bid) VALUES (1, 10, 1), (2, 20, NULL), (3, 30, 2), (4, NULL, 2);
INSERT INTO dbo.b (id, m) VALUES (1, 100), (2, 200), (3, NULL);
INSERT INTO dbo.c (k1, k2, v) VALUES (1, 1, 11), (1, 2, 12), (2, 1, 21);
GO

-- Q11 inner and cross joins: the rows are those of the product where every ON
-- and the WHERE are TRUE (ON conditions pool with the WHERE); a NULL join key
-- matches nothing; a self-join through two aliases; picks by key across a join.
CREATE OR ALTER PROCEDURE dbo.q11 AS
BEGIN
  SET NOCOUNT ON;
  DECLARE @c1 int, @c2 int, @c3 int, @c4 int;
  SELECT @c1 = COUNT(*) FROM dbo.a JOIN dbo.b ON a.bid = b.id WHERE b.m > 100;
  SELECT @c2 = COUNT(*) FROM dbo.a, dbo.b WHERE a.bid = b.id AND b.m > 100;
  SELECT @c3 = COUNT(*) FROM dbo.a CROSS JOIN dbo.b WHERE a.bid = b.id AND b.m > 100;
  SELECT @c4 = COUNT(*) FROM dbo.a INNER JOIN dbo.b ON a.bid = b.id AND b.m > 100;
  PRINT 'Q11 on-pooling: join-on-where=' + CAST(@c1 AS varchar(11)) + ' comma=' + CAST(@c2 AS varchar(11))
    + ' cross=' + CAST(@c3 AS varchar(11)) + ' all-in-on=' + CAST(@c4 AS varchar(11));
  SELECT @c1 = COUNT(*) FROM dbo.a CROSS JOIN dbo.b;
  SELECT @c2 = COUNT(*) FROM dbo.a, dbo.b, dbo.c;
  PRINT 'Q11 product: a x b=' + CAST(@c1 AS varchar(11)) + ' a x b x c=' + CAST(@c2 AS varchar(11));
  SELECT @c1 = COUNT(*) FROM dbo.a JOIN dbo.b ON a.bid = b.id WHERE a.id = 2;
  SELECT @c2 = COUNT(*) FROM dbo.a AS x JOIN dbo.a AS y ON x.bid = y.bid WHERE x.id = 2;
  PRINT 'Q11 null join key: a.id=2 joins=' + CAST(@c1 AS varchar(11)) + ' self on NULL=' + CAST(@c2 AS varchar(11));
  SELECT @c1 = COUNT(*) FROM dbo.a AS x JOIN dbo.a AS y ON x.bid = y.id;
  SELECT @c2 = COUNT(*) FROM dbo.a AS x JOIN dbo.a AS y ON y.id = x.id;
  PRINT 'Q11 self-join: x.bid=y.id rows=' + CAST(@c1 AS varchar(11)) + ' y.id=x.id rows=' + CAST(@c2 AS varchar(11));
  DECLARE @n int = -1, @m int = -1;
  SELECT @n = a.n, @m = b.m FROM dbo.a JOIN dbo.b ON b.id = a.bid WHERE a.id = 3;
  PRINT 'Q11 pick by key across a join: n=' + ISNULL(CAST(@n AS varchar(11)), 'NULL') + ' m=' + ISNULL(CAST(@m AS varchar(11)), 'NULL');
  SET @n = -1; SET @m = -1;
  SELECT @n = a.n, @m = b.m FROM dbo.a JOIN dbo.b ON b.id = a.bid WHERE a.id = 2;
  PRINT 'Q11 pick, no row (NULL join key): n=' + ISNULL(CAST(@n AS varchar(11)), 'NULL') + ' m=' + ISNULL(CAST(@m AS varchar(11)), 'NULL');
  SET @n = -1;
  SELECT @n = y.n FROM dbo.a AS x JOIN dbo.a AS y ON y.id = x.bid WHERE x.id = 4;
  PRINT 'Q11 pick through a self-join: n=' + ISNULL(CAST(@n AS varchar(11)), 'NULL');
  SET @n = -1;
  SELECT @n = c.v FROM dbo.a, dbo.c WHERE c.k1 = a.bid AND c.k2 = 1 AND a.id = 3;
  PRINT 'Q11 pick, two-column key fixed from another table: v=' + ISNULL(CAST(@n AS varchar(11)), 'NULL');
  IF EXISTS (SELECT 1 FROM dbo.a JOIN dbo.b ON a.bid = b.id WHERE b.m IS NULL) PRINT 'Q11 exists over a join: TRUE';
  ELSE PRINT 'Q11 exists over a join: FALSE';
  IF EXISTS (SELECT * FROM dbo.a CROSS JOIN dbo.b WHERE a.id = b.id) PRINT 'Q11 exists over a cross join: TRUE';
  ELSE PRINT 'Q11 exists over a cross join: FALSE';
END
GO
EXEC dbo.q11;
GO

-- Q12 names: an unqualified name in two tables (209), an aliased table's own
-- name as a qualifier (4104), two sources with one exposed name (1013), an ON
-- that names a table joined later (4104); the alias hides the table name, and a
-- name resolves to the one table that has it. Each statement is compiled on
-- its own (sp_executesql), so the error is the compile error.
CREATE OR ALTER PROCEDURE dbo.q12_try @sql nvarchar(max), @label varchar(100) AS
BEGIN
  SET NOCOUNT ON;
  BEGIN TRY
    EXEC sp_executesql @sql;
    PRINT 'Q12 ' + @label + ': no error';
  END TRY
  BEGIN CATCH
    PRINT 'Q12 ' + @label + ': error ' + CAST(ERROR_NUMBER() AS varchar(11));
  END CATCH
END
GO
EXEC dbo.q12_try N'DECLARE @v int; SELECT @v = id FROM dbo.a JOIN dbo.b ON a.bid = b.id;', 'unqualified name in two tables';
EXEC dbo.q12_try N'DECLARE @v int; SELECT @v = x.n FROM dbo.a AS x WHERE a.id = 1;', 'aliased table''s own name';
EXEC dbo.q12_try N'DECLARE @v int; SELECT @v = COUNT(*) FROM dbo.a JOIN dbo.a ON a.id = a.bid;', 'one exposed name twice';
EXEC dbo.q12_try N'DECLARE @v int; SELECT @v = COUNT(*) FROM dbo.a JOIN dbo.b ON c.k1 = a.id JOIN dbo.c ON c.k2 = b.id;', 'ON names a later table';
EXEC dbo.q12_try N'DECLARE @v int; SELECT @v = COUNT(*) FROM dbo.a, dbo.b JOIN dbo.c ON c.k1 = a.id;', 'ON names a comma table';
EXEC dbo.q12_try N'DECLARE @v int; SELECT @v = COUNT(*) FROM dbo.a AS b, dbo.b AS a WHERE a.m = 100 AND b.n = 10;', 'aliases swap the table names';
GO
CREATE OR ALTER PROCEDURE dbo.q12 AS
BEGIN
  SET NOCOUNT ON;
  DECLARE @v int = -1, @w int = -1;
  SELECT @v = b.n, @w = a.m FROM dbo.a AS b, dbo.b AS a WHERE b.id = 1 AND a.id = 2;
  PRINT 'Q12 alias hides the table name: b.n=' + CAST(@v AS varchar(11)) + ' a.m=' + CAST(@w AS varchar(11));
  SELECT @v = n, @w = m FROM dbo.a JOIN dbo.b ON a.bid = b.id WHERE a.id = 3;
  PRINT 'Q12 unqualified names of one table each: n=' + CAST(@v AS varchar(11)) + ' m=' + CAST(@w AS varchar(11));
END
GO
EXEC dbo.q12;
GO
-- Q12 case: in a case-sensitive database, a qualifier or a column spelt in
-- another case than the alias or the column does not resolve (4104, 207), so
-- the rule asks the spelling to be exact.
DROP DATABASE IF EXISTS probe_c3d_cs;
GO
CREATE DATABASE probe_c3d_cs COLLATE Latin1_General_100_CS_AS;
GO
USE probe_c3d_cs;
GO
CREATE TABLE dbo.a (id int NOT NULL PRIMARY KEY, n int NULL);
INSERT INTO dbo.a (id, n) VALUES (1, 10);
GO
CREATE OR ALTER PROCEDURE dbo.q12_try @sql nvarchar(max), @label varchar(100) AS
BEGIN
  SET NOCOUNT ON;
  BEGIN TRY
    EXEC sp_executesql @sql;
    PRINT 'Q12 ' + @label + ': no error';
  END TRY
  BEGIN CATCH
    PRINT 'Q12 ' + @label + ': error ' + CAST(ERROR_NUMBER() AS varchar(11));
  END CATCH
END
GO
EXEC dbo.q12_try N'DECLARE @v int; SELECT @v = x.n FROM dbo.a AS x WHERE x.id = 1;', 'CS database, alias as spelt';
EXEC dbo.q12_try N'DECLARE @v int; SELECT @v = X.n FROM dbo.a AS x WHERE x.id = 1;', 'CS database, alias in another case';
EXEC dbo.q12_try N'DECLARE @v int; SELECT @v = x.N FROM dbo.a AS x WHERE x.id = 1;', 'CS database, column in another case';
GO
USE probe_c3d;
GO

-- Q13 a scalar subquery over a join: NULL on zero rows, the value on one, error
-- 512 on two (the variable kept); under XACT_ABORT ON the error ends the
-- procedure (the caller catches it).
CREATE OR ALTER PROCEDURE dbo.q13 @k int AS
BEGIN
  SET NOCOUNT ON;
  DECLARE @v int = -1;
  BEGIN TRY
    SET @v = (SELECT b.m FROM dbo.a JOIN dbo.b ON b.id = a.bid WHERE a.n > @k);
    PRINT 'Q13 scalar over a join, n > ' + CAST(@k AS varchar(11)) + ': v=' + ISNULL(CAST(@v AS varchar(11)), 'NULL');
  END TRY
  BEGIN CATCH
    PRINT 'Q13 scalar over a join, n > ' + CAST(@k AS varchar(11)) + ': error ' + CAST(ERROR_NUMBER() AS varchar(11))
      + ', v=' + CAST(@v AS varchar(11));
  END CATCH
END
GO
EXEC dbo.q13 @k = 30;
EXEC dbo.q13 @k = 20;
EXEC dbo.q13 @k = 5;
GO
CREATE OR ALTER PROCEDURE dbo.q13x @k int, @v int OUTPUT AS
BEGIN
  SET XACT_ABORT ON;
  SET NOCOUNT ON;
  SET @v = (SELECT b.m FROM dbo.a JOIN dbo.b ON b.id = a.bid WHERE a.n > @k);
  PRINT 'Q13 after the scalar subquery (not reached on error)';
END
GO
DECLARE @v int = -1;
BEGIN TRY
  EXEC dbo.q13x @k = 5, @v = @v OUTPUT;
END TRY
BEGIN CATCH
  PRINT 'Q13 XACT_ABORT ON, two rows: error ' + CAST(ERROR_NUMBER() AS varchar(11)) + ' reaches the caller, v=' + CAST(@v AS varchar(11));
END CATCH
GO

USE master;
GO
DROP DATABASE probe_c3d;
DROP DATABASE probe_c3d_cs;
GO
