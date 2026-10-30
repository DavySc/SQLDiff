-- Return type of CHARINDEX over derived strings: max haystack -> bigint, otherwise int.
SET NOCOUNT ON;
DECLARE @m nvarchar(max) = N'a,b', @n nvarchar(4000) = N'a,b', @vm varchar(max) = 'a,b', @vn varchar(100) = 'a,b';
DROP TABLE IF EXISTS #t;
SELECT
  var_max       = CHARINDEX(N',', @m),
  var_n         = CHARINDEX(N',', @n),
  var_vmax      = CHARINDEX(',', @vm),
  var_vn        = CHARINDEX(',', @vn),
  replace_n     = CHARINDEX(N',', REPLACE(@n, N'x', N'y')),
  replace_max   = CHARINDEX(N',', REPLACE(@m, N'x', N'y')),
  replace_n_maxarg = CHARINDEX(N',', REPLACE(@n, N'x', @m)),
  concat_n_n    = CHARINDEX(N',', @n + N','),
  concat_n_max  = CHARINDEX(N',', @n + @m),
  ltrim_n       = CHARINDEX(N',', LTRIM(RTRIM(@n))),
  ltrim_max     = CHARINDEX(N',', LTRIM(RTRIM(@m))),
  upper_max     = CHARINDEX(N',', UPPER(@m)),
  lower_n       = CHARINDEX(N',', LOWER(@n)),
  left_max      = CHARINDEX(N',', LEFT(@m, 2)),
  right_n       = CHARINDEX(N',', RIGHT(@n, 2)),
  substring_max = CHARINDEX(N',', SUBSTRING(@m, 1, 2)),
  substring_n   = CHARINDEX(N',', SUBSTRING(@n, 1, 2)),
  isnull_n_max  = CHARINDEX(N',', ISNULL(@n, @m)),
  isnull_max_n  = CHARINDEX(N',', ISNULL(@m, @n)),
  cast_max      = CHARINDEX(N',', CAST(@n AS nvarchar(max))),
  cast_n        = CHARINDEX(N',', CAST(@m AS nvarchar(10))),
  literal       = CHARINDEX(N',', N'a,b'),
  start_max     = CHARINDEX(N',', @m, 1),
  start_n       = CHARINDEX(N',', @n, 1),
  len_max       = LEN(@m),
  len_n         = LEN(@n)
INTO #t;
SELECT c.name, t.name AS type FROM tempdb.sys.columns c JOIN sys.types t ON t.user_type_id = c.user_type_id
WHERE c.object_id = OBJECT_ID('tempdb..#t') ORDER BY c.column_id;
-- Bigint start beyond int range, and a bigint start in range.
SELECT CHARINDEX(N',', @m, CAST(5000000000 AS bigint)) AS huge_start, CHARINDEX(N',', @m, CAST(2 AS bigint)) AS start2;
