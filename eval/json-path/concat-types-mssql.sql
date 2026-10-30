SET NOCOUNT ON;
CREATE TABLE #c (a nvarchar(50), b nvarchar(50), m nvarchar(max), v varchar(30), s1 AS concat(a, N' ', b), s2 AS concat(a, N' ', m), s3 AS concat(v, ' ', v), s4 AS concat(a, ' ', v));
SELECT c.name + N' ' + TYPE_NAME(c.system_type_id) + N' ' + CAST(c.max_length AS nvarchar(10)) FROM tempdb.sys.columns c WHERE c.object_id = OBJECT_ID(N'tempdb..#c') AND c.name LIKE 's%';
