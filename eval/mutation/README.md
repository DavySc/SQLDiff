# Mutation gate

Measures how often `sqldiff diff` notices a real behaviour change in real
code, and checks that it never calls a changed routine equal or an
unchanged one different.

Sources are pinned in `sources.json`: the WideWorldImporters stored
procedures (T-SQL) and pg_partman's functions and procedures (PostgreSQL).
For each routine file the harness makes:

- one mutant per operator: `=`→`<>`, `>`→`>=`, integer literal +1,
  `AND`→`OR`, `IS NULL`→`IS NOT NULL`, `DESC`→`ASC`, `MAX(`→`MIN(`,
  `LEFT JOIN`→`INNER JOIN`, ` + `→` - `, a dropped `INSERT`/`UPDATE`/`DELETE`
  statement and a dropped `AND ...` predicate line;
- three preserving edits: re-indentation with trailing blanks and spaces
  after commas, flipped keyword case, and added comment lines.

`sqltok.py` tokenizes both dialects, so mutations only change code tokens
and preserving edits only change whitespace, keyword case and comments;
string literals and existing comments are never touched. A PostgreSQL routine
body in dollar quotes (`AS $$ ... $$`) is code; other dollar-quoted text is a
string.

The gate (`.github/workflows/mutation.yml`) fails when

- any mutant is reported `equal` or `equal-under-assumptions`,
- any preserving edit is reported `different`,
- fewer than 80% of the mutants of a set are reported `different`.

A mutant that does not change behaviour at all (for example `EXISTS (SELECT
1 ...)` → `EXISTS (SELECT 2 ...)`) may be listed in `equivalent.json` with
the reason it is equivalent. Listed mutants leave the denominator, and the
gate fails if one of them is ever reported `different`.

Run it locally:

```sh
cargo build --release -p sqldiff-cli
python3 eval/mutation/mutate.py fetch /tmp/ext
python3 eval/mutation/mutate.py gen tsql /tmp/ext /tmp/cases/tsql
python3 eval/mutation/mutate.py gen pg /tmp/ext /tmp/cases/pg
python3 eval/mutation/mutate.py run /tmp/cases/tsql /tmp/tsql.json
python3 eval/mutation/mutate.py run /tmp/cases/pg /tmp/pg.json
python3 eval/mutation/mutate.py gate /tmp/tsql.json /tmp/pg.json
```

`gen --seed N` picks other files and other mutation sites, which is how the
held-out runs are made.
