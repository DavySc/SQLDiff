# SQLDiff

A semantic diff for **T-SQL (SQL Server 2022), PostgreSQL 18 (incl. PL/pgSQL) and Db2 for i SQL PL**. It tells you what changed between two SQL artifacts, whether the *meaning* changed, and gives a concrete scenario for every behavioural difference. It is built for code kept in sync across dialects and for reviewing conversions.

The full design is in [docs/design.md](docs/design.md). Its first rule: **never a false "equal"**. When equivalence can't be shown, the verdict is `unknown` or `equal under assumptions`.

## Quick start

```sh
cargo build --release
B=target/release/sqldiff

# Conversion review (T-SQL -> Db2 for i): the worked example of design section 7
$B diff tests/fixtures/movestock.tsql.sql tests/fixtures/movestock.db2i.sql --dialect-a tsql --dialect-b db2i

# Version review in one dialect: a pure reformat reports EQUAL with zero findings
$B diff tests/fixtures/movestock.tsql.sql tests/fixtures/movestock_reformatted.tsql.sql -d tsql

# Lint one file: residue from other dialects, missing terminator before THROW
$B check tests/fixtures/catch_rethrow.tsql.sql -d tsql

# Collection drift report: pair .sql files of two trees by name
$B tree src/tsql src/db2 --dialect-a tsql --dialect-b db2i

# Machine output for CI
$B diff a.sql b.sql -d postgres -f json      # or -f sarif
```

Exit codes: `0` equal or equal under assumptions, `1` different or unknown, `2` usage or I/O error.

Example output for the worked example:

```
Verdict: DIFFERENT
4 findings (3 transactional, 1 semantic)

[2] TRANSACTIONAL · critical · different   txn.atomicity 74561e0a6f3688b0
    If the INSERT into StockLog fails (B line 6), side A (T-SQL) undoes the earlier changes
    (ROLLBACK on the error path at A line 6); side B (Db2 for i) exits with the UPDATE of Stock
    (B line 5) already durable (COMMIT = *NONE at B line 3: no commitment control, every change
    is durable immediately).
    assumes: ASM-TSQL-CALLER-XACT-ABORT-OFF, ASM-NO-TRIGGERS
```

## What works today

| Layer (design §3) | Status |
|---|---|
| 1 Parsing | Hand-written lossless lexers and a statement-level CST (rowan) for all three dialects: `print(parse(x)) == x` for any input (fuzz-tested). Procedural compounds (blocks, IF, loops, CASE, Db2 handlers, T-SQL TRY/CATCH, PL/pgSQL EXCEPTION), T-SQL `GO` batches and inferred terminators, Db2 labels and `SET OPTION`, nested PL/pgSQL/SQL parsing of dollar-quoted bodies with offsets into the outer file. Expressions are kept flat for now. |
| 5 Structural diff | Same-dialect: statements aligned by normalized content (not lines), cosmetic changes counted and hidden, edits, additions, removals and moves classified, including moves across transaction boundaries. T-SQL identifier case folds only under an explicit collation assumption. |
| Behavioural checks | Summary-level transaction/error model: atomicity when a later write fails, Db2 `*NONE` statement atomicity, caller-owned transactions vs `*NONE`, return-code vs raised-error contract. Cross-dialect write pairing (incl. MERGE vs `ON CONFLICT` upserts, Db2 `FINAL TABLE`). |
| Lints | Data-driven residue rules ([profiles/residue.toml](crates/sqldiff-core/profiles/residue.toml)) and the missing-`;`-before-`THROW` hazard. |
| Interfaces | CLI `diff` / `check` / `parse` / `tree`, text, JSON and SARIF 2.1.0. Findings carry stable fingerprints that survive reformatting. |

Not yet built: binding against catalogs, the semantic IR and CFG with error edges (M4), statement-level cross-dialect equivalence, idiom lifting, packs/adapters, MCP server, GUI. See design §14 for the milestones.

## Layout

```
crates/sqldiff-syntax   lexers, parser, CST helpers (no analysis)
crates/sqldiff-core     findings model, normalization, diff engines, checks, renderers
crates/sqldiff-cli      the `sqldiff` binary
tests/fixtures          synthetic fixtures (all written for this repository)
scripts/leak-scan.sh    deny-list scan (design §5.6); the list itself is supplied at run time
```

Target engine versions default to SQL Server 2022, PostgreSQL 18 and Db2 for i 7.6 (`Dialect::default_version`).

## Development

```sh
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```
