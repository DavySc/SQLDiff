SET NOCOUNT ON;
DECLARE @t TABLE (k varchar(20), s nvarchar(max));
INSERT INTO @t VALUES
 ('long2nd', N'a.' + REPLICATE(N'b', 129)), ('long1st', REPLICATE(N'b', 129) + N'.a'),
 ('esc128', N'[' + REPLICATE(N'a', 127) + N']]]'), ('esc129', N'[' + REPLICATE(N'a', 128) + N']]]'),
 ('dq128', N'"' + REPLICATE(N'a', 127) + N'"""'), ('sp129', REPLICATE(N' ', 129)),
 ('sur64', REPLICATE(N'𝄞', 64)), ('sur65', REPLICATE(N'𝄞', 65)), ('bmp128', REPLICATE(N'é', 128)), ('bmp129', REPLICATE(N'é', 129)),
 ('long4', N'a.b.c.' + REPLICATE(N'd', 129)), ('trail129', REPLICATE(N'a', 128) + N' ');
SELECT k + '|' + ISNULL(CAST(LEN(PARSENAME(s, 1)) AS varchar(10)), 'N') + '|' + ISNULL(CAST(LEN(PARSENAME(s, 2)) AS varchar(10)), 'N') + '|' + ISNULL(CAST(LEN(PARSENAME(s, 3)) AS varchar(10)), 'N') + '|' + ISNULL(CAST(LEN(PARSENAME(s, 4)) AS varchar(10)), 'N') FROM @t;
