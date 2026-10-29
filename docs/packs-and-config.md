# Packs and configuration

Design section 5. Nothing organisation-specific is compiled in or committed:
it lives in packs and config files the user controls, and the core runs with
none loaded.

## Layers

Lowest to highest precedence; every finding's provenance lists the layers
that were loaded, with content hashes (`sqldiff config` shows them).

| Layer | Where |
|---|---|
| Built-in | Shipped dialect profiles and rules |
| Shared packs | `packs = [...]` in project or user config |
| Private packs | `private_packs = [...]` in config, then `--pack PATH` |
| Project config | `--config FILE`, else `sqldiff.toml` in the working directory or a parent |
| User config | `--user-config FILE`, else `$SQLDIFF_CONFIG`, else `config/config.toml` beside the executable (a colleague bundle written by `sqldiff dist`), else `~/.config/sqldiff/config.toml` (`%APPDATA%\sqldiff\config.toml`) |
| Command line | `--assume ID=TEXT`, `--dialect-version DIALECT=VERSION` |

`--isolated` skips project and user files not named on the command line.
Relative paths in a config file are relative to that file.

The GUI says on every page that uses packs (Compare, Convert in template
mode, Review, each run on the Runs page, and Home for what a comparison
opened there loads) which packs were loaded: a shared pack by name, version
and path, a private pack (`private_packs`, and every `--pack` or pack field)
by path only. "No pack loaded" means built-in rules and config files only.
Comparisons opened from Home load the packs `sqldiff-gui` was started with.

Merging: ordered rule lists (`pairing.rules`, `dialect_map`, `overrides`) are
tried higher layer first; maps (`assumptions`, `dialect_versions`) and keyed
lists (`limits` by id, `adapters` by name, `helpers` by dialect and name) are
replaced by key; everything else is appended.

## Pack format

A pack is a `pack.toml` (or a directory containing one) with a `[pack]`
header and any of the sections below. Unknown keys, invalid regexes, globs,
dialects and credential references are errors at load. Packs are never
written by the tool. [`examples/pack/pack.toml`](examples/pack/pack.toml)
uses every section.

```toml
[pack]
name = "team-conventions"   # unique among loaded packs
version = "1.4.0"
schema = 1                  # packs newer than the build are rejected
```

| Section | Purpose |
|---|---|
| `[[pairing.rules]]` | `name`, optional `dialect` and `path` glob; `strip_prefix`, `strip_suffix`, `case` (`lower`, `upper`, `preserve`), `ignore_chars` (default `_-`) |
| `[[pairing.pairs]]` | Explicit pairs by object id (`a`, `b`) |
| `pairing.shared` | Globs of artifacts shared by all dialects, never paired |
| `[[dialect_map]]` | `glob`, `dialect`, optional `version`; first match wins |
| `[dialect_versions]` | Target version per dialect |
| `ignore` | Globs a tree source skips |
| `[template]` | Placeholder delimiters, name pattern, special forms, the `include` form, value kinds (`values`), fragment kinds (`fragments`), routine halves (`pairs`), parameter sources, collision rule, generator-supplied `known_parameters` ([templates-and-composition.md](templates-and-composition.md), [convert-mode.md](convert-mode.md#fragments-routine-halves-and-value-kinds)) |
| `[observer]` | Side channels the caller reads (decision 1 in [verification-roadmap.md](verification-roadmap.md)). `row_counts`: `"same-dialect"` (default; rows-affected counts are observed within one dialect, and across dialects a side that sends them while the other does not makes the verdict depend on `ASM-OBSERVER-ROWCOUNT`), `"always"` (also across dialects: such a pair is unknown) or `"never"` (within one dialect a changed `SET NOCOUNT` is set aside under `ASM-OBSERVER-ROWCOUNT`). The higher layer's section replaces a lower one's |
| `[deployment]` | The deployment profile (decision 14 in [verified-conversion.md](verified-conversion.md)): `accept`, the assumption ids this deployment accepts, and `[deployment.facts]`, catalog facts its owner asserts. Reported, never a change of verdict; see [Deployment profile](#deployment-profile) |
| `[[composition]]` | Begin and end line markers of composed parts; named groups `part`, `template`, others become parameters |
| `[[helpers]]` | Organisation functions for the built-in catalog |
| `[[author_markers]]` | `kind` (`review_note`, `intentional_difference`, `sync_partner`, `assumption`) and a `pattern` whose first group is the value; see [review-and-export.md](review-and-export.md) |
| `[[change_patterns]]` | Work-item ids in change descriptions (first capture group) |
| `[[limits]]` | Lint rules of class `limit`: `pattern`, `max_bytes` or `max_lines`, optional `dialects`, `severity` |
| `[assumptions]` | Declared facts by assumption id; matching report assumptions become *declared* |
| `[[overrides]]` | `rule` glob over finding rules, new `class` and/or `severity`; first match wins |
| `[[adapters]]` | Out-of-process adapters: `name`, `command`, `env`, `credential` |
| `[format]` | Formatter layout for `sqldiff fmt`, with `[format.tsql]`, `[format.postgres]`, `[format.db2i]` overrides; merged key by key ([formatter.md](formatter.md)) |
| `deny_list` | Terms the leak scan rejects (top-level key, before `[pack]`) |

Globs use `/`, `*` within a segment, `**` across segments, `?`; prefix
`(?i)` for case-insensitive matching.

Without a `dialect_map` match, a tree source takes the dialect from a
directory or file-name part that names one (`tsql/x.sql`, `x.pg.sql`,
`pairs/x/db2i.sql`); otherwise the dialect is unknown and the object is
listed but never paired or diffed. Dialects are never guessed from content.

## Deployment profile

The assumptions a converted tree still carries are reviewed once per
deployment, not once per routine ([verified-conversion.md](verified-conversion.md),
decision 14). `[deployment]` records that review:

```toml
[deployment]
accept = ["ASM-OBSERVER-ROWCOUNT", "ASM-DBO-SCHEMA", "ASM-BIND-COLUMN"]

[deployment.facts]
collation = "SQL_Latin1_General_CP1_CI_AS"
```

- `accept` names ids from the assumption registry
  ([assumptions.md](assumptions.md)). A registered id also covers its
  instances (`ASM-BIND-COLUMN` covers `ASM-BIND-COLUMN:qty`); an instance or
  a family member (`ASM-IDENTITY-ORDERS`) covers only itself. An id that is
  not registered, a whole family (`ASM-FACT-*`) and an id whose discharge is
  `nothing` (a vendor fact still to verify, a per-routine review, a
  difference's witness) are errors at load.
- `facts` takes the fact keys the registry lists (`collation`,
  `default_schema`, `time_zone`, `db2i_commit_level`, ...); other keys are
  errors. A fact is reported as the user's own declaration with the
  assumptions it bears on; it changes no verdict yet. (The Db2 for i commit
  level the analysis does read is still declared as
  `[assumptions] ASM-DB2-COMMIT-LEVEL = "*CS"`.)
- Merging: `accept` lists of all layers are joined, and each accepted id
  remembers every layer that accepts it; a fact key in a higher layer
  replaces a lower one's. The observer switches stay in `[observer]`.

Acceptance never changes a verdict or a status: an accepted assumption still
makes a result "equal under assumptions" and a converted unit "under
assumptions". What changes is the reporting:

- In a diff report each accepted assumption has `accepted_by`: the layer
  (`shared_pack`, `private_pack`, `project`, `user`), the pack's name and
  the file. A private pack is named by its file only, never by its name. The text output adds "accepted by the deployment profile: ...".
- In a converted tree (`conversion-report.json` and `.md`) each unit lists
  its accepted assumptions in `accepted`, and a `deployment` section lists
  the profile (accepted ids with their sources, the facts) and every
  assumption the tree carries (unit notes and the engine's assumptions on
  each file) with whether it is accepted.
  `assumptions_within_accept` is true when all of them are. That is one of
  the conditions for shipping a tree without re-review, not all of them:
  units that differ, are not converted or are not proven are not
  assumptions, and the proof and catalog conditions are separate.

## Config files

Config files hold the same sections (without `[pack]`) plus:

```toml
packs = ["../shared-pack"]
private_packs = ["/secure/org-pack"]
data_dir = "/var/lib/sqldiff"      # cache, snapshots, review store; default $SQLDIFF_DATA_DIR or ~/.local/share/sqldiff
excerpt_lines = 3
data_key = "env:SQLDIFF_DATA_KEY"  # optional: encrypt the review store and cache at rest (env:NAME or file:PATH)

[source]                            # default SOURCE for commands
kind = "git"                        # fs (root), git (repo, rev, subdir), bundle (path), adapter (name, credential)
repo = "."
rev = "main"
subdir = "db"
```

## Secrets

Packs and config never hold secrets. Keys that look like secrets
(`password`, `token`, `secret`, `api_key`, `connection_string`, ...) are
rejected anywhere in a file. Adapters get a credential *reference*
(`env:NAME`, `keychain:SERVICE/ACCOUNT`, `vault:PATH`) in
`SQLDIFF_CREDENTIAL_REF` and resolve it themselves; the core never sees the
credential.

## Commands

```sh
sqldiff config                               # merged config and layers
sqldiff pack validate path/to/pack           # load and validate
SQLDIFF_DENYLIST="$(sqldiff pack deny-list --pack /secure/org-pack)" scripts/leak-scan.sh
sqldiff objects db/                          # what a source holds
sqldiff pairs db/ --pack /secure/org-pack    # pairing by pack rules
sqldiff pairs git:.@main//db --diff          # diff every pair at a revision
sqldiff snapshot git:.@v1.2 -o v1.2.json --history
sqldiff serve db/                            # adapter protocol on stdio
```
