-- Typed values beyond int and strings (phase C2c of docs/verified-conversion.md),
-- SQL Server 2022: integer widths, bit, date and datetime2.
-- Run: sqlcmd -I -i mssql.sql > mssql.out (a scratch server, never one holding data).
-- Each probe prints "Vn <name>: <observation>"; errors are caught and printed by
-- number, so the output names no server. Dates print in ISO 8601 (style 126).
SET NOCOUNT ON;
GO
DROP DATABASE IF EXISTS probe_c2c;
GO
CREATE DATABASE probe_c2c;
GO
USE probe_c2c;
GO

-- V1 integer widths: the result type of arithmetic on each width, and the
-- overflow errors. smallint + smallint is int (not smallint); int + bigint
-- is bigint; int and bigint overflow raise 8115.
CREATE OR ALTER PROCEDURE dbo.v1 AS
BEGIN
  SET NOCOUNT ON;
  DECLARE @s smallint = 32767, @i int = 2147483647, @b bigint = 9223372036854775807, @r bigint, @s1 smallint = 1, @rs smallint, @b1 bigint = 1, @i1 int = 1;
  PRINT 'V1 smallint + smallint: type=' + CAST(SQL_VARIANT_PROPERTY(@s1 + @s1, 'BaseType') AS varchar(20));
  PRINT 'V1 -smallint: type=' + CAST(SQL_VARIANT_PROPERTY(-@s1, 'BaseType') AS varchar(20));
  BEGIN TRY
    SET @r = @s + @s;
    PRINT 'V1 smallint 32767 + 32767: no error, value=' + CAST(@r AS varchar(20));
  END TRY
  BEGIN CATCH
    PRINT 'V1 smallint 32767 + 32767: error ' + CAST(ERROR_NUMBER() AS varchar(11));
  END CATCH
  BEGIN TRY
    SET @rs = -32768;
    SET @r = -@rs;
    PRINT 'V1 -(-32768 smallint): no error, value=' + CAST(@r AS varchar(20));
  END TRY
  BEGIN CATCH
    PRINT 'V1 -(-32768 smallint): error ' + CAST(ERROR_NUMBER() AS varchar(11));
  END CATCH
  BEGIN TRY
    SET @rs = -32768;
    SET @r = @rs / -1;
    PRINT 'V1 smallint min / -1: no error, value=' + CAST(@r AS varchar(20));
  END TRY
  BEGIN CATCH
    PRINT 'V1 smallint min / -1: error ' + CAST(ERROR_NUMBER() AS varchar(11));
  END CATCH
  PRINT 'V1 smallint / smallint: type=' + CAST(SQL_VARIANT_PROPERTY(@s1 / @s1, 'BaseType') AS varchar(20))
    + '; smallint % smallint: type=' + CAST(SQL_VARIANT_PROPERTY(@s1 % @s1, 'BaseType') AS varchar(20))
    + '; smallint * smallint: type=' + CAST(SQL_VARIANT_PROPERTY(@s1 * @s1, 'BaseType') AS varchar(20));
  PRINT 'V1 smallint + int: type=' + CAST(SQL_VARIANT_PROPERTY(@s1 + @i1, 'BaseType') AS varchar(20));
  PRINT 'V1 smallint + bigint: type=' + CAST(SQL_VARIANT_PROPERTY(@s1 + @b1, 'BaseType') AS varchar(20));
  PRINT 'V1 int + int: type=' + CAST(SQL_VARIANT_PROPERTY(@i + 0, 'BaseType') AS varchar(20));
  PRINT 'V1 int + bigint: type=' + CAST(SQL_VARIANT_PROPERTY(@i + @b1, 'BaseType') AS varchar(20));
  PRINT 'V1 bigint / int: type=' + CAST(SQL_VARIANT_PROPERTY(@b / @i, 'BaseType') AS varchar(20))
    + ' value=' + CAST(@b / @i AS varchar(20));
  PRINT 'V1 bigint % int: type=' + CAST(SQL_VARIANT_PROPERTY(@b % 7, 'BaseType') AS varchar(20))
    + ' value=' + CAST(@b % 7 AS varchar(20));
  PRINT 'V1 -bigint / -1: value=' + CAST((-@b) / -1 AS varchar(20));
  BEGIN TRY
    SET @r = @i + 1;
    PRINT 'V1 int + 1 overflow: no error, value=' + CAST(@r AS varchar(20));
  END TRY
  BEGIN CATCH
    PRINT 'V1 int + 1 overflow: error ' + CAST(ERROR_NUMBER() AS varchar(11));
  END CATCH
  BEGIN TRY
    SET @r = @b + 1;
    PRINT 'V1 bigint + 1 overflow: no error, value=' + CAST(@r AS varchar(20));
  END TRY
  BEGIN CATCH
    PRINT 'V1 bigint + 1 overflow: error ' + CAST(ERROR_NUMBER() AS varchar(11));
  END CATCH
  BEGIN TRY
    SET @r = @b * 2;
    PRINT 'V1 bigint * 2 overflow: no error, value=' + CAST(@r AS varchar(20));
  END TRY
  BEGIN CATCH
    PRINT 'V1 bigint * 2 overflow: error ' + CAST(ERROR_NUMBER() AS varchar(11));
  END CATCH
  BEGIN TRY
    SET @b = -9223372036854775807 - 1;
    SET @r = @b / -1;
    PRINT 'V1 bigint min / -1: no error, value=' + CAST(@r AS varchar(20));
  END TRY
  BEGIN CATCH
    PRINT 'V1 bigint min / -1: error ' + CAST(ERROR_NUMBER() AS varchar(11));
  END CATCH
  BEGIN TRY
    SET @r = @b % -1;
    PRINT 'V1 bigint min % -1: no error, value=' + CAST(@r AS varchar(20));
  END TRY
  BEGIN CATCH
    PRINT 'V1 bigint min % -1: error ' + CAST(ERROR_NUMBER() AS varchar(11));
  END CATCH
  BEGIN TRY
    SET @r = @b / 0;
    PRINT 'V1 bigint / 0: no error';
  END TRY
  BEGIN CATCH
    PRINT 'V1 bigint / 0: error ' + CAST(ERROR_NUMBER() AS varchar(11));
  END CATCH
  PRINT 'V1 bigint / 7 truncates: value=' + CAST(CAST(-100000000000 AS bigint) / 7 AS varchar(20));
END
GO
EXEC dbo.v1;
GO

-- V2 integer literals and stores between widths: a literal beyond the int
-- range is numeric, not bigint; a store that narrows raises when the value
-- does not fit (int into smallint: 220; bigint into int: 8115); a store that
-- widens keeps the value; COUNT(*) is int and stores into a bigint variable.
CREATE TABLE dbo.c (id int NOT NULL CONSTRAINT pk_c PRIMARY KEY);
GO
CREATE OR ALTER PROCEDURE dbo.v2 AS
BEGIN
  SET NOCOUNT ON;
  DECLARE @s smallint, @i int, @b bigint;
  PRINT 'V2 literal 2147483647: type=' + CAST(SQL_VARIANT_PROPERTY(2147483647, 'BaseType') AS varchar(20));
  PRINT 'V2 literal 2147483648: type=' + CAST(SQL_VARIANT_PROPERTY(2147483648, 'BaseType') AS varchar(20))
    + '(' + CAST(SQL_VARIANT_PROPERTY(2147483648, 'Precision') AS varchar(20)) + ','
    + CAST(SQL_VARIANT_PROPERTY(2147483648, 'Scale') AS varchar(20)) + ')';
  PRINT 'V2 literal 32767: type=' + CAST(SQL_VARIANT_PROPERTY(32767, 'BaseType') AS varchar(20));
  SET @s = 32767;
  SET @b = @s;
  SET @i = @s;
  PRINT 'V2 widening stores keep the value: int=' + CAST(@i AS varchar(20)) + ' bigint=' + CAST(@b AS varchar(20));
  BEGIN TRY
    SET @i = 40000;
    SET @s = @i;
    PRINT 'V2 int 40000 into smallint: no error, value=' + CAST(@s AS varchar(20));
  END TRY
  BEGIN CATCH
    PRINT 'V2 int 40000 into smallint: error ' + CAST(ERROR_NUMBER() AS varchar(11));
  END CATCH
  BEGIN TRY
    SET @b = 3000000000;
    SET @i = @b;
    PRINT 'V2 bigint 3000000000 into int: no error, value=' + CAST(@i AS varchar(20));
  END TRY
  BEGIN CATCH
    PRINT 'V2 bigint 3000000000 into int: error ' + CAST(ERROR_NUMBER() AS varchar(11));
  END CATCH
  BEGIN TRY
    SET @s = 40000;
    PRINT 'V2 literal 40000 into smallint: no error, value=' + CAST(@s AS varchar(20));
  END TRY
  BEGIN CATCH
    PRINT 'V2 literal 40000 into smallint: error ' + CAST(ERROR_NUMBER() AS varchar(11));
  END CATCH
  INSERT INTO dbo.c (id) VALUES (1), (2), (3);
  SELECT @b = COUNT(*) FROM dbo.c;
  DECLARE @ct sql_variant;
  SELECT @ct = SQL_VARIANT_PROPERTY(COUNT(*), 'BaseType') FROM dbo.c;
  PRINT 'V2 COUNT(*) into bigint: value=' + CAST(@b AS varchar(20)) + ' count type=' + CAST(@ct AS varchar(20));
END
GO
EXEC dbo.v2;
GO

-- V3 bit: what a store converts (any nonzero number is 1, the strings
-- 'TRUE'/'FALSE' convert), a comparison with an int literal promotes the bit
-- to int, and the three-valued outcome of @b = 1, @b = 0, @b <> 1 for a bit
-- that is 1, 0 and NULL.
CREATE OR ALTER PROCEDURE dbo.v3 AS
BEGIN
  SET NOCOUNT ON;
  DECLARE @b bit, @r varchar(100);
  SET @b = 2;
  SET @r = 'V3 bit stores: 2=' + CAST(@b AS varchar(1));
  SET @b = -5;
  SET @r = @r + ' -5=' + CAST(@b AS varchar(1));
  SET @b = 0;
  SET @r = @r + ' 0=' + CAST(@b AS varchar(1));
  SET @b = 'TRUE';
  SET @r = @r + ' TRUE=' + CAST(@b AS varchar(1));
  SET @b = 'FALSE';
  SET @r = @r + ' FALSE=' + CAST(@b AS varchar(1));
  SET @b = 1;
  SET @r = @r + ' 1=' + CAST(@b AS varchar(1));
  PRINT @r;
  PRINT 'V3 bit + 0: type=' + CAST(SQL_VARIANT_PROPERTY(@b + 0, 'BaseType') AS varchar(20));
  PRINT 'V3 bit = 2 with bit 1: ' + CASE WHEN @b = 2 THEN 'true' ELSE 'not true' END;
  DECLARE @v bit, @k int = 0, @out varchar(200) = 'V3 three-valued:';
  WHILE @k < 3
  BEGIN
    SET @v = CASE @k WHEN 0 THEN 1 WHEN 1 THEN 0 ELSE NULL END;
    SET @out = @out + ' [' + ISNULL(CAST(@v AS varchar(1)), 'NULL') + ':';
    IF @v = 1 SET @out = @out + ' =1' ELSE SET @out = @out + ' not=1';
    IF @v = 0 SET @out = @out + ' =0' ELSE SET @out = @out + ' not=0';
    IF @v <> 1 SET @out = @out + ' <>1' ELSE SET @out = @out + ' not<>1';
    IF @v <> 0 SET @out = @out + ' <>0' ELSE SET @out = @out + ' not<>0';
    IF @v IS NULL SET @out = @out + ' isnull' ELSE SET @out = @out + ' notnull';
    SET @out = @out + ']';
    SET @k = @k + 1;
  END
  PRINT @out;
  PRINT 'V3 NULL = 1 is: ' + CASE WHEN @v = 1 THEN 'true' WHEN NOT (@v = 1) THEN 'false' ELSE 'unknown' END;
END
GO
EXEC dbo.v3;
GO

-- V4 date and datetime2 literals: the ISO 8601 forms 'YYYY-MM-DD' and
-- 'YYYY-MM-DDThh:mm:ss[.fffffff]' read the same under every DATEFORMAT and
-- LANGUAGE for date and datetime2 (datetime, outside the spec, reads
-- 'YYYY-MM-DD' by DATEFORMAT); other forms are not probed as literals of the
-- spec. A datetime2(6) literal with seven fractional digits: rounded or
-- truncated?
CREATE OR ALTER PROCEDURE dbo.v4 AS
BEGIN
  SET NOCOUNT ON;
  DECLARE @d date, @t datetime2(6), @dt datetime, @t3 datetime2(3);
  SET DATEFORMAT ydm;
  SET @d = '2024-01-02';
  SET @t = '2024-01-02T03:04:05';
  SET @dt = '2024-01-02';
  PRINT 'V4 DATEFORMAT ydm: date=' + CONVERT(varchar(30), @d, 126) + ' datetime2=' + CONVERT(varchar(30), @t, 126)
    + ' (datetime=' + CONVERT(varchar(30), @dt, 126) + ')';
  SET DATEFORMAT dmy;
  SET @d = '2024-01-02';
  SET @t = '2024-01-02T03:04:05.5';
  PRINT 'V4 DATEFORMAT dmy: date=' + CONVERT(varchar(30), @d, 126) + ' datetime2=' + CONVERT(varchar(30), @t, 126);
  SET DATEFORMAT mdy;
  SET LANGUAGE British;
  SET @d = '2024-01-02';
  SET @t = '2024-01-02T03:04:05.123456';
  PRINT 'V4 LANGUAGE British: date=' + CONVERT(varchar(30), @d, 126) + ' datetime2=' + CONVERT(varchar(30), @t, 126);
  SET LANGUAGE us_english;
  SET @t = '2024-01-01T00:00:00.1234567';
  PRINT 'V4 seven digits into datetime2(6): ' + CONVERT(varchar(30), @t, 126);
  SET @t = '2024-01-01T00:00:00.1234565';
  PRINT 'V4 .1234565 into datetime2(6): ' + CONVERT(varchar(30), @t, 126);
  SET @t = '2024-01-01T00:00:00.9999995';
  PRINT 'V4 .9999995 into datetime2(6): ' + CONVERT(varchar(30), @t, 126);
  SET @t3 = '2024-01-01T00:00:00.1239';
  PRINT 'V4 four digits into datetime2(3): ' + CONVERT(varchar(30), @t3, 126);
  SET @t = '2024-01-01T00:00:00.123456';
  SET @t3 = @t;
  PRINT 'V4 datetime2(6) .123456 into datetime2(3): ' + CONVERT(varchar(30), @t3, 126);
  SET @t = '2024-01-01T00:00:00.1235';
  SET @t3 = @t;
  PRINT 'V4 datetime2(6) .1235 into datetime2(3): ' + CONVERT(varchar(30), @t3, 126);
  SET @t3 = '2024-01-01T00:00:00.123';
  SET @t = @t3;
  PRINT 'V4 datetime2(3) into datetime2(6): ' + CONVERT(varchar(30), @t, 126);
  BEGIN TRY
    SET @d = '2024-02-30';
    PRINT 'V4 2024-02-30: no error';
  END TRY
  BEGIN CATCH
    PRINT 'V4 2024-02-30: error ' + CAST(ERROR_NUMBER() AS varchar(11));
  END CATCH
  SET @d = '0001-01-01';
  SET @t = '9999-12-31T23:59:59.999999';
  PRINT 'V4 range ends: ' + CONVERT(varchar(30), @d, 126) + ' ' + CONVERT(varchar(30), @t, 126);
  PRINT 'V4 literal types: ' + CAST(SQL_VARIANT_PROPERTY(CAST('2024-01-02' AS date), 'BaseType') AS varchar(20))
    + ' ' + CAST(SQL_VARIANT_PROPERTY(CAST('2024-01-02T03:04:05' AS datetime2(6)), 'BaseType') AS varchar(20));
END
GO
EXEC dbo.v4;
GO

-- V5 date against datetime2: a date stored into datetime2 is midnight; a
-- datetime2 cast or stored into date keeps the date part; comparisons.
CREATE OR ALTER PROCEDURE dbo.v5 AS
BEGIN
  SET NOCOUNT ON;
  DECLARE @d date = '2024-03-15', @t datetime2(6), @e date;
  SET @t = @d;
  PRINT 'V5 date into datetime2(6): ' + CONVERT(varchar(30), @t, 126);
  SET @t = '2024-03-15T23:59:59.999999';
  SET @e = CAST(@t AS date);
  PRINT 'V5 CAST(datetime2 AS date): ' + CONVERT(varchar(30), @e, 126);
  SET @e = @t;
  PRINT 'V5 datetime2 stored into date: ' + CONVERT(varchar(30), @e, 126);
  PRINT 'V5 date = datetime2 (same day, 23:59): ' + CASE WHEN @d = @t THEN 'true' ELSE 'not true' END
    + '; date < datetime2: ' + CASE WHEN @d < @t THEN 'true' ELSE 'not true' END;
  PRINT 'V5 date = datetime2 midnight: ' + CASE WHEN @d = CAST('2024-03-15T00:00:00' AS datetime2(6)) THEN 'true' ELSE 'not true' END;
  PRINT 'V5 date comparisons: ' + CASE WHEN CAST('2024-03-14' AS date) < @d THEN '< true' ELSE '< false' END
    + ' ' + CASE WHEN @d <= CAST('2024-03-15' AS date) THEN '<= true' ELSE '<= false' END
    + ' ' + CASE WHEN @d <> CAST('2024-03-15' AS date) THEN '<> true' ELSE '<> false' END;
  SET @e = NULL;
  PRINT 'V5 NULL date comparison: ' + CASE WHEN @e = @d THEN 'true' WHEN NOT (@e = @d) THEN 'false' ELSE 'unknown' END;
END
GO
EXEC dbo.v5;
GO

-- V6 DATEADD(day|month|year): month-end clamping, Feb 29 plus a year,
-- the time part kept on datetime2, and the range error 517 past 9999-12-31
-- and before 0001-01-01.
CREATE OR ALTER PROCEDURE dbo.v6 AS
BEGIN
  SET NOCOUNT ON;
  DECLARE @d date = '2024-01-31', @t datetime2(6) = '2024-01-31T10:20:30.123456', @r date, @rt datetime2(6);
  PRINT 'V6 2024-01-31 + 1 month: ' + CONVERT(varchar(30), DATEADD(month, 1, @d), 126);
  PRINT 'V6 2024-01-31 + 13 months: ' + CONVERT(varchar(30), DATEADD(month, 13, @d), 126);
  PRINT 'V6 2024-03-31 - 1 month: ' + CONVERT(varchar(30), DATEADD(month, -1, CAST('2024-03-31' AS date)), 126);
  PRINT 'V6 2024-02-29 + 1 year: ' + CONVERT(varchar(30), DATEADD(year, 1, CAST('2024-02-29' AS date)), 126);
  PRINT 'V6 2024-02-29 - 1 year: ' + CONVERT(varchar(30), DATEADD(year, -1, CAST('2024-02-29' AS date)), 126);
  PRINT 'V6 2024-02-29 + 4 years: ' + CONVERT(varchar(30), DATEADD(yy, 4, CAST('2024-02-29' AS date)), 126);
  PRINT 'V6 2024-01-31 + 1 day: ' + CONVERT(varchar(30), DATEADD(dd, 1, @d), 126);
  PRINT 'V6 2024-03-01 - 1 day: ' + CONVERT(varchar(30), DATEADD(d, -1, CAST('2024-03-01' AS date)), 126);
  PRINT 'V6 2023-12-31 + 60 days: ' + CONVERT(varchar(30), DATEADD(day, 60, CAST('2023-12-31' AS date)), 126);
  PRINT 'V6 datetime2 + 1 month keeps the time: ' + CONVERT(varchar(30), DATEADD(mm, 1, @t), 126);
  PRINT 'V6 datetime2 + 2 days keeps the time: ' + CONVERT(varchar(30), DATEADD(day, 2, @t), 126);
  PRINT 'V6 datetime2 + 1 year: ' + CONVERT(varchar(30), DATEADD(year, 1, CAST('2024-02-29T01:02:03' AS datetime2(6))), 126);
  PRINT 'V6 DATEADD types: ' + CAST(SQL_VARIANT_PROPERTY(DATEADD(day, 1, @d), 'BaseType') AS varchar(20))
    + ' ' + CAST(SQL_VARIANT_PROPERTY(DATEADD(day, 1, @t), 'BaseType') AS varchar(20))
    + '(' + CAST(SQL_VARIANT_PROPERTY(DATEADD(day, 1, @t), 'Scale') AS varchar(20)) + ')';
  DECLARE @nn int = NULL;
  PRINT 'V6 DATEADD NULL number: ' + ISNULL(CONVERT(varchar(30), DATEADD(day, @nn, @d), 126), 'NULL')
    + '; NULL date: ' + ISNULL(CONVERT(varchar(30), DATEADD(day, 1, CAST(NULL AS date)), 126), 'NULL');
  BEGIN TRY
    SET @r = DATEADD(day, 1, CAST('9999-12-31' AS date));
    PRINT 'V6 9999-12-31 + 1 day: no error, ' + CONVERT(varchar(30), @r, 126);
  END TRY
  BEGIN CATCH
    PRINT 'V6 9999-12-31 + 1 day: error ' + CAST(ERROR_NUMBER() AS varchar(11));
  END CATCH
  BEGIN TRY
    SET @r = DATEADD(year, -1, CAST('0001-06-01' AS date));
    PRINT 'V6 0001-06-01 - 1 year: no error, ' + CONVERT(varchar(30), @r, 126);
  END TRY
  BEGIN CATCH
    PRINT 'V6 0001-06-01 - 1 year: error ' + CAST(ERROR_NUMBER() AS varchar(11));
  END CATCH
  BEGIN TRY
    SET @rt = DATEADD(month, 1, CAST('9999-12-31T00:00:00' AS datetime2(6)));
    PRINT 'V6 datetime2 9999-12-31 + 1 month: no error, ' + CONVERT(varchar(30), @rt, 126);
  END TRY
  BEGIN CATCH
    PRINT 'V6 datetime2 9999-12-31 + 1 month: error ' + CAST(ERROR_NUMBER() AS varchar(11));
  END CATCH
  BEGIN TRY
    SET @r = DATEADD(day, 2147483647, @d);
    PRINT 'V6 + 2147483647 days: no error, ' + CONVERT(varchar(30), @r, 126);
  END TRY
  BEGIN CATCH
    PRINT 'V6 + 2147483647 days: error ' + CAST(ERROR_NUMBER() AS varchar(11));
  END CATCH
END
GO
EXEC dbo.v6;
GO

-- V7 DATEDIFF(day): counts midnight boundaries crossed, not elapsed time;
-- its type is int; negative when the end is earlier.
CREATE OR ALTER PROCEDURE dbo.v7 AS
BEGIN
  SET NOCOUNT ON;
  DECLARE @a datetime2(6) = '2024-01-01T23:59:00', @b datetime2(6) = '2024-01-02T00:01:00';
  PRINT 'V7 23:59 to next 00:01: ' + CAST(DATEDIFF(day, @a, @b) AS varchar(11))
    + '; reversed: ' + CAST(DATEDIFF(day, @b, @a) AS varchar(11))
    + '; type=' + CAST(SQL_VARIANT_PROPERTY(DATEDIFF(day, @a, @b), 'BaseType') AS varchar(20));
  PRINT 'V7 00:00 to 23:59 same day: ' + CAST(DATEDIFF(dd, CAST('2024-01-01T00:00:00' AS datetime2(6)), CAST('2024-01-01T23:59:59.999999' AS datetime2(6))) AS varchar(11));
  PRINT 'V7 dates 2023-12-31 to 2024-03-01: ' + CAST(DATEDIFF(d, CAST('2023-12-31' AS date), CAST('2024-03-01' AS date)) AS varchar(11));
  PRINT 'V7 date to datetime2: ' + CAST(DATEDIFF(day, CAST('2024-01-01' AS date), @b) AS varchar(11));
  PRINT 'V7 0001-01-01 to 9999-12-31: ' + CAST(DATEDIFF(day, CAST('0001-01-01' AS date), CAST('9999-12-31' AS date)) AS varchar(11));
  PRINT 'V7 NULL: ' + ISNULL(CAST(DATEDIFF(day, CAST(NULL AS date), @b) AS varchar(11)), 'NULL');
END
GO
EXEC dbo.v7;
GO

-- V8 typed columns: a table with bigint, smallint, bit, date and datetime2(6)
-- columns stores and reads back the values as declared; a date key; bit
-- stored from 1/0; DATEADD on a column in an UPDATE.
CREATE TABLE dbo.ev (
  d date NOT NULL CONSTRAINT pk_ev PRIMARY KEY,
  n bigint NULL,
  s smallint NULL CONSTRAINT df_ev_s DEFAULT 0,
  b bit NOT NULL CONSTRAINT df_ev_b DEFAULT 0,
  t datetime2(6) NULL CONSTRAINT df_ev_t DEFAULT '2000-01-01T00:00:00'
);
GO
CREATE OR ALTER PROCEDURE dbo.v8 AS
BEGIN
  SET NOCOUNT ON;
  DECLARE @n bigint, @s smallint, @b bit, @t datetime2(6), @d date, @c int;
  INSERT INTO dbo.ev (d, n, s, b, t) VALUES ('2024-01-31', 3000000000, -7, 1, '2024-01-31T10:20:30.123456');
  INSERT INTO dbo.ev (d) VALUES ('2024-02-01');
  SELECT @n = n, @s = s, @b = b, @t = t FROM dbo.ev WHERE d = '2024-01-31';
  PRINT 'V8 read back: n=' + CAST(@n AS varchar(20)) + ' s=' + CAST(@s AS varchar(20)) + ' b=' + CAST(@b AS varchar(1))
    + ' t=' + CONVERT(varchar(30), @t, 126);
  SELECT @n = n, @s = s, @b = b, @t = t FROM dbo.ev WHERE d = CAST('2024-02-01' AS date);
  PRINT 'V8 defaults: n=' + ISNULL(CAST(@n AS varchar(20)), 'NULL') + ' s=' + CAST(@s AS varchar(20)) + ' b=' + CAST(@b AS varchar(1))
    + ' t=' + CONVERT(varchar(30), @t, 126);
  UPDATE dbo.ev SET d = DATEADD(month, 1, d), b = 0, t = DATEADD(day, 1, t) WHERE d = '2024-01-31';
  SELECT @b = b, @t = t FROM dbo.ev WHERE d = '2024-02-29';
  PRINT 'V8 after UPDATE: key 2024-02-29 b=' + CAST(@b AS varchar(1)) + ' t=' + CONVERT(varchar(30), @t, 126);
  BEGIN TRY
    INSERT INTO dbo.ev (d) VALUES ('2024-02-29');
    PRINT 'V8 duplicate date key: no error';
  END TRY
  BEGIN CATCH
    PRINT 'V8 duplicate date key: error ' + CAST(ERROR_NUMBER() AS varchar(11));
  END CATCH
  SELECT @c = COUNT(*) FROM dbo.ev WHERE b = 0;
  PRINT 'V8 rows with b = 0: ' + CAST(@c AS varchar(11));
  SELECT @c = COUNT(*) FROM dbo.ev WHERE d > '2024-02-01';
  PRINT 'V8 rows with d > 2024-02-01: ' + CAST(@c AS varchar(11));
END
GO
EXEC dbo.v8;
GO

USE master;
GO
DROP DATABASE probe_c2c;
GO
