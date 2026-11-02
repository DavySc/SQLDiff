# Money probes (review of PR #255, 2026-11-02)

Re-run of the PR's generator (`gen.py` in the repo) on SQL Server 2022 CU27-GDR (16.0.4305.5)
and PostgreSQL 18.6, plus `extra-*.sql` for bigint operands, a chained sum with an
intermediate overflow, money-to-decimal rounding and money with a decimal.

Results of `cmp.py money-tsql.out money-pg.out`: add/sub/addint/mulint/intmul equal except
SQL Server's money-range errors; toint/tobigint/todec/todec0/todec4 equal on all 17 values;
eq equal; mul/div/divint/round/tovarchar/tofloat differ; abs and neg differ for
-922337203685477.5808 only (SQL Server overflows).

Extra: `bi + m`, `bi * m`, `m - bi` are money in sys.columns; `bigint 1e15 * money 0`
fails on SQL Server (conversion of the bigint to money) and stores 0 on PostgreSQL;
`6e14 + 6e14 - 5e14` fails on SQL Server (intermediate sum) and stores 7e14 on PostgreSQL.
