SET NOCOUNT ON;
DROP TABLE IF EXISTS #c; CREATE TABLE #c (c nchar(1));
INSERT INTO #c VALUES (N'a'), (N'b'), (N'.'), (N'['), (N']'), (N'"'), (N' ');
DROP TABLE IF EXISTS #s; CREATE TABLE #s (s nvarchar(10));
INSERT INTO #s VALUES (N'');
DECLARE @i int = 1;
WHILE @i <= 5 BEGIN
  INSERT INTO #s SELECT s + c FROM #s CROSS JOIN #c WHERE LEN(s + N'x') - 1 = @i - 1;
  SET @i += 1;
END;
SELECT '|' + s + '|' + ISNULL('<' + PARSENAME(s, 1) + '>', 'N') + '|' + ISNULL('<' + PARSENAME(s, 2) + '>', 'N') + '|' + ISNULL('<' + PARSENAME(s, 3) + '>', 'N') + '|' + ISNULL('<' + PARSENAME(s, 4) + '>', 'N')
FROM #s ORDER BY s COLLATE Latin1_General_BIN2;
