SET NOCOUNT ON;
CREATE TABLE #s (id int, CustomFields nvarchar(max), Tags AS (json_query([CustomFields],N'$.Tags')), Brand AS (json_value([CustomFields],N'$.Brand')));
INSERT INTO #s (id, CustomFields) VALUES (1, N'{ "CountryOfManufacture": "China", "Tags": ["USB Powered"] }'), (2, NULL), (3, N'{"Brand":{"a":1},"Tags":"x"}'), (4,N'{"Brand":12.50e1,"Tags":[ 1 ,2 ]}');
SELECT CONCAT(id, N'|', ISNULL(Tags,N'NULL'), N'|', ISNULL(Brand,N'NULL')) FROM #s ORDER BY id;
