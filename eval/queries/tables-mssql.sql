-- Read-only catalog tables (phase C3d part 2 of docs/verified-conversion.md),
-- SQL Server 2022. Run: sqlcmd -I -i tables-mssql.sql > tables-mssql.out (a
-- scratch server, never one holding data; it creates and drops probe_c3d2).
-- Each probe prints "Qn <name>: <observation>". The statements are the T-SQL
-- sides of the shapes `sqldiff convert --to postgres` writes.
SET NOCOUNT ON;
GO
DROP DATABASE IF EXISTS probe_c3d2;
GO
CREATE DATABASE probe_c3d2;
GO
USE probe_c3d2;
GO
-- A table with what reads do not see: identity, UNIQUE, a foreign key, CHECK,
-- an expression default, decimal and datetime2(7) columns, a non-persisted
-- computed column that raises on every row (1/0) and a persisted one; and its
-- plain twin with the same rows in the columns the reads name.
CREATE TABLE dbo.item (id int IDENTITY(1,1) NOT NULL CONSTRAINT pk_item PRIMARY KEY,
  name varchar(20) NOT NULL CONSTRAINT uq_item_name UNIQUE, stock decimal(12,2) NOT NULL);
CREATE TABLE dbo.line (ord_id int NOT NULL, line_no int NOT NULL,
  item_id int NOT NULL CONSTRAINT fk_line_item REFERENCES dbo.item (id),
  qty int NOT NULL CONSTRAINT ck_line_qty CHECK (qty > 0), price decimal(12,2) NOT NULL,
  added datetime2 NOT NULL CONSTRAINT df_line_added DEFAULT SYSDATETIME(),
  boom AS (1 / (qty - qty)), twice AS (qty * 2) PERSISTED,
  CONSTRAINT pk_line PRIMARY KEY (ord_id, line_no));
CREATE INDEX ix_line_item ON dbo.line (item_id) INCLUDE (price);
CREATE TABLE dbo.item_plain (id int NOT NULL PRIMARY KEY, name varchar(20) NOT NULL);
CREATE TABLE dbo.line_plain (ord_id int NOT NULL, line_no int NOT NULL, item_id int NOT NULL,
  qty int NOT NULL, CONSTRAINT pk_line_plain PRIMARY KEY (ord_id, line_no));
INSERT INTO dbo.item (name, stock) VALUES ('a', 1.50), ('b', 2.25), ('c', 0);
INSERT INTO dbo.line (ord_id, line_no, item_id, qty, price) VALUES
  (1, 1, 1, 5, 9.99), (1, 2, 2, 7, 1.00), (2, 1, 1, 9, 3.50);
INSERT INTO dbo.item_plain (id, name) VALUES (1, 'a'), (2, 'b'), (3, 'c');
INSERT INTO dbo.line_plain (ord_id, line_no, item_id, qty) VALUES (1, 1, 1, 5), (1, 2, 2, 7), (2, 1, 1, 9);
GO

-- Q16 reads of a table with constraints, identity, defaults, computed and
-- out-of-slice columns give what the plain table with the same rows gives:
-- COUNT, EXISTS (SELECT 1 and SELECT *), a pick by the whole key across a
-- join, a scalar subquery; the raising computed column raises only where a
-- query names it; two rows share the visible half of a key.
CREATE OR ALTER PROCEDURE dbo.q16 AS
BEGIN
  SET NOCOUNT ON;
  DECLARE @c1 int, @c2 int, @q1 int = -1, @q2 int = -1, @s1 int = -1, @s2 int = -1;
  SELECT @c1 = COUNT(*) FROM dbo.line WHERE item_id = 1;
  SELECT @c2 = COUNT(*) FROM dbo.line_plain WHERE item_id = 1;
  PRINT 'Q16 count: table=' + CAST(@c1 AS varchar(11)) + ' plain=' + CAST(@c2 AS varchar(11));
  SELECT @q1 = l.qty FROM dbo.line l JOIN dbo.item i ON i.id = l.item_id WHERE l.ord_id = 1 AND l.line_no = 2;
  SELECT @q2 = l.qty FROM dbo.line_plain l JOIN dbo.item_plain i ON i.id = l.item_id WHERE l.ord_id = 1 AND l.line_no = 2;
  PRINT 'Q16 pick by key across a join: table=' + CAST(@q1 AS varchar(11)) + ' plain=' + CAST(@q2 AS varchar(11));
  SET @s1 = (SELECT l.item_id FROM dbo.line AS l WHERE l.ord_id = 2 AND l.line_no = 1);
  SET @s2 = (SELECT l.item_id FROM dbo.line_plain AS l WHERE l.ord_id = 2 AND l.line_no = 1);
  PRINT 'Q16 scalar subquery: table=' + CAST(@s1 AS varchar(11)) + ' plain=' + CAST(@s2 AS varchar(11));
  IF EXISTS (SELECT 1 FROM dbo.line WHERE qty > 8) PRINT 'Q16 exists select 1: TRUE';
  ELSE PRINT 'Q16 exists select 1: FALSE';
  IF EXISTS (SELECT * FROM dbo.line WHERE qty > 8) PRINT 'Q16 exists select * (raising computed column): TRUE';
  ELSE PRINT 'Q16 exists select * (raising computed column): FALSE';
  SELECT @c1 = COUNT(*) FROM dbo.line WHERE ord_id = 1;
  PRINT 'Q16 rows sharing the key part ord_id=1: ' + CAST(@c1 AS varchar(11));
END
GO
EXEC dbo.q16;
GO
CREATE OR ALTER PROCEDURE dbo.q16_boom AS
BEGIN
  SET NOCOUNT ON;
  DECLARE @b int = -1;
  BEGIN TRY
    SELECT @b = l.boom FROM dbo.line l WHERE l.ord_id = 1 AND l.line_no = 1;
    PRINT 'Q16 the raising column named: no error, b=' + ISNULL(CAST(@b AS varchar(11)), 'NULL');
  END TRY
  BEGIN CATCH
    PRINT 'Q16 the raising column named: error ' + CAST(ERROR_NUMBER() AS varchar(11));
  END CATCH
  SELECT @b = l.twice FROM dbo.line l WHERE l.ord_id = 1 AND l.line_no = 1;
  PRINT 'Q16 the persisted computed column: ' + CAST(@b AS varchar(11));
END
GO
EXEC dbo.q16_boom;
GO

-- Q17 a system-versioned table read with a plain FROM: the current rows only,
-- after an UPDATE and a DELETE; the history table holds the old versions.
CREATE TABLE dbo.color (id int NOT NULL CONSTRAINT pk_color PRIMARY KEY, n int NULL,
  valid_from datetime2(7) GENERATED ALWAYS AS ROW START NOT NULL,
  valid_to datetime2(7) GENERATED ALWAYS AS ROW END NOT NULL,
  PERIOD FOR SYSTEM_TIME (valid_from, valid_to))
  WITH (SYSTEM_VERSIONING = ON (HISTORY_TABLE = dbo.color_archive));
GO
INSERT INTO dbo.color (id, n) VALUES (1, 10), (2, 20), (3, 30);
UPDATE dbo.color SET n = 11 WHERE id = 1;
DELETE FROM dbo.color WHERE id = 3;
GO
CREATE OR ALTER PROCEDURE dbo.q17 AS
BEGIN
  SET NOCOUNT ON;
  DECLARE @c int, @h int, @n int = -1, @e int = -1;
  SELECT @c = COUNT(*) FROM dbo.color;
  SELECT @h = COUNT(*) FROM dbo.color_archive;
  SELECT @n = c.n FROM dbo.color c WHERE c.id = 1;
  SELECT @e = COUNT(*) FROM dbo.color WHERE id = 3;
  PRINT 'Q17 system-versioned table: count=' + CAST(@c AS varchar(11)) + ' pick id=1 n=' + CAST(@n AS varchar(11))
    + ' deleted id=3 rows=' + CAST(@e AS varchar(11)) + ' history rows=' + CAST(@h AS varchar(11));
END
GO
EXEC dbo.q17;
GO

-- Q18 a security policy's filter predicate changes what a read returns: why
-- such a table stays out. Rows owned by dbo, probe_u and someone else.
CREATE TABLE dbo.doc (id int NOT NULL PRIMARY KEY, owner sysname NOT NULL);
INSERT INTO dbo.doc (id, owner) VALUES (1, 'dbo'), (2, 'probe_u'), (3, 'x');
GO
CREATE USER probe_u WITHOUT LOGIN;
GRANT SELECT ON dbo.doc TO probe_u;
GO
CREATE FUNCTION dbo.doc_pred (@owner sysname) RETURNS TABLE WITH SCHEMABINDING AS
  RETURN SELECT 1 AS ok WHERE @owner = USER_NAME();
GO
CREATE OR ALTER PROCEDURE dbo.q18_count @label varchar(40) AS
BEGIN
  SET NOCOUNT ON;
  DECLARE @c int;
  SELECT @c = COUNT(*) FROM dbo.doc;
  PRINT 'Q18 ' + @label + ': count=' + CAST(@c AS varchar(11));
END
GO
GRANT EXECUTE ON dbo.q18_count TO probe_u;
GO
EXEC dbo.q18_count 'no policy, dbo';
EXECUTE AS USER = 'probe_u';
DECLARE @c int; SELECT @c = COUNT(*) FROM dbo.doc;
PRINT 'Q18 no policy, probe_u: count=' + CAST(@c AS varchar(11));
REVERT;
GO
CREATE SECURITY POLICY dbo.doc_policy ADD FILTER PREDICATE dbo.doc_pred(owner) ON dbo.doc WITH (STATE = ON);
GO
EXEC dbo.q18_count 'filter predicate, dbo';
EXECUTE AS USER = 'probe_u';
DECLARE @c int; SELECT @c = COUNT(*) FROM dbo.doc;
PRINT 'Q18 filter predicate, probe_u: count=' + CAST(@c AS varchar(11));
REVERT;
GO

-- Q19 names: an unqualified name that is a decimal (opaque) column of one
-- table and an int column of the other is ambiguous (209), as for two int
-- columns; qualified it reads the int column.
CREATE OR ALTER PROCEDURE dbo.q19_try @sql nvarchar(max), @label varchar(100) AS
BEGIN
  SET NOCOUNT ON;
  BEGIN TRY
    EXEC sp_executesql @sql;
    PRINT 'Q19 ' + @label + ': no error';
  END TRY
  BEGIN CATCH
    PRINT 'Q19 ' + @label + ': error ' + CAST(ERROR_NUMBER() AS varchar(11));
  END CATCH
END
GO
CREATE TABLE dbo.price_int (item_id int NOT NULL PRIMARY KEY, price int NULL);
INSERT INTO dbo.price_int (item_id, price) VALUES (1, 10), (2, 20);
GO
EXEC dbo.q19_try N'DECLARE @v int; SELECT @v = COUNT(*) FROM dbo.line l JOIN dbo.price_int p ON p.item_id = l.item_id WHERE price > 5;', 'decimal and int column, unqualified';
EXEC dbo.q19_try N'DECLARE @v int; SELECT @v = COUNT(*) FROM dbo.line l JOIN dbo.price_int p ON p.item_id = l.item_id WHERE p.price > 5;', 'decimal and int column, qualified';
EXEC dbo.q19_try N'DECLARE @v int; SELECT @v = COUNT(*) FROM dbo.line l JOIN dbo.item i ON i.id = l.item_id WHERE boom IS NULL;', 'raising computed column of one table named in WHERE';
GO
USE master;
GO
DROP DATABASE probe_c3d2;
GO
