SELECT DATEADD(minute, 2.7, CAST('2020-01-01 00:00:00' AS datetime2(7))) a, DATEADD(minute, -2.7, CAST('2020-01-01 00:00:00' AS datetime2(7))) b,
 DATEADD(minute, CAST(2.7e0 AS float), CAST('2020-01-01 00:00:00' AS datetime2(7))) c, DATEADD(minute, -2.7e0, CAST('2020-01-01 00:00:00' AS datetime2(7))) d,
 DATEADD(minute, CAST(2.7 AS int), CAST('2020-01-01 00:00:00' AS datetime2(7))) e, DATEADD(day, 2.9, CAST('2020-01-01' AS date)) f, DATEADD(second, 2.5, CAST('2020-01-01' AS datetime)) g, DATEADD(second, -0.9, CAST('2020-01-01 00:01:00' AS datetime2)) h;
SELECT DATEADD(minute, CAST(2147483647.5 AS float), CAST('2020-01-01' AS datetime2));
