\set ON_ERROR_STOP 0
SELECT CAST(e ->> 'd' AS numeric(18,2)) FROM jsonb_array_elements('[{"d":-0.125},{"d":0.1234567890123456789012345678901234567890123},{"d":-0}]') e;
SELECT CAST(e ->> 'd' AS numeric(18,2)) FROM jsonb_array_elements('[{"d":12345678901234567.994}]') e;
SELECT CAST(e ->> 'i' AS integer) FROM jsonb_array_elements('[{"i":-0},{"i":-2147483648},{"i":2147483647}]') e;
SELECT CAST(e ->> 'b' AS boolean) FROM jsonb_array_elements('[{"b":true},{"b":false},{"b":0},{"b":1},{"b":null},{}]') e;
SELECT CAST(e ->> 'd' AS date) FROM jsonb_array_elements('[{"d":"0001-01-01"},{"d":"9999-12-31"},{"d":"2020-02-29"}]') e;
SELECT CAST(e ->> 's' AS varchar(5)), length(CAST(e ->> 's' AS varchar(5))) FROM jsonb_array_elements('[{"s":"abc  "},{"s":"é\t\"x"},{"s":"abcdefghij"}]') e;
SELECT CAST(e ->> 'x' AS integer) FROM jsonb_array_elements('[[1,2],5,"s"]') e;
SELECT CAST(jsonb_path_query_first(e, 'strict $."x"') #>> '{}' AS integer) FROM jsonb_array_elements('[[1,2]]') e;
SELECT jsonb_extract_path_text('{"a":1}', 'b') IS NULL;
