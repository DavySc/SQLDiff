SET NOCOUNT ON;
SELECT d FROM OPENJSON(N'[{"d":-0.125},{"d":0.1234567890123456789012345678901234567890123},{"d":12345678901234567.994},{"d":-0}]') WITH (d decimal(18,2) '$.d');
SELECT i FROM OPENJSON(N'[{"i":-0},{"i":-2147483648},{"i":2147483647}]') WITH (i int);
SELECT b FROM OPENJSON(N'[{"b":true},{"b":false},{"b":0},{"b":1},{"b":null},{}]') WITH (b bit);
SELECT d FROM OPENJSON(N'[{"d":"0001-01-01"},{"d":"9999-12-31"},{"d":"2020-02-29"}]') WITH (d date);
SELECT s, LEN(s) l FROM OPENJSON(N'[{"s":"abc  "},{"s":"é\t\"x"},{"s":"abcdefghij"}]') WITH (s nvarchar(5));
SELECT x FROM OPENJSON(N'[[1,2],5,"s"]') WITH (x int '$.x');
GO
SELECT x FROM OPENJSON(N'[[1,2]]') WITH (x int 'strict $.x');
GO
SELECT d FROM OPENJSON(N'[{"d":1234567890123456789.5}]') WITH (d decimal(18,2) '$.d');
GO
