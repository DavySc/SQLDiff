-- Literal-conversion probes for equiv::literal, SQL Server 2022 (run 2026-10-26).
-- Results are summarised in crates/sqldiff-core/src/equiv/literal.rs.
SET NOCOUNT ON;
CREATE TABLE t (k int, d2 datetime2 NOT NULL, d0 datetime2(0), dt datetime, sdt smalldatetime, dd date, tm time, tm3 time(3), i int, dec2 decimal(10,2), b bit, g uniqueidentifier, dto datetimeoffset, v varchar(30));
INSERT t VALUES (1,'9999-12-31 23:59:59.9999999','2020-01-01 10:00:00','2020-01-01 10:00:00','2020-01-01 10:01','2020-01-01','10:00:00.1234567','10:00:00.123',5,1.01,1,'6F9619FF-8B86-D011-B42D-00C04FC964FF','2020-01-01 10:00:00 +02:00','abc');
SELECT 'A1', COUNT(*) FROM t WHERE d2 = '9999-12-31 23:59:59.9999999';
SELECT 'A2', COUNT(*) FROM t WHERE d2 = '9999-12-31T23:59:59.9999999';
GO
SELECT 'A3 8digits', COUNT(*) FROM t WHERE d2 = '9999-12-31 23:59:59.99999999';
GO
SELECT 'A4 d0 frac', COUNT(*) FROM t WHERE d0 = '2020-01-01 10:00:00.4';
SELECT 'A5 dt .001', COUNT(*) FROM t WHERE dt = '2020-01-01 10:00:00.001';
SELECT 'A6 dt .002', COUNT(*) FROM t WHERE dt = '2020-01-01 10:00:00.002';
SELECT 'A7 sdt :30', COUNT(*) FROM t WHERE sdt = '2020-01-01 10:00:30';
SELECT 'A8 sdt :29.999', COUNT(*) FROM t WHERE sdt = '2020-01-01 10:00:29.999';
SELECT 'A9 date w time', COUNT(*) FROM t WHERE dd = '2020-01-01 10:00';
GO
SELECT 'A10 date w time T', COUNT(*) FROM t WHERE dd = '2020-01-01T10:00:00';
GO
SELECT 'A11 dd < with time', COUNT(*) FROM t WHERE dd < '2020-01-01 10:00';
GO
SELECT 'A12 d2 date only', COUNT(*) FROM t WHERE d0 = '2020-01-01';
SELECT 'A13 d2 > date only', COUNT(*) FROM t WHERE d0 > '2020-01-01';
SELECT 'A14 tm', COUNT(*) FROM t WHERE tm = '10:00:00.1234567';
SELECT 'A15 tm3 more digits', COUNT(*) FROM t WHERE tm3 = '10:00:00.1234';
SELECT 'A16 i', COUNT(*) FROM t WHERE i = '5';
SELECT 'A17 i sp', COUNT(*) FROM t WHERE i = ' 5 ';
GO
SELECT 'A18 i 5.0', COUNT(*) FROM t WHERE i = '5.0';
GO
SELECT 'A19 dec 1.005', COUNT(*) FROM t WHERE dec2 = '1.005';
SELECT 'A20 dec 1.01', COUNT(*) FROM t WHERE dec2 = '1.01';
SELECT 'A20b dec 1.010', COUNT(*) FROM t WHERE dec2 = '1.010';
SELECT 'A21 dec 1.0100001', COUNT(*) FROM t WHERE dec2 = '1.0100001';
GO
SELECT 'A22 b true', COUNT(*) FROM t WHERE b = 'true';
SELECT 'A23 b 1', COUNT(*) FROM t WHERE b = '1';
GO
SELECT 'A24 b yes', COUNT(*) FROM t WHERE b = 'yes';
GO
SELECT 'A25 b TRUE', COUNT(*) FROM t WHERE b = 'TRUE';
GO
SELECT 'A26 g lower', COUNT(*) FROM t WHERE g = '6f9619ff-8b86-d011-b42d-00c04fc964ff';
SELECT 'A27 g braces', COUNT(*) FROM t WHERE g = '{6F9619FF-8B86-D011-B42D-00C04FC964FF}';
GO
SET DATEFORMAT dmy;
SELECT 'A28 dmy dt iso-dash', COUNT(*) FROM t WHERE dt = '2020-01-01 10:00:00';
SELECT 'A29 dmy d0 iso-dash', CAST(CAST('2020-02-01' AS datetime2) AS varchar(30)), CAST(CAST('2020-02-01' AS datetime) AS varchar(30)), CAST(CAST('2020-02-01' AS date) AS varchar(30)), CAST(CAST('2020-02-01T00:00:00' AS datetime) AS varchar(30)), CAST(CAST('20200201' AS datetime) AS varchar(30));
SET DATEFORMAT mdy;
GO
SELECT 'A30 IN', COUNT(*) FROM t WHERE d2 IN ('9999-12-31 23:59:59.9999999', '2020-01-01');
SELECT 'A31 BETWEEN', COUNT(*) FROM t WHERE d0 BETWEEN '2020-01-01' AND '2020-01-02';
SELECT 'A32 v vs date col', COUNT(*) FROM t WHERE v = d0;
GO
SELECT 'A33 dto', COUNT(*) FROM t WHERE dto = '2020-01-01 08:00:00 +00:00';
SELECT 'A34 dt rounding .0015', CONVERT(varchar(30), CAST('2020-01-01 10:00:00.0015' AS datetime), 121), CONVERT(varchar(30), CAST('2020-01-01 10:00:00.0049' AS datetime), 121), CONVERT(varchar(30), CAST('2020-01-01 10:00:00.005' AS datetime), 121), CONVERT(varchar(30), CAST('2020-01-01 10:00:00.9989' AS datetime), 121), CONVERT(varchar(30), CAST('2020-01-01 10:00:00.9999' AS datetime), 121);
SELECT 'A35 d2 round', CONVERT(varchar(30), CAST('2020-01-01 10:00:00.12345675' AS datetime2), 121);
GO
SELECT 'A36 d2(0) round', CONVERT(varchar(30), CAST('2020-01-01 10:00:00.5' AS datetime2(0)), 121), CONVERT(varchar(30), CAST('2020-01-01 10:00:00.49' AS datetime2(0)), 121);
SELECT 'A37 dec cmp precision', COUNT(*) FROM t WHERE dec2 = '1.01000000000000000000000000000000000001';
GO
SELECT 'A38 i = 5.5 literal str', COUNT(*) FROM t WHERE i < '5.5';
GO
SELECT 'A39 d2 24:00', COUNT(*) FROM t WHERE d0 = '2020-01-01 24:00:00';
GO
SELECT 'A40 d2 single digit', COUNT(*) FROM t WHERE d0 = '2020-1-1 10:00';
GO
SET NOCOUNT ON;
SELECT 'B1', CONVERT(varchar(30), CAST('2020-01-01 10:00:00.0015' AS datetime), 121);
GO
SELECT 'B2', CONVERT(varchar(30), CAST('2020-01-01 10:00:00.001' AS datetime), 121), CONVERT(varchar(30), CAST('2020-01-01 10:00:00.002' AS datetime), 121), CONVERT(varchar(30), CAST('2020-01-01 10:00:00.005' AS datetime), 121), CONVERT(varchar(30), CAST('2020-01-01 10:00:00.009' AS datetime), 121), CONVERT(varchar(30), CAST('2020-01-01 10:00:00.5' AS datetime), 121), CONVERT(varchar(30), CAST('2020-01-01 10:00:00.01' AS datetime), 121);
GO
SELECT 'B3', CONVERT(varchar(30), CAST('2020-01-01 10:00:00.12345675' AS datetime2), 121), CONVERT(varchar(30), CAST('2020-01-01 10:00:00.12345674' AS datetime2), 121), CONVERT(varchar(30), CAST('2020-01-01 10:00:00.12345665' AS datetime2), 121);
GO
SELECT 'B4', CONVERT(varchar(30), CAST('9999-12-31 23:59:59.99999999' AS datetime2), 121);
GO
SELECT 'B5', CONVERT(varchar(30), CAST('10:00:00.12345675' AS time), 121);
GO
CREATE TABLE #d (dec2 decimal(10,2), i int, b bit, s smalldatetime, dt datetime, d2 datetime2(3), tm time(0));
INSERT #d VALUES (1.01, 5, 1, '2020-01-02 00:00', '2020-01-02 00:00', '2020-01-01 10:00:00.123', '10:00:00');
SELECT 'B6 dec .015 round', COUNT(*) FROM #d WHERE dec2 = '1.005';
SELECT 'B7 dec 1.0149', COUNT(*) FROM #d WHERE dec2 = '1.0149';
SELECT 'B8 dec 1.015', COUNT(*) FROM #d WHERE dec2 = '1.015';
SELECT 'B8b dec < 1.014', COUNT(*) FROM #d WHERE dec2 < '1.014';
GO
SELECT 'B9 dec overflow', COUNT(*) FROM #d WHERE dec2 = '123456789012';
GO
SELECT 'B10 int overflow', COUNT(*) FROM #d WHERE i = '9999999999';
GO
SELECT 'B11 bit 2', COUNT(*) FROM #d WHERE b = '2';
GO
SELECT 'B12 bit spaces', COUNT(*) FROM #d WHERE b = ' true ';
GO
SELECT 'B12b int +5', COUNT(*) FROM #d WHERE i = '+5';
SELECT 'B12c int 05', COUNT(*) FROM #d WHERE i = '05';
GO
SET DATEFORMAT mdy; SELECT 'B13 mdy', COUNT(*) FROM #d WHERE dt = '2020-01-02', COUNT(*) FROM #d WHERE s = '2020-01-02';
GO
SET DATEFORMAT ymd; SELECT 'B13 ymd', (SELECT COUNT(*) FROM #d WHERE dt = '2020-01-02'), (SELECT COUNT(*) FROM #d WHERE s = '2020-01-02');
GO
SET DATEFORMAT dmy; SELECT 'B13 dmy', (SELECT COUNT(*) FROM #d WHERE dt = '2020-01-02'), (SELECT COUNT(*) FROM #d WHERE s = '2020-01-02'), (SELECT COUNT(*) FROM #d WHERE dt = '2020-01-02 00:00'), (SELECT COUNT(*) FROM #d WHERE dt = '2020-01-02T00:00:00'),(SELECT COUNT(*) FROM #d WHERE dt = '20200102');
GO
SET DATEFORMAT ydm; SELECT 'B13 ydm', (SELECT COUNT(*) FROM #d WHERE dt = '2020-01-02'), (SELECT COUNT(*) FROM #d WHERE s = '2020-01-02');
GO
SET DATEFORMAT myd; SELECT 'B13 myd', (SELECT COUNT(*) FROM #d WHERE dt = '2020-01-02'), (SELECT COUNT(*) FROM #d WHERE s = '2020-01-02');
GO
SET DATEFORMAT dym; SELECT 'B13 dym', (SELECT COUNT(*) FROM #d WHERE dt = '2020-01-02'), (SELECT COUNT(*) FROM #d WHERE s = '2020-01-02');
GO
SET DATEFORMAT dmy; SELECT 'B14 dmy date/d2/dto', CAST(CAST('2020-02-01' AS date) AS varchar(30)), CAST(CAST('2020-02-01 10:00' AS datetime2) AS varchar(30)), CAST(CAST('2020-02-01' AS datetimeoffset) AS varchar(40)), CAST(CAST('2020-02-01T10:00:00' AS datetime) AS varchar(30)), CAST(CAST('2020-02-01T10:00' AS datetime) AS varchar(30));
GO
SET DATEFORMAT mdy;
SET LANGUAGE british; SELECT 'B15 british', @@DATEFIRST, CAST(CAST('2020-02-01' AS datetime) AS varchar(30)), CAST(CAST('2020-02-01' AS datetime2) AS varchar(30));
SET LANGUAGE us_english;
GO
SELECT 'B16 d2(3) vs 7 digits', COUNT(*) FROM #d WHERE d2 = '2020-01-01 10:00:00.1230000';
SELECT 'B17 tm(0) vs .4', COUNT(*) FROM #d WHERE tm = '10:00:00.4';
SELECT 'B18 smalldt vs 23:59:30', CONVERT(varchar(30), CAST('2020-01-01 23:59:30' AS smalldatetime), 121);
SELECT 'B19 d2 = date-only T?', COUNT(*) FROM #d WHERE d2 = '2020-01-01T10:00:00.123';
GO
SELECT 'B20 dt year 1700', COUNT(*) FROM #d WHERE dt = '1700-01-01';
GO
SELECT 'B21 dt T no seconds', CONVERT(varchar(30), CAST('2020-01-01T10:00' AS datetime), 121);
GO
SELECT 'B22 date ymd no dash', CAST(CAST('20200201 10:00' AS date) AS varchar(30)), CAST(CAST('2020/02/01' AS date) AS varchar(30));
GO
SELECT 'B23 time with date', CAST(CAST('2020-01-01 10:00:00' AS time) AS varchar(30));
GO
SELECT 'B24 IN uses conversion', COUNT(*) FROM #d WHERE dec2 IN ('1.005', '7');
SELECT 'B25 BETWEEN conv', COUNT(*) FROM #d WHERE dec2 BETWEEN '1.005' AND '1.006';
GO
SELECT 'B26 literal on left', COUNT(*) FROM #d WHERE '1.005' = dec2;
GO
SELECT 'B27 nvarchar literal', COUNT(*) FROM #d WHERE dec2 = N'1.005';
GO
