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
  tools/check_corpus.py  validator
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
pg_create = "ok"                  # optional: "error" if creating it in PostgreSQL must fail

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
| p05-email-lookup | case-insensitive compare emulated; trailing spaces not | different, assumed, different |
| p06-paged-orders | OFFSET/FETCH vs LIMIT/OFFSET | equal ×3 |
| p07-insert-return-id | SCOPE_IDENTITY / RETURNING / FINAL TABLE | equal ×3 |
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
python3 corpus/tools/check_corpus.py                 # format, hygiene, coverage, manifest
python3 corpus/tools/check_corpus.py --pg "<dsn>"    # also create every PG file on a server
python3 corpus/tools/check_corpus.py --deny <file>   # leak scan with a runtime deny-list
```

`--pg` loads `catalog/pg.sql` and one fixture at a time inside a rolled-back
transaction on PostgreSQL 16 or later. PL/pgSQL bodies are only parsed at create
time, so a fixture with a binding mistake (m15, m18) is still created; the
behavioural claims of several PostgreSQL witnesses (p10, p12, m11, m15, m18, m21, m22)
were checked by running them. T-SQL and Db2 for i fixtures are not executed yet.

## Adding a case

1. Create `pairs/<id>/` or `mutations/<id>/` with the SQL files and a `case.toml`.
2. Use only the catalog schema, or add the objects to all three catalog files.
3. For a mutation, add its entry to `mutations/CATALOG.toml`.
4. Add the directory to `MANIFEST.toml`.
5. Run the validator.
