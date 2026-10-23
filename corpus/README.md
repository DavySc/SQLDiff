# Test corpus

Synthetic SQL fixtures for SQLDiff: paired routines in T-SQL, PostgreSQL and Db2 for i,
and the mutation catalog of design §12. Everything here is plain data, so the Rust
core, the CLI and any future oracle harness can consume it without code from this
directory. All fixtures are synthetic (see `MANIFEST.toml`); nothing is derived from
private code (design §5.6).

```
corpus/
  MANIFEST.toml          origin and licence of every fixture
  catalog/               the shared schema every case runs against
    tsql.sql pg.sql db2i.sql
  pairs/<id>/            cross-dialect pairs with expected verdicts
    case.toml  tsql.sql  pg.sql  db2i.sql  [extra sides]
  mutations/
    CATALOG.toml         design §12 items, each mapped to its case
    <id>/                one plausible conversion mistake per case
      case.toml  original.<dialect>.sql  mutated.<dialect>.sql
  packs/                 synthetic packs referenced by cases
  oracle/<id>.toml       seed data, calls and fault points for the execution oracle
  tools/check_corpus.py  validator
  tools/fault_oracle.py  fault-injection execution oracle (PostgreSQL, SQL Server)
```

Dialect keys are `tsql`, `pg` and `db2i`. Files are UTF-8 without BOM, LF line
endings, ending in a newline, no trailing whitespace.

## The catalog

`catalog/<dialect>.sql` creates the same small order/stock schema in each dialect:
`customer`, `item`, `stock`, `stock_log`, `item_price`, `price_feed` (no key, used as
a merge source), `orders`, `order_line`, `app_lock` (rows for lock emulation),
`audit_event`, and one routine, `log_stock_change(item, delta, reason)`, whose
signature the binding checks rely on. T-SQL objects are in `dbo` and the database
collation is assumed case-insensitive. Db2 for i objects are unqualified (SQL naming)
and the tables are assumed journaled.

Expectations that rely on a catalog fact (a primary key, NOT NULL) list it under
`discharged`; those are proven facts, not assumptions.

## case.toml

```toml
id = "p05-email-lookup"           # must equal the directory name
title = "Case-insensitive lookup and trailing spaces"
refs = ["design §7 NULL, strings, types"]   # where in the design the behaviour is described
tags = ["collation", "strings"]   # optional, free-form
description = """..."""           # optional
note = "..."                      # optional
pack = "../../packs/x.toml"       # optional, pack the case needs (relative to the case)

# mutations only
catalog_item = "..."              # the design §12 text, as in mutations/CATALOG.toml
detected_by = "early-check"       # parse-lint | early-check | behavioural
milestone = "M3"                  # design §14 milestone whose gate covers it

[sides.<name>]                    # pairs use the dialect as the name; mutations use
dialect = "pg"                    # "original" and "mutated"
file = "pg.sql"
create = "ok"                     # optional: "error" if creating it on its engine must fail

[[expect]]                        # one per compared pair of sides
a = "tsql"
b = "pg"
verdict = "different"             # equal | equal-under-assumptions | different | unknown
classes = ["semantic"]            # design §2 change classes
assumptions = ["..."]             # required for equal-under-assumptions
discharged = ["..."]              # catalog facts the verdict relies on
facts = ["V8"]                    # design §13 facts the verdict depends on
witnesses = ["..."]               # required for different: concrete scenario, both outcomes

[[substitutes]]                   # optional: out-of-engine handlers (design §7) that an
side = "mutated"                  # adapter or pack would normally supply
replaces = "..."
handler = "..."
invocation = "..."
error_propagation = "..."
```

`classes` lists the classes of the findings that justify the verdict; for an `equal`
verdict it lists the noise that must be counted but not shown (`cosmetic`,
`structural`). Witness text is the reference phrasing for humans; tests should match
on verdict, classes and assumptions, not on witness wording.

A verdict that depends on a §13 fact (`facts`) is the expected verdict once that fact
is confirmed. Until then the tool may cap it (design §12, oracle availability).

## Pairs

| Case | Topic | Verdicts (tsql/pg, tsql/db2i, pg/db2i) |
|---|---|---|
| p01-move-stock | §7 worked example: return codes vs *NONE vs caller transaction | different ×3 |
| p02-upsert-price | MERGE HOLDLOCK / ON CONFLICT / Db2 MERGE | equal, assumed, assumed |
| p03-lookup-by-key | three single-row contracts made equal by a primary key | equal ×3 |
| p04-display-name | NULL-safe concatenation in a view | equal ×3 |
| p05-email-lookup | case-insensitive compare emulated; trailing spaces and key uniqueness not | different ×3 |
| p06-paged-orders | OFFSET/FETCH vs LIMIT/OFFSET | equal ×3 |
| p07-insert-return-id | SCOPE_IDENTITY / RETURNING / FINAL TABLE; T-SQL continues after a failed INSERT | different ×2, equal |
| p08-delete-orphans | anti-join vs NOT EXISTS | equal ×3 |
| p09-error-rollback | TRY/CATCH + XACT_ABORT / procedure COMMIT / EXIT handler | assumed ×3 |
| p10-temp-state-rollback | table variable vs temp table in an exception block vs NOT LOGGED DGTT | different ×3 |
| p11-string-aggregation | STRING_AGG / string_agg / LISTAGG | assumed ×3 |
| p12-days-between | DATEDIFF vs date subtraction vs DAYS(), plus a near-miss side | equal ×3, near-miss different |
| p13-named-lock | sp_getapplock / advisory lock / row-lock emulation | assumed ×3 |

## Mutations

`mutations/CATALOG.toml` lists every §12 item and its case; the validator fails if
one is missing. Most mutations stay in one dialect so they can run against a single
oracle; m12, m18, m19 and m22 are cross-dialect because the mistake only exists in a
conversion. m23 uses composed output with provenance markers defined by
`packs/synthetic-generator.toml` (a draft; the pack format is not fixed yet).

## Validation

```
python3 corpus/tools/check_corpus.py                  # format, hygiene, coverage, manifest
python3 corpus/tools/check_corpus.py --pg "<dsn>"     # also create every PostgreSQL file
python3 corpus/tools/check_corpus.py --mssql "<cmd>"  # also create every T-SQL file
python3 corpus/tools/check_corpus.py --deny <file>    # leak scan with a runtime deny-list
```

Target engines are **PostgreSQL 18** and **SQL Server 2022**; CI runs both as
service containers. `--pg` loads `catalog/pg.sql` and one fixture at a time inside a
rolled-back transaction. `--mssql` takes a `sqlcmd` command line (it must use `-b`
to stop on errors and `-I` for QUOTED_IDENTIFIER ON) and creates `catalog/tsql.sql`
plus one fixture at a time in a fresh case-insensitive database, for example:

```
python3 corpus/tools/check_corpus.py --mssql \
  "docker exec -i mssql /opt/mssql-tools18/bin/sqlcmd -S localhost -U sa -P <pw> -C -b -I"
```

PL/pgSQL and T-SQL bodies are only partly bound at create time, so fixtures with a
binding mistake (m15, m16, m18) are still created. Witnesses checked by execution:
PostgreSQL 18 for p10, p12, m11, m15, m18, m21, m22; SQL Server 2022 for p05, p10,
m05, m10, m13, m14, m16, m22. Db2 for i fixtures are not executed (no container
oracle, design §12).

## Execution oracle

`tools/fault_oracle.py` runs the T-SQL and PostgreSQL sides of a case on real
engines and compares what a caller observes: whether an error reached the client
(and its codes, when both sides run on one engine), the rows returned, and the
durable state of the observed tables read in a fresh session. Each scenario runs
once without a fault and once per fault point; a fault point makes every write to
one table fail with a constraint violation. The specs live in `oracle/<case>.toml`:

```toml
case = "p01-move-stock"
pairs = [["tsql", "pg"]]                  # side names from case.toml
observe = ["stock(item_id, qty)"]         # durable state; leave out time-dependent columns
faults = ["stock", "price_feed:delete"]   # write fault, or delete fault [":key"]
seed = "INSERT INTO ..."                  # shared, or seed.tsql / seed.pg
# reproducible = false, why = "..."       # a "different" the oracle cannot show
# known_divergence = "..."                # an open contradiction of case.toml

[[scenario]]
name = "caller checks the return code"
call.tsql = "DECLARE @rc int; EXEC @rc = dbo.move_stock 1, 3; SELECT @rc;"
call.pg = "SELECT move_stock(1, 3);"     # or call = "..." for both
```

```
python3 corpus/tools/fault_oracle.py \
  --pg "host=localhost user=postgres password=postgres dbname=postgres" \
  --mssql "docker exec -i mssql /opt/mssql-tools18/bin/sqlcmd -S localhost -U sa -P <pw> -C -I" \
  [--sqldiff target/debug/sqldiff] [p01 m1 ...]
```

It fails when a case expected to be equal diverges, when a "different" case shows
no divergence, and (with `--sqldiff`) when the engine says "equal" for a pair that
diverges. See `docs/validation.md` for how this fits with the golden runner.

## Adding a case

1. Create `pairs/<id>/` or `mutations/<id>/` with the SQL files and a `case.toml`.
2. Use only the catalog schema, or add the objects to all three catalog files.
3. For a mutation, add its entry to `mutations/CATALOG.toml`.
4. Add the directory to `MANIFEST.toml`.
5. If it has T-SQL or PostgreSQL sides, add `oracle/<id>.toml` and run the oracle.
6. Run the validator, then `cargo test -p sqldiff-corpus`: a new expectation the
   engine cannot meet yet goes into `crates/sqldiff-corpus/known-failing.toml`.
