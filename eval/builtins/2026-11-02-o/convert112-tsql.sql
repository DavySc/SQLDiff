-- CONVERT(type, 'YYYYMMDD', 112) against CAST('YYYYMMDD' AS type), 2026-11-02, SQL Server 2022 CU27
SET NOCOUNT ON;
SELECT CONVERT(datetime,'20030701',112) a, CAST('20030701' AS datetime) b, CONVERT(datetime,'99991231',112) c, CAST('99991231' AS datetime) d;
SELECT CONVERT(date,'20030701',112) a, CAST('20030701' AS date) b, CONVERT(datetime2(3),'20030701',112) c, CAST('20030701' AS datetime2(3)) d, CONVERT(smalldatetime,'20030701',112) e, CAST('20030701' AS smalldatetime) f;
SELECT CONVERT(datetime,'17530101',112) a, CAST('17530101' AS datetime) b, CONVERT(date,'00010101',112) c, CAST('00010101' AS date) d;
-- errors
SELECT TRY_CONVERT(datetime,'20030230',112) a, TRY_CAST('20030230' AS datetime) b;
SELECT TRY_CONVERT(datetime,'17520101',112) a, TRY_CAST('17520101' AS datetime) b;
SELECT TRY_CONVERT(smalldatetime,'20790607',112) a, TRY_CAST('20790607' AS smalldatetime) b, TRY_CONVERT(smalldatetime,'20790606',112) c, TRY_CAST('20790606' AS smalldatetime) d;
SELECT TRY_CONVERT(datetime,'2003071',112) a, TRY_CAST('2003071' AS datetime) b;
SELECT TRY_CONVERT(datetime,'20031301',112) a, TRY_CAST('20031301' AS datetime) b;
