\set ON_ERROR_STOP 0
CREATE TEMP TABLE sel (Selected boolean);
\echo insert select of an integer into a boolean column:
WITH c AS (SELECT CASE WHEN 'x' LIKE '-%' THEN 0 ELSE 1 END AS Selected) INSERT INTO sel (Selected) SELECT Selected FROM c;
\echo insert values of an integer literal:
INSERT INTO sel (Selected) VALUES (1);
