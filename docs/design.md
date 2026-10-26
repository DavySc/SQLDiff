# SQL Semantic Diff — Design (generic master)

Draft v4 · Oct 2026 · dialect-generic, configuration-driven, AI-optional

## 1. Purpose and scope

A standalone desktop and command-line tool for **T-SQL, PostgreSQL (incl. PL/pgSQL) and Db2 for i SQL PL**. It tells you exactly what changed between two SQL artifacts and whether the *meaning* changed, with a concrete scenario for every behavioural difference. It is built for code that exists in several dialect variants, often produced by a code generator from templates, and for conversions between dialects.

Three properties define it:
- **A good GUI for human review**, as the primary face of the product.
- **A headless interface (CLI + local MCP server)** for scripts, CI and, optionally, agents.
- **A generic core with all organisation-specific knowledge supplied as configuration** (section 5). The codebase, its tests and its documentation contain nothing specific to any one company or platform.

### Design principles
1. **Never a false "equal".** If equivalence cannot be shown, the verdict is "unknown" or "equal under assumptions".
2. **The tool decides; AI is optional.** Verdicts come from deterministic analysis. The product works completely without AI (section 11).
3. **Behaviour over text.** Transaction, error, cardinality and concurrency semantics are first-class.
4. **Facts come from sources, not from guesses.** A source adapter supplies catalog facts, and everything it cannot prove becomes a visible assumption.
5. **Configuration over code.** Anything that names a company, a platform, a schema, a convention or a credential is data supplied at run time and never compiled in or committed.
6. **Dialects are profiles.** Each dialect is a data-driven profile plus a parser. PostgreSQL, T-SQL and Db2 for i are peers.
7. **Read-only toward sources.** The tool never writes to a source system. Outputs are files and text.
8. **Local only.** Source and findings stay on the machine. Nothing is sent anywhere unless a provider is explicitly configured.

### Use cases
| ID | Use case | Inputs | What the user learns |
|---|---|---|---|
| UC1 | Cross-dialect drift | An object and its counterpart in another dialect | Where they behave differently, and in which scenario |
| UC2 | Version change review | Two versions of one object | Cosmetic vs structural vs semantic vs transactional changes |
| UC3 | Conversion review | An original and a converted version (human or machine) | Every place the conversion changed atomicity, error flow, cardinality or concurrency |
| UC4 | Collection drift report | Two dialect trees, or all pairs in a source | Which paired objects have drifted, ranked by severity |

When the code under review is *generated*, every use case can be run at two levels: the **template or source fragment** and the **composed output**. Both are first-class.

### Non-goals
- Three-way merge; non-SQL content.
- A general plugin system for analysis. Extensibility is through profiles, packs, rules and adapters (section 5).
- Executing against live databases in the product itself. Execution is used only in the validation harness.
- Writing to any source system.

### Future work
- **Other RDBMS types.** The first release covers only the three dialects above. Supporting another database system later would mean a new dialect profile (section 4) with its own parser and semantics catalog entries. The Db2 scope stays Db2 for i only: Db2 for z/OS and Db2 LUW are not candidates.

## 2. Verdicts and change classes

| Verdict | Meaning | Must carry |
|---|---|---|
| Equal | Proven equivalent with no assumptions beyond the dialect specs | Normalizations applied |
| Equal under assumptions | Proven equivalent if every listed assumption holds | Assumption IDs |
| Different | A concrete divergence was found | A witness: scenario, location pair, both outcomes |
| Unknown | Neither equivalence nor a witness could be established | The reason (opaque dynamic SQL, unsupported construct, budget hit, stale input) |

| Class | Examples | Default visibility |
|---|---|---|
| Cosmetic | Whitespace, case, comments, alias names, redundant parentheses | Hidden, count shown |
| Structural | Reordered clauses, moved blocks, equivalent rewrites | Shown, collapsed |
| Semantic | Different results, NULL behaviour, type, collation or conversion behaviour | Shown |
| Cardinality | Different behaviour on zero or several rows | Shown, flagged |
| Transactional | Different commit, rollback, savepoint or atomicity outcome | Shown, highest severity |
| Concurrency | One side is race-free and the other is not | Shown, flagged |
| Binding | A name, column, signature or built-in that does not resolve on one side | Shown, flagged |
| Residue | Syntax or names from another dialect left in a converted artifact | Shown, flagged |
| Limit | A configured engine or platform limit is exceeded or likely to be | Shown, own filter |

A witness is plain language, anchored to both sides, for example: "If the INSERT on line 42 fails, side A rolls back to the transaction start and enters the handler; side B exits with lines 30–41 already durable."

Every finding has a **stable fingerprint** (object identity + class + normalized location pair + witness kind) that survives reformatting. Reviews and suppressions key on it.

## 3. Architecture

```
  [Source adapters]  fs/git · snapshot bundle · private adapters (out of process)
          |
  [0 Acquisition]    snapshot, content-hashed, provenance-tagged
          v
  [1 Parsing]        lossless CST per dialect, nested languages, optional template layer
          v
  [2 Binding]        names -> catalog facts; unresolved -> assumptions
          v
  [3 Semantic IR]    queries, CFG with error edges, effects, transaction state
          v
  [4 Composition]    fragments + wrappers + parameters -> composed output model (when generated)
          v
  [5 Diff engines]   pairing, structural, semantic, behavioural      (per pair)
          v
  findings + witnesses + assumption ledger
        /                        \
  [GUI review]            [CLI / MCP / SARIF]
        \                        /
              review store

  Configuration (section 5) feeds every layer: profiles, packs, project config, user config.
```

Layers 0 to 4 run per object and are cached by content hash. Only layer 5 depends on the pair. Adapters can report what changed, so incremental runs touch only changed objects.

### Layer 0: Acquisition
- Adapters deliver **artifacts** (text and metadata), **catalog facts**, **pairings**, **composition information** and **history** through one documented schema (section 5.4).
- Snapshots are immutable and content-hashed. A finding cites the snapshot it was computed on.
- Line endings, BOMs and trailing whitespace are normalized when hashing, not when displaying.
- Large values are read in verified chunks, and the reassembled text is checked against a length or hash supplied by the adapter.
- An artifact flagged *stale* by its source (generated output older than its inputs) is analysed but verdicts depending on it are capped at "unknown".

### Layer 1: Parsing
One hand-written recursive-descent parser per dialect producing a **lossless concrete syntax tree**: every byte is owned by exactly one token, and print(parse(x)) equals x. Hand-written, because generated parsers lose trivia or recover poorly, and no generic grammar covers all three procedural languages.

Requirements: error recovery with synchronization points, incremental reparse, shared token kinds with dialect-specific extensions, and dialect as a property of the artifact, never guessed per statement. The output is the CST plus a typed AST view over it.

Dialect hard cases:
| Dialect | Case | Handling |
|---|---|---|
| T-SQL | `GO` batch separator, optional terminators, `MERGE` requires one | Lexer-level batch splitter; an explicit "inferred terminator" token. A missing terminator before `THROW` or `ROLLBACK` changes meaning, so it feeds a lint |
| T-SQL | `QUOTED_IDENTIFIER`, bracketed identifiers, `N'...'`, `@@` globals | Lexer; settings read from the header or assumed |
| PostgreSQL | Function bodies in dollar-quoted strings (also `DO` blocks) | **Nested parse** of the body as PL/pgSQL with offsets mapped to the outer artifact; tagged quotes (`$body$`) supported |
| PostgreSQL | `E'…'`, `standard_conforming_strings`, `::` casts, `RETURNING`, `ON CONFLICT`, array and JSON operators | Lexer and expression grammar; settings as assumptions |
| PostgreSQL | Identifier case folding (unquoted → lower) | Binding works on normalized keys |
| Db2 for i | System naming `LIB/FILE` vs SQL naming `SCHEMA.TABLE`; `SET OPTION` clause | Both forms parsed; options become routine attributes |
| Db2 for i | Labels, `LEAVE`, `ITERATE`, condition names, declaration order | Procedural grammar; declaration order checked |
| All | Dynamic SQL in string literals | Best-effort nested parse when the string is constant or a simple template; otherwise an opaque node |

**Optional template layer.** Some artifacts are templates that contain placeholders or directives interleaved with SQL. The template syntax is **defined in configuration** (section 5.3), not in the parser. The template layer parses placeholders per that definition and leaves SQL spans as typed fragments, which the SQL layer parses in a fragment-tolerant mode. Offsets map across template, fragment and composed output. A placeholder that collides with SQL syntax (for example a bracketed identifier) is resolved against the set of known parameters, and an unresolved one is a finding, never a guess.

### Layer 2: Binding and the catalog
Binding resolves every name to what it refers to, using only facts the adapter supplies. Whatever stays unresolved becomes an assumption.
- Local scopes: variables, parameters, cursors, labels, conditions, handler scopes, CTEs, aliases. **Shadowing** and unresolved names are reported.
- Temporary objects with their lifetime (see the catalog in section 7).
- Persistent objects against the catalog: tables, columns, types, nullability, keys and unique indexes, triggers, identity columns and sequences, views, routine signatures, collations.
- **Referenced columns and call signatures are checked** against the catalog, so a column removed on one side and still used on the other is caught before any behavioural analysis.
- A **built-in function and type catalog per dialect and version** (including documented availability and differences). Unknown or unavailable built-ins are findings. Organisation-supplied helper functions are added by packs.
- Identifier normalization per dialect (PostgreSQL lower case, Db2 upper case, SQL Server by collation), with a pairing table that supports folding and user overrides.

### Layer 3: Semantic IR
Dialect-neutral in structure but never erasing dialect behaviour: every place where dialects differ is an explicit attribute, so comparing IRs compares behaviour.
1. **Expression and query terms:** relational algebra with explicit semantic flags (NULL propagation, string comparison and padding, case sensitivity, implicit conversion, result type, NULL ordering, time-source rule).
2. **Control-flow graph with error edges:** one node per statement, a normal edge and one error edge per abstract error kind it can raise. Error points include **statement execution and commit time** (deferred constraints fail at commit).
3. **Effect annotations:** tables read and written, temp objects, rows-affected contract, triggers or substitutes that can fire, and the transaction action.

Abstract error kinds: constraint, conversion, arithmetic, cardinality, not-found, lock (deadlock, timeout, serialization failure), user-signal, name-resolution, other. Each dialect lowering states, per kind, the target (next statement, handler, end of scope, caller) and the transaction effect (statement rollback, rollback to savepoint, full rollback, transaction doomed, none).

**Transaction state** is tracked by abstract interpretation: open depth, savepoint stack, doomed flag, commit mode, the set of effects not yet durable, and when each becomes durable. Transaction actions in the IR: begin, commit, rollback-all, rollback-to, savepoint, atomic-enter and atomic-exit (including implicit subtransactions such as exception blocks), commit-on-return.

**Canonical nodes from idiom lifting:** upsert, insert-if-absent, delete-orphans, paged-read, advisory-lock, plus *out-of-engine substitute* nodes (section 7, triggers).

### Layer 4: Composition (for generated code)
When the code under review is generated from templates, behaviour belongs to the **composed output**, not to a template fragment alone, because wrappers, session options and sibling fragments determine transaction and error semantics.
- The core defines a generic **composition model**: an output made of parts, each with a fragment, its template, its parameters and its span in the output. Adapters populate it from whatever the generator persists, or from provenance markers in the generated text described by a pack.
- **Read mode (authoritative):** split the output into parts using the persisted composition or the markers, giving exact offset mapping between template, fragment and output.
- **Check mode (oracle):** substitute parameters into the template and compare with the generated part. A mismatch is reported as an input inconsistency, never hidden.
- Wrapper facts (commit level, atomicity, error handler, session options) are read from the composed output and become **proven from generated code** in the ledger.
- The generator's own logic is treated as **opaque input**. Staleness is detected through adapter-reported flags and hashes.

### Layer 5: Diff engines
1. **Pairing:** by adapter-supplied pairs or pack rules; inside a routine, statements aligned by sequence alignment over fingerprints.
2. **Structural:** GumTree-style matching with Merkle hashes and move detection.
3. **Semantic:** both sides lowered and normalized (alpha-renaming, commutativity of AND/OR/inner joins, predicate canonicalization, constant folding, removal of redundant casts).
4. **Behavioural:** paired CFGs explored together; for each aligned statement and error kind, compare outcome tuples (control target, transaction effect, durable effects, rows-affected contract). The first divergence becomes a witness.

**Deterministic early checks** (need only binding and catalogs, so they run before any behavioural analysis): referenced columns exist; call signatures match; residue; shadowing and unresolved names; built-in and type availability; declaration order; terminator hazards; untyped values in typed positions; configured limits.

**Soundness rules:** a normalization is applied only if it preserves meaning under every assumption it does not record (reordering inner joins is sound, outer joins is not). Loops are summarized to a fixpoint, and loss of precision degrades to "unknown", never "equal". Every rewrite rule ships with an equal pair and a near-miss pair.

## 4. Dialect profiles

A **dialect profile** is data plus a parser. It contains:
- lexical and grammar settings, nested-language hooks and identifier folding;
- the built-in function and type catalog with versions;
- the implicit-conversion and comparison tables;
- error-kind mapping and the transaction, atomicity, cardinality and temporary-state tables of section 7;
- the vocabulary of session settings and lock intents;
- default lint rules.

The shipped profiles are **T-SQL, PostgreSQL and Db2 for i**. A pairing uses two profiles, and every difference between two profile rows is something the tool must be able to report. The semantic tables are the single source for the lowerings, the tests and the formal model, so adding or correcting a dialect fact means editing data and its test pair.

**What PostgreSQL adds to the generic model** (all verified against vendor documentation before implementation, section 13):
- **Failed statements doom the transaction block:** after an error inside a transaction block, further statements fail until rollback or a savepoint. This differs from engines where only the statement fails.
- **Exception blocks are subtransactions** (implicit savepoints); work inside is rolled back when an error is caught.
- **Functions cannot commit; procedures can**, only outside an explicit transaction block and outside exception blocks.
- **Transaction-scoped time:** `now()` and `current_timestamp` are fixed at transaction start, unlike per-call functions elsewhere. `statement_timestamp()` and `clock_timestamp()` differ again.
- **NULL ordering:** NULLs sort last in ascending order by default, which differs from some other engines.
- **Case:** `LIKE` is case-sensitive, `ILIKE` is not; string comparison follows the collation, trailing spaces are significant for `varchar` and `text`.
- **Advisory locks** (`pg_advisory_xact_lock` and friends) are the natural counterpart of application locks in other engines, which makes lock emulation easier to prove, or to refute.
- **Deferrable constraints** check at commit, so a constraint error can surface at a different point than elsewhere.
- **Functions have declared volatility and strictness**, and `SECURITY DEFINER` with `search_path`. They are behavioural attributes, not hints.
- **Isolation:** `READ COMMITTED` takes a snapshot per statement; `REPEATABLE READ` and `SERIALIZABLE` raise serialization failures that callers must retry.
- **Set-returning functions, `OUT` parameters, `RETURNING`, `ON CONFLICT`, `MERGE` (version-dependent)** lower to canonical nodes.
- **Variable and column name conflicts** in PL/pgSQL are a binding hazard (the setting that decides the winner is read or assumed).

## 5. Configuration architecture

**Rule: nothing organisation-specific is compiled in, committed to the core repository, or embedded in a fixture.** Organisation-specific knowledge is configuration, supplied at run time from files the user controls, and the core runs without it.

### 5.1 Layers of configuration (lowest to highest precedence)
1. **Built-in defaults:** the shipped dialect profiles and generic rules.
2. **Shared packs:** optional public or team packs.
3. **Private packs:** organisation-specific, stored in a private location (for example a private repository) and loaded by path.
4. **Project config:** per project or source.
5. **User overrides.**
6. **Command line.**

Every finding records which packs and config produced it (names and content hashes), shown in the provenance bar and in exports.

### 5.2 What lives in a pack
| Item | Purpose |
|---|---|
| Pairing rules and mapping tables | Prefix, suffix and case conventions, explicit overrides, "shared artifact" markers |
| Template syntax definition | Placeholder delimiters, special forms, parameter sources, collision rules |
| Composition and provenance patterns | How to split composed output and read provenance markers |
| Helper catalog | Organisation-supplied functions and their semantics, for the built-in catalog |
| Author-marker patterns | Comment conventions for review notes, intentional-difference notes, sync partners |
| Change-description patterns | Work-item identifiers and keywords, for grouping history |
| Limits | Engine and platform limits as lint rules (thresholds and patterns) |
| Assumptions | Declared facts such as commit level, naming mode, collation, isolation |
| Severity and class overrides | Local policy |
| Adapter commands and environments | Which adapter to run for which source |
| **Deny-list** | Terms that must never appear in the core repository, fixtures or exports |

Packs are declarative (one documented format with a published schema), validated at load, versioned, and **never written into by the tool**.

### 5.3 Template syntax is configuration
The template layer is driven by a definition such as "placeholders are `<delimiter>NAME<delimiter>`, with these special forms, resolved from these parameter sources". The core knows nothing about any particular generator's syntax. A new generator needs a new definition, not a new parser.

### 5.4 Source adapters: out of process, private by default
Adapters are separate programs that talk to the core over a documented JSON protocol on standard input and output. **Private adapters are never linked into the core and never shipped with it.**

| Method | Returns |
|---|---|
| `describe` | Name, version, capabilities |
| `list_objects(filter)` | Objects with identity, kind, dialect, content hash, last-changed time, stale flag |
| `get_artifact(id)` | Text, metadata, provenance (author kind and time), chunked and hashed |
| `get_pairs(scope)` | Pairings across dialects or versions |
| `get_composition(id)` | Parts of a composed output, or marker rules |
| `get_catalog(scope)` | Tables, columns, types, keys, indexes, triggers, signatures |
| `get_history(id)` | Prior versions and change descriptions |
| `get_settings(scope)` | Declared runtime settings, where the source knows them |

Reference adapters shipped with the core are generic: a **filesystem/git adapter** (pairing by pack rules) and a **snapshot-bundle adapter** (a documented, canonical bundle format, which also serves as the offline mode and as the test fixture format).

### 5.5 Secrets, storage and logs
- **No secrets in packs or config.** Config names a credential reference (an environment variable, the OS keychain or a vault entry), and the adapter process resolves it. The core never sees credentials.
- **Cache, snapshots and the review store** live in a configurable data directory outside any repository, with optional encryption at rest.
- **No source text in logs** unless a debug flag is set. No telemetry, and crash information stays local.
- **Redacted export:** findings can be exported with pack-derived identifiers (object names, schema names, work-item ids) replaced by stable placeholders, so they can be shared outside the environment they came from.

### 5.6 Repository hygiene
- The core repository, its tests and its documentation use **synthetic or public-domain fixtures only**, with a manifest recording each fixture's origin and licence.
- **CI leak scan:** a scan runs the pack-supplied deny-list against the repository, build outputs and fixtures. The deny-list itself is never committed. It is supplied to CI at run time from the private location.
- A pre-commit hook runs the same scan locally.
- Bug classes learned from private corpora enter the public catalog **only as abstract classes and synthetic examples**, never as the original text.

## 6. Interfaces

### 6.1 CLI and local MCP server
The same core is exposed as a CLI and a stdio MCP server. Tools are coarse, deterministic and token-bounded.

| Tool | Returns |
|---|---|
| `queue(source, filter)` | Pairs with verdict and highest class, a few lines each |
| `diff(pair)` | Verdict and a list of **finding cards** |
| `finding(id)` | One finding in full: witness, both excerpts, assumptions |
| `what_if(object, statement, error_kind)` | Both sides' control target, rollback scope, durable effects |
| `assumptions(pair)` / `set_assumption(...)` | Open, declared, discharged; session-local flips |
| `comment_text(finding_ids)` | Plain-text comment from the deterministic phrase grammar |
| `open_in_gui(finding_id)` | Deep link |

A **finding card** is the unit of economy: fingerprint, class, severity, verdict, one-sentence witness, location pairs, assumption IDs and an excerpt capped at a configurable number of lines. Cosmetic and structural noise is counted, not listed. Output formats include JSON and SARIF, so CI can block on new transactional drift.

### 6.2 GUI (a first-class requirement)
The interface answers "what matters here?" before "what changed?". A 500-line reformat shows zero findings, and a one-word conversion bug is the first thing on screen.

| View | Shows | Key interactions |
|---|---|---|
| **Review queue** | Pairs with verdict, highest class, finding count, review state | Filter by class, dialect pair, folder, reviewed/unreviewed; sort by severity |
| **Pair view** | Two panes aligned by **statement, not line**; unchanged subtrees collapsed; idioms as labelled blocks | Jump to next finding; toggle classes; switch lens |
| **Lens switch** | Template or source ↔ composed output, in sync | One keystroke; selection and scroll follow |
| **Transaction overlay** | Gutter brackets for transactions, savepoints, atomic and exception regions on both sides | Click a bracket to highlight its partner |
| **"What if this fails?"** | For a statement and error kind: both sides' control target, rollback scope, durable effects | Pick the error kind; step through the path |
| **Findings panel** | Witness, class, severity, assumptions | Accept / reject / comment; copy as text with both excerpts |
| **Assumption panel** | Open, declared, discharged assumptions with their sources | Flip an assumption and watch verdicts update |
| **Provenance bar** | Source, snapshot time, packs and their hashes, dialect versions | Refresh; compare with another snapshot |

Keyboard-first with a command palette; edits in a scratchpad mode re-diff incrementally and never write to a source; deep links open exactly what an agent or script cited; sessions export as short reports (optionally redacted).

**Technology:** a Rust core (lossless trees in the rowan style) with a web-technology UI shell and a mature code-view component for per-token styling, synchronised scrolling and large files. A time-boxed spike decides.

### 6.3 Review store
Accepted, rejected and commented decisions, keyed by finding fingerprint and snapshot, with optional expiry. Deliberate, accepted divergences are recorded here (and optionally as an in-source marker defined by a pack). GUI and CLI share it.

## 7. Dialect semantics catalog

The catalog specifies what the lowerings implement. **(verify)** means "check against vendor documentation before implementing"; the list is in section 13.

**Error handling**
| | T-SQL | PostgreSQL | Db2 for i |
|---|---|---|---|
| After a runtime error | Often statement-only; some errors abort the batch | Whole transaction block doomed until rollback or savepoint, unless an exception block catches it | Unhandled exits the routine; warnings and not-found continue |
| Session switch | `XACT_ABORT` | none | none |
| Structured handling | TRY/CATCH (severity 11–19) | `BEGIN … EXCEPTION WHEN` (subtransaction) | CONTINUE / EXIT / UNDO handlers (UNDO only in ATOMIC) |
| Status checks | `@@ERROR`, RETURN codes, `XACT_STATE()` | `FOUND`, `GET [STACKED] DIAGNOSTICS` | SQLSTATE / SQLCODE, `GET DIAGNOSTICS` |
| Raising | THROW (honours XACT_ABORT), RAISERROR | `RAISE EXCEPTION`; notices do not affect flow | SIGNAL / RESIGNAL |
| Unterminated statement | `THROW` or `ROLLBACK` can be read as part of the previous statement | n/a | n/a |

**Transactions**
| | T-SQL | PostgreSQL | Db2 for i |
|---|---|---|---|
| Default mode | Autocommit / IMPLICIT_TRANSACTIONS | Function: caller's transaction; procedure may commit under conditions | Commitment control level; `*NONE` means no transaction |
| Nesting | `@@TRANCOUNT`; bare ROLLBACK ends the outermost | none | none; COMMIT/ROLLBACK end the unit of work |
| Savepoints | SAVE TRAN | SAVEPOINT, implicit in exception blocks | SAVEPOINT (needs commitment control); ATOMIC acts as an implicit savepoint (verify under `*NONE`) |
| Commit scope | session | session | commitment definition (verify defaults) |
| Prerequisite | none | none | tables journaled |
| Survives rollback | table variables, identity values | sequence values | identity/sequence; everything under `*NONE` |
| DDL | transactional in most cases | transactional | verify |

**Statement atomicity:** single statements are atomic in T-SQL and PostgreSQL. In Db2 for i they are atomic only under commitment control. Routine bodies are not atomic in T-SQL, are part of the caller's transaction in PostgreSQL, and are not atomic in Db2 for i unless ATOMIC or commitment control applies.

**Cardinality contracts:** `SELECT @v = col` keeps a silent last row; scalar subqueries error on several rows; PL/pgSQL `SELECT INTO` keeps the first row silently unless `STRICT`; Db2 `SELECT INTO` raises not-found (a warning) and errors on several rows; `UPDATE … FROM` with a join uses one unpredictable source row in T-SQL and PostgreSQL; a correlated Db2 `UPDATE` sets NULL unless restricted with `EXISTS`; `MERGE` with several matching source rows errors; `TOP`/`LIMIT`/`FETCH FIRST` without `ORDER BY` is nondeterministic. `@@ROWCOUNT` is reset by several statements (verify the list); PostgreSQL and Db2 use `FOUND` and `GET DIAGNOSTICS`.

**Upserts and MERGE:** all patterns lift to one canonical upsert node (target, match key, on-match, on-absent, several-source-rows behaviour, concurrency, atomicity). Patterns: T-SQL `MERGE` (races without hints); `IF EXISTS … UPDATE ELSE INSERT`; `UPDATE` then check the row count then `INSERT`; PostgreSQL `INSERT … ON CONFLICT` (safe, needs a unique index) and `MERGE` (15+, can raise unique violations under concurrency; more clauses from 17); Db2 `MERGE` (no `NOT MATCHED BY SOURCE`, partial application with NOT ATOMIC, verify).

**Temporary state:** T-SQL `#temp` (transactional) and table variables (survive rollback); Db2 declared global temporary tables (job lifetime, default delete rows on commit, logging options, verify); PostgreSQL temporary tables (session lifetime, `ON COMMIT` options, a second `CREATE` fails without `IF NOT EXISTS`).

**NULL, strings, types:**
- Concatenation with NULL: `+` yields NULL in T-SQL, `||` yields NULL in PostgreSQL and Db2, `CONCAT` / `concat()` treat NULL as empty in some dialects.
- Trailing spaces in comparison; case sensitivity of comparison and `LIKE`; `CAST('' AS int)`; `ISNULL` vs `COALESCE` typing; date difference semantics (`DATEDIFF` boundaries vs exact subtraction vs estimates).
- **NULL ordering** and the **time source** (statement vs transaction vs call).
- Implicit conversion permissiveness, untyped parameters getting an inferred length, domain or user-defined types that one dialect lacks.
- Built-in availability and differences per dialect and version (for example string aggregation, JSON shredding, UUID generation).

**Session settings** are read from source or composed output, otherwise assumed. T-SQL: `XACT_ABORT`, `IMPLICIT_TRANSACTIONS`, ANSI options, `NOCOUNT`, snapshot isolation. PostgreSQL: `search_path`, `SECURITY DEFINER`, volatility, strictness, default isolation, `variable_conflict`. Db2 for i: `SET OPTION` (commit, naming, sort sequence, user profile), library list.

**Dynamic SQL:** constant or simple-template strings are parsed; anything else is opaque, so affected paths can at best be "equal under assumptions".

**Triggers, handlers and substitutes:** `SCOPE_IDENTITY` / `@@IDENTITY` / `IDENTITY_VAL_LOCAL()` / `RETURNING` / `FINAL TABLE`; trigger rollback behaviour; statement vs row granularity. Where a platform replaces an engine trigger with an **application-level or out-of-engine handler**, the IR has a separate node, because atomicity with the triggering statement, ordering and error propagation may all differ. How such a handler is invoked is a source-specific fact supplied by the adapter or a pack.

**Concurrency and isolation:** lock intent per read (none, read-committed, update-lock, range, skip-locked, advisory). Check-then-write sequences with differing intents are flagged. Lock emulation is analysed explicitly, including what happens when the locked row does not exist. Isolation defaults and snapshot behaviour per dialect are assumptions unless declared.

**Platform and engine limits** (large-object columns in triggers, query size and complexity, row size, missing user-defined types, declaration order in compound statements) are **configurable lint rules** in packs. They are reported as class *Limit* so they never mask a behavioural finding.

**Worked example.** A conversion that looks line-for-line faithful and is behaviourally different in three ways:

T-SQL original:
```sql
CREATE PROCEDURE dbo.MoveStock @Item int, @Qty int AS
BEGIN
  SET NOCOUNT ON;
  BEGIN TRAN;
  UPDATE Stock SET Qty = Qty - @Qty WHERE ItemId = @Item;
  IF @@ERROR <> 0 BEGIN ROLLBACK; RETURN 1; END
  INSERT INTO StockLog (ItemId, Delta) VALUES (@Item, -@Qty);
  IF @@ERROR <> 0 BEGIN ROLLBACK; RETURN 2; END
  COMMIT;
  RETURN 0;
END
```
Db2 for i conversion:
```sql
CREATE PROCEDURE MoveStock (IN p_Item INT, IN p_Qty INT)
LANGUAGE SQL
SET OPTION COMMIT = *NONE
BEGIN
  UPDATE Stock SET Qty = Qty - p_Qty WHERE ItemId = p_Item;
  INSERT INTO StockLog (ItemId, Delta) VALUES (p_Item, -p_Qty);
END
```
| # | Class | Witness | Assumptions |
|---|---|---|---|
| 1 | Transactional | If the INSERT fails, T-SQL rolls back the UPDATE and returns 2; Db2 exits with the UPDATE already durable | Caller's `XACT_ABORT` off; no triggers on `StockLog` |
| 2 | Transactional | If the caller already holds an open transaction, T-SQL's COMMIT only decrements the count; Db2 under `*NONE` has no transaction to join | Caller transaction state unknown |
| 3 | Semantic (error contract) | T-SQL returns 1 or 2 and raises nothing to a caller that checks only return codes; Db2 propagates the SQLSTATE | Callers inspect the return code |

With `XACT_ABORT ON` in the caller, witness 1 changes on the T-SQL side (the batch aborts and rolls back; the return code is never set). The tool shows both variants and lets the ledger decide which applies.

## 8. Idiom lifting and rules

Rules are declarative S-expressions that match dialect AST patterns and either lift them to canonical IR nodes or declare a cross-dialect equivalence with side conditions. Rules are data, versioned, and organisation-specific rules live in private packs.

- **Lift:** a pattern becomes a canonical node (upsert, insert-if-absent, delete-orphans, paged-read, advisory-lock).
- **Equivalence:** a pattern in dialect A is equivalent to a pattern in dialect B when side conditions hold; a failed side condition produces a finding, not silence.
- **Lint:** a pattern is reported on its own, even in a single-dialect diff.

```
(lift upsert/if-exists
  (tsql (if (exists (select _ (from ?t) (where ?key-pred)))
            (update ?t ?sets (where ?key-pred))
            (insert ?t ?cols ?vals)))
  => (upsert (target ?t) (match ?key-pred) (on-match ?sets)
             (on-absent (?cols ?vals))
             (concurrency race) (atomicity two-statements)))

(equiv case-insensitive-compare
  (tsql (= ?a ?b))
  (pg   (= (call lower ?a) (call lower ?b)))
  (when (assumed (collation tsql case-insensitive))))
```

Matching: metavariables (`?x`), wildcards (`_`), sequence patterns, alpha-equivalence on bound names, and side conditions that query types, keys and nullability after binding (falling back to the ledger). Near misses ("almost an upsert: the UPDATE and the EXISTS use different predicates") are ranked by tree edit distance. The shipped rule set is confluent, and a test fails when two rules overlap without priority.

## 9. Assumption ledger

| Source | Example | Shown as |
|---|---|---|
| Proven from the object's own source | `SET XACT_ABORT ON` in the routine | Not an assumption |
| Proven from composed output | The wrapper sets the commit level | Discharged, cites the output |
| Proven from the catalog | A unique index on a column | Discharged, cites the source |
| **Author-declared in source** | A review marker comment stating an unverified premise | Declared by author, open until resolved; parsed by pattern, no AI |
| Declared in a pack or project config | "All routines run under commit level X" | Declared, with config line |
| Default | "Caller's `XACT_ABORT` is off" | Open, highlighted |

Behaviour: a verdict lists its assumptions; flipping one re-runs the analysis; assumptions that change a verdict rank above those that don't; a contradiction between sources is itself a finding. When a generated wrapper does not state a fact the converted code depends on (for example commit level), that single assumption is the highest-value item to declare once in a pack.

## 10. Performance targets (to be set in the spike)
- Open a pair of 5,000-line routines: under one second.
- Re-diff after an edit in the scratchpad: no visible delay.
- Collection drift report: bounded by parse time, cached per content hash, incremental after the first run.
- `queue` and `diff` on cached objects: milliseconds.
Mechanisms: incremental reparse, Merkle hashes, interned identifiers, arena-allocated trees, per-object caches.

## 11. AI-optional operation

The whole product is fully usable with no AI, no network model access and no AI dependency installed.

| Tier | What runs | AI needed |
|---|---|---|
| **0: Deterministic core (the product)** | Acquisition, parsing, binding, composition, diff engines, verdicts, witnesses, ledger, review store, GUI, CLI, SARIF/JSON | None |
| **1: Consumers (optional)** | Scripts, CI or agents calling the CLI or MCP interface | The consumer, not the tool |
| **2: AI assist inside the tool (optional)** | Explanation, fix suggestions, comment polishing, natural-language search | A configurable provider (local or approved endpoint) |

**What replaces AI at Tier 0:** witness and comment text from a deterministic phrase grammar over the witness structure; prioritisation from class, severity, whether an assumption flips the verdict, near-miss distance, authorship and recency signals (supplied by adapters), size and review state; author notes parsed by pattern; structured search.

**What Tier 2 may do:** explain findings, propose fixes as text diffs, polish drafted comments, answer questions over findings. It receives finding cards only, bounded by the excerpt cap. Everything it produces is labelled AI-generated, stored separately with provenance, and **never changes a verdict, class, severity or assumption**. It is off by default, enabled per project and provider, and visible in the GUI when on.

**Enforcement:** Tier 2 lives in its own module behind a feature flag with no dependency from the core. CI runs the full golden set and soundness suite with the module absent and network access blocked; findings must be identical to a run with it present. No review action may be reachable only through AI.

## 12. Validation and verification

The property that must never fail is **soundness**: the tool never says "equal" for a pair that an execution oracle shows behaving differently.

| Layer | Check | Oracle |
|---|---|---|
| Parser | Round trip on the whole corpus, including nested languages and template layer | The input |
| Parser | Grammar-aware fuzzing; error nodes bounded | Invariants |
| Composition | Parameter substitution reproduces the generated part | The generated output |
| Binding | Every reference resolves or becomes an assumption | Invariants |
| Normalization | Each rule has an equal pair and a near-miss pair | Golden tests |
| Semantic / behavioural | Metamorphic tests; mutations become "different" | Generated pairs |
| Behavioural | Fault injection: force each statement to fail in turn and compare durable state | **SQL Server and PostgreSQL in containers**; Db2 for i on a real partition |
| Error/transaction model | Specified in TLA+ and checked against the profile tables | Model checker |
| Configuration | The core builds, runs and passes the public suite with **no pack loaded** | CI |
| Leak scan | The runtime deny-list finds nothing in the repository, fixtures or exports | CI |
| AI separation | Byte-identical findings with the AI module absent and network blocked | CI |

**Oracle availability drives the order of validation.** SQL Server and PostgreSQL run in containers, so the T-SQL ↔ PostgreSQL pair is the fully testable one and is used to prove the behavioural machinery first. Db2 for i cannot run in a container, so until a partition is available, Db2 verdicts that depend on untested lowering rules are capped at "equal under assumptions". Synthetic cases can run on a public partition; private code runs only in a private environment.

**Public and synthetic corpora.** PostgreSQL's own regression tests, open-source schemas and procedures, and purpose-written synthetic pairs form the public corpus. Private corpora are studied through adapters, and only abstract bug classes and synthetic examples come back into the public catalog.

**Mutation catalog** (each is a plausible conversion mistake and must be detected):
- remove `WHERE EXISTS` from a correlated UPDATE;
- move a COMMIT before the last DML;
- drop a locking hint from a MERGE; replace advisory-lock emulation with a lock on a row that can be missing;
- table variable → transactional temporary table;
- remove CONTINUE handlers, or EXIT → CONTINUE; drop an `ATOMIC` compound;
- change the commit level to `*NONE`;
- `SET @v = (subquery)` → `SELECT @v = …`; `SELECT INTO` without `STRICT`;
- split a MERGE with `NOT MATCHED BY SOURCE` into MERGE + DELETE;
- `COALESCE` → `ISNULL` with differing lengths;
- remove the terminator before `THROW`;
- reference a column that no longer exists; call a routine with an outdated signature; shadow a variable;
- leave a schema qualifier or syntax from another dialect behind;
- drop a case-insensitivity emulation (`upper` / `lower` / `ILIKE`) from a comparison;
- replace an engine trigger with an out-of-engine handler;
- change `now()` to `clock_timestamp()` (or the reverse); change NULL ordering;
- drop a composed fragment from a generated output.

## 13. Facts to verify

Each claim needs a vendor source link before its lowering rule is implemented.

| ID | Claim |
|---|---|
| V1 | Db2 for i `MERGE` has no `WHEN NOT MATCHED BY SOURCE` |
| V2 | Db2 for i `MERGE` supports `NOT ATOMIC CONTINUE ON SQLEXCEPTION`; what is kept on failure |
| V3 | Db2 for i `MERGE` with several source rows on one target |
| V4 | ATOMIC compound and UNDO handler behaviour under `*NONE` |
| V5 | Default commit level for SQL routines per creation interface |
| V6 | Default activation group of SQL procedures and the scope of the commitment definition |
| V7 | Declared global temporary tables: default ON COMMIT behaviour and the effect of NOT LOGGED |
| V8 | Blank-padding of VARCHAR comparisons on Db2 for i |
| V9 | `CAST('' AS INT)` and string-to-number conversion on Db2 for i |
| V10 | Exact list of T-SQL errors that abort the batch when `XACT_ABORT` is off |
| V11 | Statements that reset `@@ROWCOUNT` |
| V12 | PostgreSQL versions: `MERGE` (15+), `NOT MATCHED BY SOURCE` and `merge_action()` (17+) |
| V13 | Semantics of UUID generation and UTC conversion helpers per dialect |
| V14 | `SAVEPOINT` and `ROLLBACK TO SAVEPOINT` inside Db2 compound statements, and with which commit levels |
| V15 | `SELECT … WITH RS USE AND KEEP EXCLUSIVE LOCKS` on `SELECT INTO`: accepted, and effective under which commit levels |
| V16 | A ROLLBACK's effect on a NOT LOGGED declared temporary table, compared with T-SQL table variables |
| V17 | PostgreSQL: exact rules for `COMMIT` in procedures (explicit blocks, exception blocks, called from functions) |
| V18 | PostgreSQL: which errors an `EXCEPTION WHEN OTHERS` handler does not catch by default |
| V19 | PostgreSQL: default NULL ordering and how it compares with T-SQL and Db2 for i |
| V20 | PostgreSQL: `now()`, `statement_timestamp()` and `clock_timestamp()` against `SYSDATETIME()` and `CURRENT TIMESTAMP` |
| V21 | PostgreSQL advisory locks (transaction vs session scope, key space) against T-SQL application locks |
| V22 | PostgreSQL deferrable constraints: when each error kind surfaces |
| V23 | PostgreSQL `plpgsql.variable_conflict` default and its effect on name binding |
| V24 | PostgreSQL volatility and strictness: which semantic differences they imply when converted |

## 14. Milestones

Priorities follow where conversion bugs historically occur (T-SQL → Db2 for i first), while validation follows oracle availability (T-SQL ↔ PostgreSQL first). Each gate must pass before the next starts.

| # | Milestone | Gate |
|---|---|---|
| M0 | **Corpus study and facts**, run privately through adapters: classify historical conversion bugs against the catalog, draft packs, select golden pairs, answer the fact questions that apply | At least 10 historical bugs classified; **only abstract classes and synthetic examples leave the private environment** |
| M1 | **Spike:** lossless parse of one nasty real routine, two-pane view with per-token styling and synchronised scroll, re-render on edit | Stack chosen and written down |
| M2 | **Acquisition, configuration and lossless parsers** for T-SQL, PostgreSQL (with the nested PL/pgSQL parse) and Db2 for i; filesystem/git and snapshot adapters; pack loading; leak scan in CI | Round trip passes on 100% of the public corpus; error nodes < 1% of statements; core passes with no pack |
| M3 | **Pair view, structural diff, deterministic early checks, CLI/MCP** | Daily use on real pairs beats the current diff tool; the early checks flag the corresponding mutations |
| M4 | **IR and the error/transaction model** from the profile tables | Formal model checked; every row of the error and transaction tables has a test |
| M5a | **Behavioural diff, witnesses, ledger, "what if", transaction overlay** proven on **T-SQL ↔ PostgreSQL** with the container oracle | Worked example and the mutation catalog yield correct witnesses and verdicts |
| M5b | Same for **Db2 for i**, with verdicts capped where untested | Same gates; capped verdicts documented |
| M6 | **Idiom lifting, upserts, cardinality, temporary state, advisory-lock emulation** | Mutation catalog detected in full |
| M7 | **Collection drift dashboard and review store** | Full-collection report within the performance target; accepted findings suppressed correctly |
| M8 | **Optional Tier 2 AI assist** | Separation tests pass; findings byte-identical with the module absent |

## 15. Open decisions
1. **Credential model for private adapters:** the adapter owns it (recommended); the core never sees credentials.
2. **Headless output in CI:** SARIF/JSON from M5a. Recommendation: yes.
3. **Review store location:** local only vs a shared file.
4. **Excerpt policy:** the default cap on lines per finding card sent to any model (only relevant when agents or Tier 2 are used).
5. **GUI stack:** confirmed by the spike.
6. **Tier 2 scope:** whether in-tool AI ships in the first release. Recommendation: no; Tier 0 and Tier 1 first.
7. **Pack and adapter schema stability:** how versioned and how strictly validated, since private packs will outlive releases.
8. **Cross-source sync markers:** whether to compare artifacts that declare a sync partner in another source, or only parse the markers.
9. **Deliberate differences:** review store only, or also an in-source marker defined by a pack.

## 16. Appendix: bug classes seen in conversions

Abstract classes only, each with a deterministic or behavioural detection route:
- column removed or renamed on one side and still referenced on the other (binding);
- routine called with an outdated signature (binding);
- variable shadowing a wider-scope variable; reference to a variable that does not exist (binding);
- schema qualifiers and syntax from the source dialect left in the target (residue);
- built-ins or types not available in the target (binding, built-in catalog);
- untyped value in a typed position receiving an inferred length (types);
- missing terminator turning a statement into part of the previous one (parse hazard);
- rollback scope narrower or broader than the original, including caller-owned transactions (transactional);
- commit level or atomicity not declared by the generated wrapper, while the code depends on it (assumption);
- lock emulation that fails or no-ops when the locked row is missing (concurrency);
- case-insensitive comparison lost or emulated on one side only (semantic);
- trigger replaced by an out-of-engine handler, with different atomicity and ordering (transactional);
- statement trigger vs row trigger; time difference functions (semantic);
- table variable replaced by a logged or session-persistent temporary table (temporary state);
- `SELECT … INTO` row-count contract changed (cardinality);
- engine limits (large-object columns in triggers, query complexity, row size) (limit).
