SET NOCOUNT ON;
DECLARE @t TABLE (k varchar(20), s nvarchar(max));
INSERT INTO @t VALUES
 ('plain128', REPLICATE(N'a', 128)), ('plain129', REPLICATE(N'a', 129)), ('plain200', REPLICATE(N'a', 200)),
 ('brk128', N'[' + REPLICATE(N'a', 128) + N']'), ('brk129', N'[' + REPLICATE(N'a', 129) + N']'),
 ('brk126', N'[' + REPLICATE(N'a', 126) + N']'),
 ('two128', REPLICATE(N'a', 128) + N'.' + REPLICATE(N'b', 128)),
 ('two64', REPLICATE(N'a', 64) + N'.' + REPLICATE(N'b', 63)),
 ('two64b', REPLICATE(N'a', 64) + N'.' + REPLICATE(N'b', 64)),
 ('tab', N'a' + CHAR(9) + N'b.c'), ('nl', N'a' + CHAR(10) + N'b'), ('quote', N'a''b'), ('at', N'@a.#b'),
 ('uni', N'é.ü𝄞'), ('bslash', N'a\b'), ('pipe', N'a|b'), ('null', NULL), ('empty', N''),
 ('five', N'a.b.c.d.e'), ('four', N'a.b.c.d'), ('nested', N'[a[b]'), ('dq', N'"a""b"');
SELECT k + '|' + ISNULL('<' + PARSENAME(s, 1) + '>', 'N') + '|' + ISNULL('<' + PARSENAME(s, 2) + '>', 'N') + '|' + ISNULL('<' + PARSENAME(s, 3) + '>', 'N') + '|' + ISNULL('<' + PARSENAME(s, 4) + '>', 'N') + '|' + ISNULL(CAST(LEN(PARSENAME(s, 1)) AS varchar(10)), 'N') FROM @t;
SELECT 'piece' + '|' + ISNULL(PARSENAME(N'a.b', 0), 'N') + '|' + ISNULL(PARSENAME(N'a.b', 5), 'N') + '|' + ISNULL(PARSENAME(N'a.b', -1), 'N') + '|' + ISNULL(PARSENAME(N'a.b', NULL), 'N');
DECLARE @x varchar(20) = 'a.b';
SELECT 'type' + '|' + CAST(SQL_VARIANT_PROPERTY(PARSENAME(@x, 1), 'BaseType') AS varchar(20)) + '|' + CAST(SQL_VARIANT_PROPERTY(PARSENAME(@x, 1), 'MaxLength') AS varchar(20));
