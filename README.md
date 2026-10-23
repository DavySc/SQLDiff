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

# Collection drift report: pair .sql files of two trees by pack pairing rules
$B tree src/tsql src/db2 --dialect-a tsql --dialect-b db2i --pack ../private-pack

# diff, check and tree read sqldiff.toml, user config and --pack like the source
# commands: pairing rules, dialect_map, limits, overrides, declared assumptions.
# Without a dialect flag, the dialect comes from dialect_map or the path (x.pg.sql).
$B diff tests/fixtures/movestock.tsql.sql tests/fixtures/movestock.pg.sql

# Machine output for CI
$B diff a.sql b.sql -d postgres -f json      # or -f sarif

# Review: accept a deliberate difference by fingerprint; later runs suppress it
$B review accept 06ccfd05e423182d --pair a.sql b.sql -c "callers map codes"
$B diff a.sql b.sql --redact -f sarif         # shareable: names replaced by stable placeholders

# Sources, packs and snapshots (design section 5)
$B pairs tests/fixtures --diff                        # pair by pack rules, diff every pair
$B pairs git:.@main//db --pack ../private-pack --diff  # committed tree at a revision
$B snapshot git:.@v1.2//db -o v1.2.json --history     # canonical, content-hashed bundle
$B pairs v1.2.json --diff                             # offline, from the bundle
$B config                                             # merged config and its layers

# Pair view in the browser (statement-aligned, findings panel, live scratchpad)
target/release/sqldiff-gui serve tests/fixtures/movestock.tsql.sql tests/fixtures/movestock.db2i.sql --dialect-a tsql --dialect-b db2i
target/release/sqldiff-gui export --report report.json -o pair.html   # static, read-only

# Convert mode: type T-SQL on the left, checked PostgreSQL appears on the right as you type
target/release/sqldiff-gui convert                       # starts with a sample
target/release/sqldiff-gui convert my_proc.sql --stdout  # print the conversion and per-statement badges
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
| 0 Acquisition and configuration | Layered config (built-in < shared packs < private packs < project `sqldiff.toml` < user < command line) with provenance hashes; validated, schema-versioned packs ([docs/packs-and-config.md](docs/packs-and-config.md)); secrets rejected in favour of credential references. Filesystem, git and snapshot-bundle adapters ([docs/snapshot-bundle.md](docs/snapshot-bundle.md)); out-of-process adapters over a JSON stdio protocol with chunked, hash-verified transfer ([docs/adapter-protocol.md](docs/adapter-protocol.md)). Pack-driven pairing, limit rules, severity overrides and declared assumptions. |
| Interfaces | CLI `diff` / `check` / `parse` / `tree` / `objects` / `pairs` / `snapshot` / `config` / `pack` / `serve` / `queue` / `assumptions` / `comment` / `open`, text, JSON and SARIF 2.1.0. Stdio MCP server `sqldiff-mcp` with `diff`, `check`, `finding`, `queue`, `assumptions`, `set_assumption`, `comment_text` and `open_in_gui`. Review comments come from a deterministic phrase grammar (no AI). Findings carry stable fingerprints that survive reformatting. |
| Review workflow | Review store keyed by fingerprint and snapshot with expiry and optional encryption at rest; accepted findings and in-source deliberate-difference markers suppressed (kept in JSON and SARIF `suppressions`); author-declared assumptions from pack markers; redacted export with salted stable placeholders, leak-checked; per-content-hash report cache ([docs/review-and-export.md](docs/review-and-export.md)). |
| GUI | `sqldiff-gui serve` / `export` / `report`: two files open the pair view, two directories open a review queue (filter by class, dialect pair, folder, review state; sort by severity). Pair view aligned by statement with a location pane, findings with accept / reject / comment, assumption panel (deny or confirm an assumption and watch verdicts update), provenance bar (sources, snapshot, packs and hashes, dialect versions, refresh), command palette (Ctrl+K), scratchpad that re-diffs on edit, deep links `?a=<path>&b=<path>#f=<fingerprint>`, session report as Markdown (`--redact`). Stack decision in [docs/gui-spike.md](docs/gui-spike.md). |
| Conversion (P0) | `sqldiff-gui convert`: rule-based T-SQL → PostgreSQL that updates live; every statement gets a badge (converted, under assumptions, needs a decision, differs, not converted) from its rules plus the engine's diff of input against output. Constructs without a rule stay as comments, never guessed. See [docs/convert-mode.md](docs/convert-mode.md). |

Not yet built: binding against catalogs, the semantic IR and CFG with error edges (M4), statement-level cross-dialect equivalence, idiom lifting, catalog-aware binding of adapter catalogs, the rest of the GUI (review queue, lens switch, transaction overlay). See design §14 for the milestones.

## MCP server

`sqldiff-mcp` exposes the same engine to agents as a local stdio MCP server (design §6.1). It reads files, never writes, and sends nothing over the network. It resolves packs and config like the CLI (`--pack`, `--config`, `--user-config`, `--isolated`); `--gui-url` (or `$SQLDIFF_GUI_URL`) sets where `open_in_gui` links point.

```sh
cargo build --release
claude mcp add sqldiff -- "$PWD/target/release/sqldiff-mcp"
```

Or in any client's `mcpServers` config: `{"sqldiff": {"command": "/path/to/sqldiff-mcp"}}`.

| Tool | Arguments | Returns |
|---|---|---|
| `diff` | `a`, `b`: `{dialect, path \| text, name?, version?}`; `max_findings` (20), `excerpt_lines` (3) | Verdict and finding cards, most severe first; cosmetic and structural noise counted |
| `check` | `artifact`: as above | Lint findings for one file |
| `finding` | `fingerprint` | One finding from this session in full: both locations with excerpts and the text of each assumption |
| `queue` | `source` (directory, bundle, `git:…`, `adapter:…`; default `[source]`), `dialect_a`/`dialect_b`, `classes`, `min_severity`, `verdicts`, `folder`, `review`, `limit` (25) | Every pair of the source diffed and ranked: unreviewed, verdict, severity, whether one assumption decides the verdict, recency, size. A few lines per pair, each with a pair id |
| `assumptions` | `pair` id or a finding `fingerprint` | The pair's assumption ledger: open, declared, confirmed or denied; findings relying on each; which flip the verdict |
| `set_assumption` | `id`, `holds` (true, false or null to reset) | Session-local flip; every held pair's verdict is re-derived and changes are listed. A denied assumption makes the findings relying on it unknown |
| `comment_text` | `finding_ids`, `excerpt_lines` (1), `fingerprints` (true) | A plain-text review comment from the phrase grammar: same findings, same text |
| `open_in_gui` | `finding_id` or `pair`, `gui_url` | `{gui}/?a=<path>&b=<path>#f=<fingerprint>` and the `sqldiff-gui serve` command for the pair |

The CLI has the same review commands: `sqldiff queue [SOURCE]`, `sqldiff assumptions A B`, `sqldiff comment A B [--finding FP]` and `sqldiff open A B [--finding FP]`. `--deny ID` / `--confirm ID` flip an assumption for that run only. Review state comes from the review store once it lands; until then every finding counts as unreviewed.

Each result carries a readable text block and the same data as `structuredContent` JSON.

## Layout

```
crates/sqldiff-syntax   lexers, parser, CST helpers (no analysis)
crates/sqldiff-core     findings model, normalization, diff engines, checks, renderers
crates/sqldiff-source   packs, layered config, adapter protocol, fs/git/bundle adapters, pairing
crates/sqldiff-review   review store, suppression, author markers, redaction, cache
crates/sqldiff-cli      the `sqldiff` binary
crates/sqldiff-mcp      the `sqldiff-mcp` stdio MCP server
crates/sqldiff-convert  rule-based dialect conversion, checked unit by unit by the engine
crates/sqldiff-gui      the `sqldiff-gui` review queue, pair view and convert mode (local server + embedded pages)
tests/fixtures          synthetic fixtures (all written for this repository)
docs/examples/pack      synthetic example pack using every section
scripts/leak-scan.sh    deny-list scan (design §5.6); the list itself is supplied at run time
```

Target engine versions default to SQL Server 2022, PostgreSQL 18 and Db2 for i 7.6 (`Dialect::default_version`).

## Development

```sh
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```
