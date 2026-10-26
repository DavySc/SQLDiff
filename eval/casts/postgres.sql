-- Cast and literal-typing probes, PostgreSQL 18. Run with psql -At (continues after errors).
\set ON_ERROR_STOP 0
\pset fieldsep '|'
-- 1. numeric / float to integer
SELECT 'dec2int 1.5', CAST(1.5 AS int);
SELECT 'dec2int -1.5', CAST(-1.5 AS int);
SELECT 'dec2int 2.5', CAST(2.5 AS int);
SELECT 'dec2int 1.99', CAST(1.99 AS int);
SELECT 'dec2bigint 1.5', CAST(1.5 AS bigint);
SELECT 'dec2smallint 1.5', CAST(1.5 AS smallint);
SELECT 'col dec2int 2.50', CAST(CAST(2.50 AS decimal(5,2)) AS int);
SELECT 'flt2int 1.5', CAST(CAST(1.5 AS float) AS int);
SELECT 'flt2int 2.5', CAST(CAST(2.5 AS float) AS int);
SELECT 'flt2int -2.5', CAST(CAST(-2.5 AS float) AS int);
SELECT 'real2int 2.5', CAST(CAST(2.5 AS real) AS int);
SELECT 'dec0 2int', CAST(CAST(7 AS decimal(5,0)) AS int);
SELECT 'trunc dec2int', CAST(trunc(1.5) AS int), CAST(trunc(-1.5) AS int), CAST(trunc(2.5::float8) AS int), CAST(trunc(-2.5::float8) AS int);
SELECT 'trunc bigint type', pg_typeof(trunc(9007199254740993::bigint)), trunc(9007199254740993::bigint);
SELECT 'dec2int overflow', CAST(2147483648.0 AS int);
SELECT 'int2smallint overflow', CAST(40000 AS smallint);
-- 2. numeric to numeric
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
SELECT 'dec2dec overflow', CAST(1234.5 AS decimal(4,1));
-- 3. literal typing
SELECT 'lit 1234567e0 type', pg_typeof(1234567e0);
SELECT 'lit 1.5 type', pg_typeof(1.5);
SELECT 'lit 2147483647 type', pg_typeof(2147483647);
SELECT 'lit 2147483648 type', pg_typeof(2147483648);
SELECT 'lit -2147483648 type', pg_typeof(-2147483648);
SELECT 'lit 9223372036854775808 type', pg_typeof(9223372036854775808);
SELECT 'lit N type', pg_typeof(N'ab');
SELECT 'lit N pad', N'ab ' = 'ab';
SELECT 'lit 0x1F', 0x1F;
SELECT 'lit 0x1F type', pg_typeof(0x1F);
SELECT 'lit 1e3 type', pg_typeof(1e3), 1e3;
SELECT 'lit 1.e3', 1.e3;
SELECT 'cast 1234567e0 varchar', CAST(1234567e0 AS varchar(30));
SELECT 'cast 1.5e0 varchar', CAST(1.5e0 AS varchar(30));
SELECT 'cast 0.1e0 varchar', CAST(0.1e0 AS varchar(30));
SELECT 'flt sum 0.1e0+0.2e0 = 0.3e0', CASE WHEN 0.1e0 + 0.2e0 = 0.3e0 THEN 1 ELSE 0 END;
SELECT 'dec sum 0.1+0.2 = 0.3', CASE WHEN 0.1 + 0.2 = 0.3 THEN 1 ELSE 0 END;
SELECT 'big lit div', 2147483648 / 7;
SELECT 'int lit div', 2147483647 / 7;
SELECT 'big lit 3e9 div 2', 3000000000 / 2;
-- 4. number to string
SELECT 'int2vc', CAST(123 AS varchar(30));
SELECT 'neg int2vc', CAST(-123 AS varchar(30));
SELECT 'dec2vc 1.50', CAST(1.50 AS varchar(30));
SELECT 'dec2vc 0.50', CAST(0.50 AS varchar(30));
SELECT 'dec2vc -0.50', CAST(-0.50 AS varchar(30));
SELECT 'col dec2vc', CAST(CAST(1.5 AS decimal(5,2)) AS varchar(30));
SELECT 'dec2vc short', CAST(123.45 AS varchar(3));
SELECT 'int2vc short', CAST(12345 AS varchar(3));
SELECT 'flt2vc 1234567', CAST(CAST(1234567 AS float) AS varchar(30));
SELECT 'flt2vc 0.1', CAST(CAST(0.1 AS float) AS varchar(30));
SELECT 'flt2vc 100000', CAST(CAST(100000 AS float) AS varchar(30));
SELECT 'real2vc', CAST(CAST(1.1 AS real) AS varchar(30));
SELECT 'char int', CAST(123 AS char(5)) || '|';
SELECT 'bigint2vc', CAST(CAST(-9223372036854775808 AS bigint) AS varchar(30));
-- 5. string to number
SELECT 'str2int '' 12 ''', CAST(' 12 ' AS int);
SELECT 'str2int ''+12''', CAST('+12' AS int);
SELECT 'str2int empty', CAST('' AS int);
SELECT 'str2dec empty', CAST('' AS decimal(5,2));
SELECT 'str2int 1.5', CAST('1.5' AS int);
SELECT 'str2int 1e3', CAST('1e3' AS int);
SELECT 'str2dec 1e3', CAST('1e3' AS decimal(10,2));
SELECT 'str2dec 1.555', CAST('1.555' AS decimal(5,2));
SELECT 'str2flt 1e3', CAST('1e3' AS float);
SELECT 'str2dec '' 1.5 ''', CAST(' 1.5 ' AS decimal(5,2));
SELECT 'var str2int', CAST(v AS int) FROM (VALUES ('7'), ('')) t(v);
SELECT 'str2int 1_000', CAST('1_000' AS int);
SELECT 'str2int 0x1F', CAST('0x1F' AS int);
-- 6. date / time
SELECT 'ts to date', CAST(CAST('2026-01-01 23:59:59.999999' AS timestamp) AS date);
SELECT 'ts to ts0', CAST(CAST('2026-01-01 23:59:59.6' AS timestamp) AS timestamp(0));
SELECT 'ts to time0', CAST(CAST('2026-01-01 10:00:00.6' AS timestamp) AS time(0));
SELECT 'date to ts', CAST(CAST('2026-01-01' AS date) AS timestamp);
SELECT 'ts to vc', CAST(CAST('2026-01-02 03:04:05.5' AS timestamp) AS varchar(40));
SELECT 'date to vc', CAST(CAST('2026-01-02' AS date) AS varchar(40));
SELECT 'time to vc', CAST(CAST('03:04:05' AS time) AS varchar(40));
SELECT 'tstz to ts', CAST(CAST('2026-01-02 03:04:05 +02:00' AS timestamptz) AS timestamp);
-- 7. integer arithmetic
SELECT 'int div', 7 / 2, -7 / 2, 7 % -2, -7 % 2;
SELECT 'smallint add type', pg_typeof(CAST(1 AS smallint) + CAST(1 AS smallint));
SELECT 'smallint add', CAST(30000 AS smallint) + CAST(30000 AS smallint);
SELECT 'int+bigint type', pg_typeof(1 + CAST(1 AS bigint));
SELECT 'dec div', 1 / 3.0, 1.0 / 3;
SELECT 'int*dec', 3 * 1.5;
SELECT 'dec add', CAST(1.25 AS decimal(5,2)) + CAST(1.125 AS decimal(6,3));
SELECT 'int/float', 1 / 3e0;
SELECT 'int add overflow', 2147483647 + 1;
SELECT 'intmin div -1', CAST(-2147483648 AS int) / -1;
SELECT 'div0', 1 / 0;
SELECT 'smallint mul overflow', CAST(300 AS smallint) * CAST(300 AS smallint);
-- 8. implicit conversion in comparisons
SELECT 'int = 1.5', CASE WHEN 1 = 1.0 THEN 1 ELSE 0 END, CASE WHEN 2 > 1.5 THEN 1 ELSE 0 END;
SELECT 'int col = 1.5', CASE WHEN CAST(1 AS int) = 1.5 THEN 1 ELSE 0 END;
SELECT 'flt = dec', CASE WHEN CAST(0.1 AS float) = 0.1 THEN 1 ELSE 0 END;
SELECT 'real = dec', CASE WHEN CAST(0.1 AS real) = 0.1 THEN 1 ELSE 0 END;
SELECT 'real = float', CASE WHEN CAST(0.1 AS real) = CAST(0.1 AS float) THEN 1 ELSE 0 END;
SELECT 'dec(5,2) = 1.005', CASE WHEN CAST(1.00 AS decimal(5,2)) = 1.005 THEN 1 ELSE 0 END;
SELECT 'big int = 1e0', CASE WHEN CAST(9007199254740993 AS bigint) = 9007199254740992e0 THEN 1 ELSE 0 END;
SELECT 'bigint = float', CASE WHEN CAST(9007199254740993 AS bigint) = 9007199254740992::float8 THEN 1 ELSE 0 END;
SELECT 'bigint = dec', CASE WHEN CAST(9007199254740993 AS bigint) = 9007199254740992.0 THEN 1 ELSE 0 END;
-- 9. string to string
SELECT 'vc trunc', CAST('abcdef' AS varchar(3));
SELECT 'char pad', CAST('ab' AS char(4)) || '|';
SELECT 'vc bytes', CAST('éééé' AS varchar(3));
-- 10. float / real
SELECT 'real 1.1 to float', CAST(CAST(1.1 AS real) AS float);
SELECT 'float(24) type', pg_typeof(CAST(1.1 AS float(24)));
SELECT 'float type', pg_typeof(CAST(1.1 AS float));
SELECT 'dec to float', CAST(0.1 AS float);
SELECT 'dec to real', CAST(0.1 AS real);
SELECT 'int to real', CAST(16777217 AS real);
SELECT 'bigint to float', CAST(CAST(9007199254740993 AS bigint) AS float);
SELECT 'float to bigint', CAST(CAST(9007199254740993 AS float) AS bigint);
SELECT 'money cast', CAST(1.23456 AS money), CAST(-1.23455 AS money), CAST(2.5 AS money);
