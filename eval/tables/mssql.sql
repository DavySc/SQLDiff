-- Wider plain tables (phase C3c of docs/verified-conversion.md), SQL Server 2022.
-- Run: sqlcmd -I -i mssql.sql > mssql.out (a scratch server, never one holding data).
-- Each probe prints "Tn <name>: <observation>"; errors are caught and printed by
-- number, so the output names no server. Constraints are named explicitly.
SET NOCOUNT ON;
GO
DROP DATABASE IF EXISTS probe_c3c;
GO
CREATE DATABASE probe_c3c;
GO
USE probe_c3c;
GO
CREATE TABLE dbo.s (
  id int NOT NULL CONSTRAINT pk_s PRIMARY KEY,
  v varchar(5) NULL,
  w nvarchar(5) NULL
);
GO

-- T1 a string that fits is stored as it is, trailing spaces included
-- (ANSI_PADDING ON, the default), and read back unchanged.
CREATE OR ALTER PROCEDURE dbo.t1 AS
BEGIN
  SET NOCOUNT ON;
  INSERT INTO dbo.s (id, v, w) VALUES (1, 'ab  ', N'ab  ');
  INSERT INTO dbo.s (id, v, w) VALUES (2, 'abcde', N'abcde');
  INSERT INTO dbo.s (id, v, w) VALUES (3, '', N'');
  DECLARE @v varchar(5), @w nvarchar(5), @lv int, @lw int;
  SELECT @v = v, @w = w FROM dbo.s WHERE id = 1;
  PRINT 'T1 fits, trailing spaces: v=[' + @v + '] bytes=' + CAST(DATALENGTH(@v) AS varchar(11))
    + ' w=[' + @w + '] bytes=' + CAST(DATALENGTH(@w) AS varchar(11));
  SELECT @lv = DATALENGTH(v), @lw = DATALENGTH(w) FROM dbo.s WHERE id = 1;
  PRINT 'T1 stored bytes: v=' + CAST(@lv AS varchar(11)) + ' w=' + CAST(@lw AS varchar(11));
  SELECT @v = v, @w = w FROM dbo.s WHERE id = 2;
  PRINT 'T1 fits exactly: v=[' + @v + '] w=[' + @w + ']';
  SELECT @v = v FROM dbo.s WHERE id = 3;
  PRINT 'T1 empty string: v=[' + @v + '] null=' + CASE WHEN @v IS NULL THEN 'yes' ELSE 'no' END;
  UPDATE dbo.s SET v = 'xy ' WHERE id = 3;
  SELECT @lv = DATALENGTH(v) FROM dbo.s WHERE id = 3;
  PRINT 'T1 update fits: bytes=' + CAST(@lv AS varchar(11));
END
GO
EXEC dbo.t1;
GO

-- T2 a string too long for the column: INSERT and UPDATE raise 2628 under
-- ANSI_WARNINGS ON (the default), nothing is written; trailing spaces too.
CREATE OR ALTER PROCEDURE dbo.t2 AS
BEGIN
  SET NOCOUNT ON;
  DECLARE @n int;
  BEGIN TRY
    INSERT INTO dbo.s (id, v) VALUES (10, 'abcdef');
    PRINT 'T2 insert too long: no error';
  END TRY
  BEGIN CATCH
    SELECT @n = COUNT(*) FROM dbo.s WHERE id = 10;
    PRINT 'T2 insert too long: error ' + CAST(ERROR_NUMBER() AS varchar(11)) + ', rows=' + CAST(@n AS varchar(11));
  END CATCH
  BEGIN TRY
    UPDATE dbo.s SET v = 'abcdef' WHERE id = 2;
    PRINT 'T2 update too long: no error';
  END TRY
  BEGIN CATCH
    PRINT 'T2 update too long: error ' + CAST(ERROR_NUMBER() AS varchar(11));
  END CATCH
  BEGIN TRY
    INSERT INTO dbo.s (id, v) VALUES (11, 'abcde   ');
    SELECT @n = DATALENGTH(v) FROM dbo.s WHERE id = 11;
    PRINT 'T2 insert too long by spaces: no error, bytes=' + CAST(@n AS varchar(11));
  END TRY
  BEGIN CATCH
    PRINT 'T2 insert too long by spaces: error ' + CAST(ERROR_NUMBER() AS varchar(11));
  END CATCH
  BEGIN TRY
    INSERT INTO dbo.s (id, w) VALUES (12, N'abcdef');
    PRINT 'T2 nvarchar too long: no error';
  END TRY
  BEGIN CATCH
    PRINT 'T2 nvarchar too long: error ' + CAST(ERROR_NUMBER() AS varchar(11));
  END CATCH
END
GO
EXEC dbo.t2;
GO
-- With ANSI_WARNINGS OFF the same INSERT truncates silently (why fit is required
-- rather than an error of equal class).
SET ANSI_WARNINGS OFF;
INSERT INTO dbo.s (id, v) VALUES (13, 'abcdef');
DECLARE @t varchar(10);
SELECT @t = v FROM dbo.s WHERE id = 13;
PRINT 'T2 ANSI_WARNINGS OFF: v=[' + @t + ']';
SET ANSI_WARNINGS ON;
GO

-- T3 ANSI_PADDING OFF when a varchar column is created trims trailing spaces
-- on write; an nvarchar column keeps them (ASM-TSQL-ANSI-PADDING).
SET ANSI_PADDING OFF;
GO
CREATE TABLE dbo.p (id int NOT NULL CONSTRAINT pk_p PRIMARY KEY, v varchar(5) NULL, w nvarchar(5) NULL);
GO
SET ANSI_PADDING ON;
GO
INSERT INTO dbo.p (id, v, w) VALUES (1, 'ab  ', N'ab  ');
DECLARE @lv int, @lw int, @pv bit, @pw bit;
SELECT @lv = DATALENGTH(v), @lw = DATALENGTH(w) FROM dbo.p WHERE id = 1;
SELECT @pv = is_ansi_padded FROM sys.columns WHERE object_id = OBJECT_ID('dbo.p') AND name = 'v';
SELECT @pw = is_ansi_padded FROM sys.columns WHERE object_id = OBJECT_ID('dbo.p') AND name = 'w';
PRINT 'T3 ANSI_PADDING OFF column: v bytes=' + CAST(@lv AS varchar(11)) + ' (is_ansi_padded '
  + CAST(@pv AS varchar(1)) + '), w bytes=' + CAST(@lw AS varchar(11)) + ' (is_ansi_padded ' + CAST(@pw AS varchar(1)) + ')';
GO

-- T4 constant defaults: a column left out of INSERT gets its default; a NULL
-- default and no default give NULL; NOT NULL with a default needs no value.
CREATE TABLE dbo.d (
  id int NOT NULL CONSTRAINT pk_d PRIMARY KEY,
  q int NOT NULL CONSTRAINT df_d_q DEFAULT 0,
  r int NULL CONSTRAINT df_d_r DEFAULT (-1),
  c varchar(10) NULL CONSTRAINT df_d_c DEFAULT 'Paris',
  n nvarchar(10) NOT NULL CONSTRAINT df_d_n DEFAULT (N'x'),
  z int NULL CONSTRAINT df_d_z DEFAULT NULL,
  u int NULL
);
GO
CREATE OR ALTER PROCEDURE dbo.t4 AS
BEGIN
  SET NOCOUNT ON;
  INSERT INTO dbo.d (id) VALUES (1);
  INSERT INTO dbo.d (id, q, c) VALUES (2, 5, NULL);
  DECLARE @q int, @r int, @c varchar(10), @n nvarchar(10), @z int, @u int;
  SELECT @q = q, @r = r, @c = c, @n = n, @z = z, @u = u FROM dbo.d WHERE id = 1;
  PRINT 'T4 defaults: q=' + CAST(@q AS varchar(11)) + ' r=' + CAST(@r AS varchar(11)) + ' c=' + @c
    + ' n=' + @n + ' z=' + ISNULL(CAST(@z AS varchar(11)), 'NULL') + ' u=' + ISNULL(CAST(@u AS varchar(11)), 'NULL');
  SELECT @q = q, @c = c FROM dbo.d WHERE id = 2;
  PRINT 'T4 listed columns win: q=' + CAST(@q AS varchar(11)) + ' c=' + ISNULL(@c, 'NULL');
  UPDATE dbo.d SET u = 7 WHERE id = 2;
  SELECT @c = c FROM dbo.d WHERE id = 2;
  PRINT 'T4 update leaves defaults alone: c=' + ISNULL(@c, 'NULL');
END
GO
EXEC dbo.t4;
GO
-- A default too long for its column: CREATE TABLE accepts it, the INSERT that
-- uses it raises 2628 (why the slice asks a default to fit).
CREATE TABLE dbo.dl (id int NOT NULL CONSTRAINT pk_dl PRIMARY KEY, c varchar(3) NULL CONSTRAINT df_dl_c DEFAULT 'abcdef');
GO
BEGIN TRY
  INSERT INTO dbo.dl (id) VALUES (1);
  PRINT 'T4 default too long: no error';
END TRY
BEGIN CATCH
  PRINT 'T4 default too long: error ' + CAST(ERROR_NUMBER() AS varchar(11));
END CATCH
GO

-- T5 comparisons on string columns: T-SQL pads trailing spaces whatever the
-- collation, and the default collation ignores case; a column without COLLATE
-- takes the database's collation.
INSERT INTO dbo.s (id, v) VALUES (20, 'A'), (21, 'a ');
DECLARE @n int;
SELECT @n = COUNT(*) FROM dbo.s WHERE v = 'a';
PRINT 'T5 server-default collation: rows with v = ''a'': ' + CAST(@n AS varchar(11));
GO
DROP DATABASE IF EXISTS probe_c3c_bin;
GO
CREATE DATABASE probe_c3c_bin COLLATE Latin1_General_100_BIN2;
GO
USE probe_c3c_bin;
GO
CREATE TABLE dbo.s (id int NOT NULL CONSTRAINT pk_s PRIMARY KEY, v varchar(5) NULL);
INSERT INTO dbo.s (id, v) VALUES (20, 'A'), (21, 'a '), (22, 'a');
GO
DECLARE @n int, @coll sysname;
SELECT @n = COUNT(*) FROM dbo.s WHERE v = 'a';
SELECT @coll = collation_name FROM sys.columns WHERE object_id = OBJECT_ID('dbo.s') AND name = 'v';
PRINT 'T5 BIN2 database: column collation ' + @coll + ', rows with v = ''a'': ' + CAST(@n AS varchar(11));
SELECT @n = COUNT(*) FROM dbo.s WHERE RTRIM(v) = RTRIM('a');
PRINT 'T5 BIN2 database: rows with rtrim(v) = rtrim(''a''): ' + CAST(@n AS varchar(11));
GO
-- T6 a string key: 'a' and 'a ' are one key value even under BIN2 (why string
-- keys stay outside: PostgreSQL keeps them apart).
CREATE TABLE dbo.k (s varchar(5) NOT NULL CONSTRAINT pk_k PRIMARY KEY);
GO
BEGIN TRY
  INSERT INTO dbo.k (s) VALUES ('a');
  INSERT INTO dbo.k (s) VALUES ('a ');
  PRINT 'T6 string key a, a-space: no error';
END TRY
BEGIN CATCH
  PRINT 'T6 string key a, a-space: error ' + CAST(ERROR_NUMBER() AS varchar(11));
END CATCH
GO
USE probe_c3c;
GO

-- T7 a non-unique index changes no result: duplicates are accepted, reads
-- and writes give what they give without it.
CREATE TABLE dbo.i (id int NOT NULL CONSTRAINT pk_i PRIMARY KEY, v varchar(10) NULL, n int NULL);
GO
CREATE INDEX ix_i_v ON dbo.i (v);
CREATE NONCLUSTERED INDEX ix_i_n ON dbo.i (n) INCLUDE (v);
GO
CREATE OR ALTER PROCEDURE dbo.t7 AS
BEGIN
  SET NOCOUNT ON;
  INSERT INTO dbo.i (id, v, n) VALUES (1, 'x', 1), (2, 'x', 1), (3, NULL, NULL), (4, NULL, NULL);
  UPDATE dbo.i SET v = 'y', n = 2 WHERE id = 1;
  DELETE FROM dbo.i WHERE id = 4;
  DECLARE @c int, @v varchar(10);
  SELECT @c = COUNT(*) FROM dbo.i;
  SELECT @v = v FROM dbo.i WHERE id = 1;
  PRINT 'T7 non-unique index: rows=' + CAST(@c AS varchar(11)) + ' v1=' + @v;
END
GO
EXEC dbo.t7;
GO
-- T8 an index whose key may exceed 1700 bytes (nonclustered) is created with a
-- warning, and a row whose key is longer fails: an index on wide string
-- columns can change a write (why the slice bounds index keys).
CREATE TABLE dbo.w (id int NOT NULL CONSTRAINT pk_w PRIMARY KEY, v nvarchar(1000) NULL);
GO
CREATE INDEX ix_w_v ON dbo.w (v);
GO
BEGIN TRY
  INSERT INTO dbo.w (id, v) VALUES (1, REPLICATE(N'a', 850));
  PRINT 'T8 index key 1700 bytes: no error';
END TRY
BEGIN CATCH
  PRINT 'T8 index key 1700 bytes: error ' + CAST(ERROR_NUMBER() AS varchar(11));
END CATCH
BEGIN TRY
  INSERT INTO dbo.w (id, v) VALUES (2, REPLICATE(N'a', 851));
  PRINT 'T8 index key 1702 bytes: no error';
END TRY
BEGIN CATCH
  PRINT 'T8 index key 1702 bytes: error ' + CAST(ERROR_NUMBER() AS varchar(11));
END CATCH
GO
CREATE TABLE dbo.wc (id int NOT NULL CONSTRAINT pk_wc PRIMARY KEY NONCLUSTERED, v varchar(1000) NULL);
GO
CREATE CLUSTERED INDEX ix_wc_v ON dbo.wc (v);
GO
BEGIN TRY
  INSERT INTO dbo.wc (id, v) VALUES (1, REPLICATE('a', 900));
  PRINT 'T8 clustered key 900 bytes: no error';
END TRY
BEGIN CATCH
  PRINT 'T8 clustered key 900 bytes: error ' + CAST(ERROR_NUMBER() AS varchar(11));
END CATCH
BEGIN TRY
  INSERT INTO dbo.wc (id, v) VALUES (2, REPLICATE('a', 901));
  PRINT 'T8 clustered key 901 bytes: no error';
END TRY
BEGIN CATCH
  PRINT 'T8 clustered key 901 bytes: error ' + CAST(ERROR_NUMBER() AS varchar(11));
END CATCH
GO

-- T9 SELECT @x = col into a variable shorter than the column truncates (why
-- reads are stored only where they fit).
DECLARE @x varchar(2);
SELECT @x = v FROM dbo.s WHERE id = 2;
PRINT 'T9 read into a shorter variable: x=[' + @x + ']';
GO
USE master;
GO
DROP DATABASE probe_c3c;
DROP DATABASE probe_c3c_bin;
GO
