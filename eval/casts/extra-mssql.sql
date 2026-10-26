-- Second round: precision caps, timestamp rounding, float text, misc. SQL Server 2022.
SET NOCOUNT ON;
GO
SELECT 'dec38 add', CAST(1.0000000001 AS decimal(38,10)) + CAST(0 AS decimal(38,10));
SELECT 'dec38 add type', SQL_VARIANT_PROPERTY(CAST(1.0000000001 AS decimal(38,10)) + CAST(0 AS decimal(38,10)), 'Scale');
SELECT 'dec add type', SQL_VARIANT_PROPERTY(CAST(1 AS decimal(5,2)) + CAST(1 AS decimal(6,3)), 'Precision'), SQL_VARIANT_PROPERTY(CAST(1 AS decimal(5,2)) + CAST(1 AS decimal(6,3)), 'Scale');
SELECT 'int + dec type', SQL_VARIANT_PROPERTY(1 + 1.5, 'Precision'), SQL_VARIANT_PROPERTY(1 + 1.5, 'Scale');
SELECT 'ts round pre2000', CAST(CAST('1999-12-31 23:59:59.5' AS datetime2(1)) AS datetime2(0));
SELECT 'ts round 1970', CAST(CAST('1970-01-01 00:00:00.5' AS datetime2(1)) AS datetime2(0));
SELECT 'ts round post2000', CAST(CAST('2026-01-01 00:00:00.5' AS datetime2(1)) AS datetime2(0));
SELECT 'ts round 0.25', CAST(CAST('1999-12-31 23:59:59.25' AS datetime2(2)) AS datetime2(1));
SELECT 'time round', CAST(CAST('10:00:00.5' AS time(1)) AS time(0));
SELECT 'time round 23:59:59.5', CAST(CAST('23:59:59.5' AS time(1)) AS time(0));
SELECT 'dt2 to time', CAST(CAST('2026-01-01 10:00:00.1234567' AS datetime2) AS time);
SELECT 'flt2vc 123456.7', CAST(CAST(123456.7 AS float) AS varchar(30));
SELECT 'flt2vc 1e-5', CAST(CAST(0.00001 AS float) AS varchar(30));
SELECT 'flt2vc 1234567 nvc', CAST(CAST(1234567 AS float) AS nvarchar(30));
SELECT 'flt2vc concat', CONCAT(CAST(1234567 AS float), '');
SELECT 'str2flt empty', CAST('' AS float);
SELECT 'str2flt space', CAST(' 1.5 ' AS float);
SELECT 'tinyint add', CAST(200 AS tinyint) + CAST(1 AS tinyint);
SELECT 'tinyint+int type', SQL_VARIANT_PROPERTY(CAST(200 AS tinyint) + 1, 'BaseType');
SELECT 'date to dt', CAST(CAST('2026-01-02' AS date) AS datetime);
SELECT 'dec2int neg trunc', CAST(-0.5 AS int);
SELECT 'round 3arg', ROUND(1.57, 0, 1), ROUND(-1.57, 0, 1);
SELECT 'round 3arg type', SQL_VARIANT_PROPERTY(ROUND(1.57, 0, 1), 'Scale');
SELECT 'float trunc to int via round', CAST(ROUND(CAST(2.7 AS float), 0, 1) AS int);
GO
SELECT 'tinyint overflow', CAST(200 AS tinyint) + CAST(100 AS tinyint);
GO
SELECT 'tinyint neg', CAST(-1 AS tinyint);
GO
