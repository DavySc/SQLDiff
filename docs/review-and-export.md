# Review store, suppression, redacted export and cache

Design sections 5.5, 6.3, 9 and 10. Library: `crates/sqldiff-review`.
GUI, CLI and MCP share it.

## Review store

A decision is keyed by **finding fingerprint** and **snapshot**:

- the fingerprint is stable across reformatting (object, class, rule and
  normalized anchors);
- the snapshot is `<content_hash_a>:<content_hash_b>` of the pair (just the
  hash of A for a check), or `*` for every snapshot.

A decision has a `state` (`accepted`, `rejected`, `commented`), an optional
comment, author and expiry, and the time it was made. Lookup tries the exact
snapshot, then `*`. A fingerprint decided only for another snapshot reports
as `outdated`, so a reviewer can see the source changed since.

The store is an append-only log at `<data_dir>/review/decisions.jsonl`
(the last record per key wins). It lives in the data directory, outside any
repository. Appends and compaction take a lock file next to the log, and
compaction re-reads the log first, so it never drops a decision another
process wrote. A line that does not decode (for example one torn by a
crash) is skipped with a warning; if no line decodes, opening fails (wrong
or missing data key). `sqldiff review compact` rewrites it with one line per decision
and drops expired ones.

```sh
sqldiff review accept aacde30caae50448 --pair a.sql b.sql -c "callers map codes" --expires 2027-01-01
sqldiff review accept aacde30caae50448             # every snapshot
sqldiff review reject FP --snapshot HASH_A:HASH_B
sqldiff review comment FP -c "check with the owner"
sqldiff review list [--state accepted] [-f json]
sqldiff review clear FP [--snapshot S | --pair A B]
```

The author defaults to `$SQLDIFF_AUTHOR`, then `$USER`.

## Suppression

`diff`, `check` and `tree` suppress findings accepted for the report's
snapshot (or for `*`) that have not expired. Suppressed findings move to the
report's `suppressed` list with their justification; they are not dropped:

- text output counts them and lists them under "Suppressed";
- JSON has `suppressed: [{finding, kind, justification}]`;
- SARIF keeps them as results with `suppressions` (`external` for review
  decisions, `inSource` for markers).

Once anything is suppressed, the report's `verdict` becomes the **open
verdict**, the verdict over the findings still open, and the engine's
verdict is kept in `verdict_before_suppression` (text shows
`Verdict: UNKNOWN (different before 1 suppressed)`). Text, JSON, SARIF and
the exit code all agree. The open verdict is never better than "equal under
assumptions": an accepted difference is still a difference.
`--no-suppress` shows everything.

## Author markers

Pack `author_markers` are regexes over the source; the first capture group
is the marker's value. No AI is involved.

| `kind` | Effect |
|---|---|
| `intentional_difference` | Deliberate difference: findings on that side that start on the marker's line (a trailing comment) or the next line (a comment above the statement) are suppressed (`in_source`). A finding that merely spans the marker, such as a whole-body finding, is not |
| `assumption` | Author-declared premise: becomes an assumption with source `author`, id `ASM-AUTHOR-XXXXXXXX` (hash of the text, case and whitespace folded), cited as `A:12`; open until resolved |
| `review_note`, `sync_partner` | Parsed and shown, no effect on the verdict |

```toml
[[author_markers]]
kind = "intentional_difference"
pattern = "--\\s*DELIBERATE:\\s*(.*)"

[[author_markers]]
kind = "assumption"
pattern = "--\\s*ASSUMES:\\s*(.*)"
```

## Redacted export

`--redact` (on `diff`, `check` and `tree`, every format) replaces
identifying terms with stable placeholders:

| Placeholder | Terms |
|---|---|
| `OBJ_…` | Object names and file stems |
| `SCHEMA_…` | Schema (and database) parts of qualified names |
| `PATH_….sql` | Paths of artifacts, directories and packs |
| `WI_…` | Work items found by pack `change_patterns` in any text |
| `HELPER_…`, `RULE_…`, `PACK_…` | Pack helper names, limit rule ids, pack names |
| `TERM_…` | Pack `deny_list` terms, matched anywhere, also inside words |

Identifiers are matched case-insensitively as whole words; when the longest
matching term fails its word boundary, shorter terms at the same place are
tried. Only free-text values change: JSON keys, enum values, versions and
hashes (fingerprints, content hashes, snapshots) are kept, so a file named
`rule.sql` or `error.sql` cannot break the output, and no term matches
inside a placeholder or a hex run of 16 or more characters. A placeholder is
a keyed hash of the term with a salt kept in `<data_dir>/redact.salt`: the
same environment always produces the same placeholder, so exports can be
compared, and nobody without the salt can confirm a guessed name.
Fingerprints, verdicts, classes and line numbers are kept.

`--drop-excerpts` removes source excerpts instead of redacting them; use it
when excerpts may hold sensitive text that no pack names. Before printing,
the redacted data (every free-text value, including tree listings and
provenance) is checked again for every registered term and refused if one
is left. The deny-list scan (`scripts/leak-scan.sh --file`) passes on
redacted output; a test asserts this.

Library use (GUI session reports, MCP): `Redactor::from_config(&cfg)`, then
`redact_report`, or `add_*` and `redact_value` for other JSON, and
`verify_report` / `verify_value` before writing.

## Encryption at rest

With `data_key = "env:NAME"` or `"file:PATH"` in config, every store record
and cache entry is sealed with XChaCha20-Poly1305 under a key derived from
that secret. The config holds only the reference. Use a random secret of at
least 16 characters (for example `openssl rand -hex 32`). Plain records from
before encryption was turned on are still read.

## Cache

`diff`, `check` and `tree` cache engine reports under `<data_dir>/cache`,
keyed by a hash of each side's exact text, dialect, version and path, the
engine options, every config layer's hash and the running build. A changed
input simply misses. `--no-cache` bypasses it; `sqldiff review clear-cache`
deletes it.

## Change groups

`sqldiff_review::history::group` groups `get_history` entries by work item
(from the entry, else from pack `change_patterns`), for the GUI and MCP.
