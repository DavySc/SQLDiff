-- PostgreSQL 18: left/right with a count from 0 up equal the substr forms (rows that differ are listed; none expected).
SELECT s, k, left(s, k), right(s, k)
FROM (VALUES (NULL::varchar(20)), (''), ('a'), ('ab '), ('ab  c'), ('- '), ('  x'), ('abc'), ('-[db]'), ('[a]]b]'), (' '), ('é𝄞x')) v(s)
CROSS JOIN generate_series(0, 6) k
WHERE left(s, k) IS DISTINCT FROM substr(s, 1, k)
   OR right(s, k) IS DISTINCT FROM substr(s, length(s) - k + 1, k);
SELECT 'checked', count(*) FROM (VALUES (1)) x;
