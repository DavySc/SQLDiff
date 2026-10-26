# Cross-dialect witness check

`xwmut.py` measures expression-level verdicts across dialects. It takes a
`sqldiff convert` output tree of the WideWorldImporters OLTP project, mutates
the converted side of every procedure and function inside WHERE clauses (code
tokens only, with `../mutation/sqltok.py`) and diffs the T-SQL source against
each mutant:

- behaviour-changing: `=`→`<>`, `<>`→`=`, `>`→`>=`, `<`→`<=`, `AND`→`OR`,
  integer literal +1, `IS NULL`→`IS NOT NULL`, a dropped WHERE of a DELETE or
  UPDATE. None may come back equal.
- meaning-preserving: `a = b`→`b = a`, `a = b`→`NOT (a <> b)`, parentheses
  around the WHERE condition. None may get an `equiv.witness` finding (or
  turn a pair "different" that was not).

```sh
cargo build --release -p sqldiff-cli
W=ext/sql-server-samples/samples/databases/wide-world-importers/wwi-ssdt/wwi-ssdt
target/release/sqldiff convert "$W" -o /tmp/conv-postgres --to postgres
python3 eval/cross-dialect-witness/xwmut.py "$W" /tmp/conv-postgres postgres target/release/sqldiff
```

Results on 2026-10-26 (main before and after the witness search):

| | mutants | witnessed | behaviour-changing mutants of equal pairs reported different | false equal | witness on a preserving edit |
|---|---|---|---|---|---|
| PostgreSQL, before | 297 | 0 | 0 of 30 | 0 | 0 |
| PostgreSQL, after | 297 | 30 | 30 of 30 | 0 | 0 |
| Db2 for i, before | 304 | 0 | 0 of 30 | 0 | 0 |
| Db2 for i, after | 304 | 30 | 30 of 30 | 0 | 0 |

Mutants in routines that were already "unknown" stay unknown: most sit inside
IF blocks (control flow is not lowered) or assign local variables whose later
use is not traced.
