-- Cast and literal-typing probes, SQL Server 2022. Run with sqlcmd -I -W -h -1 -s"|".
-- Each line prints label|value; a failing line prints the error instead.
SET NOCOUNT ON;
GO
-- 1. decimal / float to integer
SELECT 'dec2int 1.5', CAST(1.5 AS int);
SELECT 'dec2int -1.5', CAST(-1.5 AS int);
SELECT 'dec2int 2.5', CAST(2.5 AS int);
SELECT 'dec2int 1.99', CAST(1.99 AS int);
SELECT 'dec2bigint 1.5', CAST(1.5 AS bigint);
SELECT 'dec2smallint 1.5', CAST(1.5 AS smallint);
SELECT 'dec2tinyint 1.5', CAST(1.5 AS tinyint);
SELECT 'col dec2int 2.50', CAST(CAST(2.50 AS decimal(5,2)) AS int);
SELECT 'flt2int 1.5', CAST(CAST(1.5 AS float) AS int);
SELECT 'flt2int 2.5', CAST(CAST(2.5 AS float) AS int);
SELECT 'flt2int -2.5', CAST(CAST(-2.5 AS float) AS int);
SELECT 'real2int 2.5', CAST(CAST(2.5 AS real) AS int);
SELECT 'dec0 2int', CAST(CAST(7 AS decimal(5,0)) AS int);
SELECT 'convert dec2int', CONVERT(int, 1.5);
GO
SELECT 'dec2int overflow', CAST(2147483648.0 AS int);
GO
SELECT 'int2smallint overflow', CAST(40000 AS smallint);
GO
-- 2. decimal to decimal (narrower scale)
SELECT 'dec2dec 1.555', CAST(1.555 AS decimal(5,2));
SELECT 'dec2dec -1.555', CAST(-1.555 AS decimal(5,2));
SELECT 'dec2dec 1.545', CAST(1.545 AS decimal(5,2));
SELECT 'dec2dec 2.5 s0', CAST(2.5 AS decimal(5,0));
SELECT 'dec2dec -2.5 s0', CAST(-2.5 AS decimal(5,0));
SELECT 'flt2dec 2.675', CAST(CAST(2.675 AS float) AS decimal(5,2));
SELECT 'flt2dec 1.5 s0', CAST(CAST(2.5 AS float) AS decimal(5,0));
SELECT 'flt2dec 0.1', CAST(CAST(0.1 AS float) AS decimal(30,25));
SELECT 'dec2numeric default', CAST(1.5 AS numeric);
SELECT 'dec2decimal default', CAST(123.456 AS decimal);
GO
SELECT 'dec2dec overflow', CAST(1234.5 AS decimal(4,1));
GO
-- 3. literal typing
SELECT 'lit 1234567e0 type', SQL_VARIANT_PROPERTY(1234567e0, 'BaseType');
SELECT 'lit 1.5 type', SQL_VARIANT_PROPERTY(1.5, 'BaseType'), SQL_VARIANT_PROPERTY(1.5, 'Precision'), SQL_VARIANT_PROPERTY(1.5, 'Scale');
SELECT 'lit 2147483647 type', SQL_VARIANT_PROPERTY(2147483647, 'BaseType');
SELECT 'lit 2147483648 type', SQL_VARIANT_PROPERTY(2147483648, 'BaseType'), SQL_VARIANT_PROPERTY(2147483648, 'Precision');
SELECT 'lit -2147483648 type', SQL_VARIANT_PROPERTY(-2147483648, 'BaseType');
SELECT 'lit 9223372036854775808 type', SQL_VARIANT_PROPERTY(9223372036854775808, 'BaseType');
SELECT 'lit $12.34 type', SQL_VARIANT_PROPERTY($12.34, 'BaseType');
SELECT 'lit 0x1F type', SQL_VARIANT_PROPERTY(0x1F, 'BaseType');
SELECT 'lit N type', SQL_VARIANT_PROPERTY(N'ab', 'BaseType');
SELECT 'lit .5 type', SQL_VARIANT_PROPERTY(.5, 'BaseType');
SELECT 'lit 1e3 type', SQL_VARIANT_PROPERTY(1e3, 'BaseType');
SELECT 'lit 1.e3', 1.e3;
SELECT 'cast 1234567e0 varchar', CAST(1234567e0 AS varchar(30));
SELECT 'cast 1.5e0 varchar', CAST(1.5e0 AS varchar(30));
SELECT 'cast 0.1e0 varchar', CAST(0.1e0 AS varchar(30));
SELECT 'flt sum 0.1e0+0.2e0 = 0.3e0', CASE WHEN 0.1e0 + 0.2e0 = 0.3e0 THEN 1 ELSE 0 END;
SELECT 'dec sum 0.1+0.2 = 0.3', CASE WHEN 0.1 + 0.2 = 0.3 THEN 1 ELSE 0 END;
SELECT 'big lit div', 2147483648 / 7;
SELECT 'int lit div', 2147483647 / 7;
SELECT 'big lit 3e9 div 2', 3000000000 / 2;
SELECT 'hex plus', 0x1F + 0;
GO
-- 4. number to string
SELECT 'int2vc', CAST(123 AS varchar(30));
SELECT 'neg int2vc', CAST(-123 AS varchar(30));
SELECT 'dec2vc 1.50', CAST(1.50 AS varchar(30));
SELECT 'dec2vc 0.50', CAST(0.50 AS varchar(30));
SELECT 'dec2vc -0.50', CAST(-0.50 AS varchar(30));
SELECT 'col dec2vc', CAST(CAST(1.5 AS decimal(5,2)) AS varchar(30));
SELECT 'dec2vc short', CAST(123.45 AS varchar(3));
GO
SELECT 'int2vc short', CAST(12345 AS varchar(3));
GO
SELECT 'flt2vc 1234567', CAST(CAST(1234567 AS float) AS varchar(30));
SELECT 'flt2vc 0.1', CAST(CAST(0.1 AS float) AS varchar(30));
SELECT 'flt2vc 100000', CAST(CAST(100000 AS float) AS varchar(30));
SELECT 'real2vc', CAST(CAST(1.1 AS real) AS varchar(30));
SELECT 'money2vc', CAST(CAST(1234.5 AS money) AS varchar(30));
SELECT 'nvc int', CAST(123 AS nvarchar(30));
SELECT 'char int', CAST(123 AS char(5)) + '|';
SELECT 'vc default len', LEN(CAST(REPLICATE('x', 40) AS varchar));
SELECT 'bigint2vc', CAST(CAST(-9223372036854775808 AS bigint) AS varchar(30));
GO
-- 5. string to number
SELECT 'str2int '' 12 ''', CAST(' 12 ' AS int);
SELECT 'str2int ''+12''', CAST('+12' AS int);
SELECT 'str2int empty', CAST('' AS int);
SELECT 'str2dec empty', CAST('' AS decimal(5,2));
GO
SELECT 'str2int 1.5', CAST('1.5' AS int);
GO
SELECT 'str2int 1e3', CAST('1e3' AS int);
GO
SELECT 'str2dec 1e3', CAST('1e3' AS decimal(10,2));
GO
SELECT 'str2dec 1.555', CAST('1.555' AS decimal(5,2));
SELECT 'str2flt 1e3', CAST('1e3' AS float);
SELECT 'str2dec '' 1.5 ''', CAST(' 1.5 ' AS decimal(5,2));
GO
SELECT 'var str2int', CAST(v AS int) FROM (VALUES ('7'), ('')) t(v);
GO
-- 6. date / time
SELECT 'dt2 to date', CAST(CAST('2026-01-01 23:59:59.9999999' AS datetime2) AS date);
SELECT 'dt2 to datetime', CAST(CAST('2026-01-01 23:59:59.9989999' AS datetime2) AS datetime);
SELECT 'dt2 to datetime2', CAST(CAST('2026-01-01 23:59:59.9999999' AS datetime2) AS datetime2(0));
SELECT 'dt2(7) to time(0)', CAST(CAST('2026-01-01 10:00:00.6' AS datetime2) AS time(0));
SELECT 'date to dt2', CAST(CAST('2026-01-01' AS date) AS datetime2);
SELECT 'dt2 to vc', CAST(CAST('2026-01-02 03:04:05.5' AS datetime2) AS varchar(40));
SELECT 'dt to vc', CAST(CAST('2026-01-02 03:04:05.5' AS datetime) AS varchar(40));
SELECT 'date to vc', CAST(CAST('2026-01-02' AS date) AS varchar(40));
SELECT 'time to vc', CAST(CAST('03:04:05' AS time) AS varchar(40));
SELECT 'dto to dt2', CAST(CAST('2026-01-02 03:04:05 +02:00' AS datetimeoffset) AS datetime2);
SELECT 'dto to vc', CAST(CAST('2026-01-02 03:04:05 +02:00' AS datetimeoffset) AS varchar(40));
SELECT 'dt2 cast smalldt', CAST(CAST('2026-01-02 03:04:31' AS datetime2) AS smalldatetime);
SELECT 'dt2 to datetime2 default', CAST(CAST('2026-01-01 23:59:59.12345678' AS datetime2(7)) AS datetime2);
GO
-- 7. integer arithmetic, division, overflow
SELECT 'int div', 7 / 2, -7 / 2, 7 % -2, -7 % 2;
SELECT 'smallint add type', SQL_VARIANT_PROPERTY(CAST(1 AS smallint) + CAST(1 AS smallint), 'BaseType');
SELECT 'tinyint add type', SQL_VARIANT_PROPERTY(CAST(1 AS tinyint) + CAST(1 AS tinyint), 'BaseType');
SELECT 'smallint add', CAST(30000 AS smallint) + CAST(30000 AS smallint);
SELECT 'int+bigint type', SQL_VARIANT_PROPERTY(1 + CAST(1 AS bigint), 'BaseType');
SELECT 'dec div', 1 / 3.0, 1.0 / 3;
SELECT 'dec div type', SQL_VARIANT_PROPERTY(1 / 3.0, 'Precision'), SQL_VARIANT_PROPERTY(1 / 3.0, 'Scale');
SELECT 'int*dec', 3 * 1.5;
SELECT 'dec add', CAST(1.25 AS decimal(5,2)) + CAST(1.125 AS decimal(6,3));
SELECT 'int/float', 1 / 3e0;
GO
SELECT 'int add overflow', 2147483647 + 1;
GO
SELECT 'intmin div -1', CAST(-2147483648 AS int) / -1;
GO
SELECT 'div0', 1 / 0;
GO
SELECT 'smallint mul overflow', CAST(300 AS smallint) * CAST(300 AS smallint);
GO
-- 8. implicit conversion in comparisons
SELECT 'int = 1.5', CASE WHEN 1 = 1.0 THEN 1 ELSE 0 END, CASE WHEN 2 > 1.5 THEN 1 ELSE 0 END;
SELECT 'int col = 1.5', CASE WHEN CAST(1 AS int) = 1.5 THEN 1 ELSE 0 END;
SELECT 'flt = dec', CASE WHEN CAST(0.1 AS float) = 0.1 THEN 1 ELSE 0 END;
SELECT 'real = dec', CASE WHEN CAST(0.1 AS real) = 0.1 THEN 1 ELSE 0 END;
SELECT 'real = float', CASE WHEN CAST(0.1 AS real) = CAST(0.1 AS float) THEN 1 ELSE 0 END;
SELECT 'dec(5,2) = 1.005', CASE WHEN CAST(1.00 AS decimal(5,2)) = 1.005 THEN 1 ELSE 0 END;
SELECT 'big int = 1e0', CASE WHEN CAST(9007199254740993 AS bigint) = 9007199254740992e0 THEN 1 ELSE 0 END;
SELECT 'bigint = dec', CASE WHEN CAST(9007199254740993 AS bigint) = 9007199254740992.0 THEN 1 ELSE 0 END;
GO
-- 9. string to string
SELECT 'vc trunc', CAST('abcdef' AS varchar(3));
SELECT 'char pad', CAST('ab' AS char(4)) + '|';
SELECT 'nvc trunc', CAST(N'abcdef' AS nvarchar(3));
SELECT 'vc bytes', CAST(N'éééé' AS varchar(3));
SELECT 'vc default 30', LEN(CAST(REPLICATE('x', 40) AS varchar));
GO
-- 10. float / real
SELECT 'real 1.1 to float', CAST(CAST(1.1 AS real) AS float);
SELECT 'float(24) type', SQL_VARIANT_PROPERTY(CAST(1.1 AS float(24)), 'BaseType');
SELECT 'dec to float', CAST(0.1 AS float);
SELECT 'dec to real', CAST(0.1 AS real);
SELECT 'int to real', CAST(16777217 AS real);
SELECT 'bigint to float', CAST(CAST(9007199254740993 AS bigint) AS float);
SELECT 'float to bigint', CAST(CAST(9007199254740993 AS float) AS bigint);
GO
SELECT 'money cast', CAST(1.23456 AS money), CAST(-1.23455 AS money), CAST(2.5 AS money);
SELECT 'money to int', CAST(CAST(2.5 AS money) AS int), CAST(CAST(1.5 AS money) AS int), CAST(CAST(-2.5 AS money) AS int);
GO
