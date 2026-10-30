create temp table s (id int, CustomFields text, Tags text GENERATED ALWAYS AS (CASE WHEN json_typeof(CAST(CustomFields AS json) -> 'Tags') IN ('object', 'array') THEN CAST(CAST(CustomFields AS json) -> 'Tags' AS text) END) STORED, Brand varchar(4000) GENERATED ALWAYS AS (CASE WHEN json_typeof(CAST(CustomFields AS json) -> 'Brand') NOT IN ('object', 'array') THEN CAST(CustomFields AS json) ->> 'Brand' END) STORED);
insert into s (id, CustomFields) values (1, '{ "CountryOfManufacture": "China", "Tags": ["USB Powered"] }'), (2, NULL), (3, '{"Brand":{"a":1},"Tags":"x"}'), (4,'{"Brand":12.50e1,"Tags":[ 1 ,2 ]}');
select id || '|' || coalesce(Tags,'NULL') || '|' || coalesce(Brand,'NULL') from s order by id;
select 'esckey|' || coalesce(cast(cast('{"Tags":[1]}' as json) -> 'Tags' as text),'NULL');
select 'esckeyv|' || coalesce(cast('{"R":"x"}' as json) ->> 'R','NULL');
