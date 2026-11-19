SQL Server 2022: DATEADD converts a non-integer number argument to int by truncating toward zero
(decimal and float literals, both signs; 2147483647.5 as float is accepted). PostgreSQL 18 side:
`trunc(x)::integer`. Used by `date_add` in `equiv/canon.rs`.
