# Adapter protocol, version 1

Design section 5.4. Adapters are separate programs; private adapters are
never linked into the core or shipped with it. The reference adapters
(filesystem, git, snapshot bundle) speak the same protocol in process, and
`sqldiff serve SOURCE` exposes any of them over stdio, which is also a
working example of an adapter.

## Transport

The core starts the adapter (`[[adapters]]` in a pack or config) with
stdin and stdout piped and stderr inherited. Messages are single-line JSON
objects terminated by `\n`.

```json
{"id": 1, "method": "describe", "params": {}}
{"id": 1, "result": {"name": "fs", "version": "0.1.0", "protocol": 1, "capabilities": ["list_objects", "get_artifact", "get_pairs"], "source": "db/"}}
{"id": 2, "error": {"code": -32601, "message": "method `get_catalog` not supported"}}
```

Requests are answered in order, one response per request, with the same
`id`. The adapter writes nothing else to stdout. Error codes: `-32700`
parse error, `-32600` invalid request, `-32601` method not supported,
`-32602` invalid params, `1` object not found, `2` source error.

The adapter receives `SQLDIFF_CREDENTIAL_REF` (for example
`env:DB_CREDENTIAL`) when one is configured, and resolves it itself.
`shutdown` ends the session; the adapter answers and exits.

## Methods

| Method | Params | Result |
|---|---|---|
| `describe` | `{}` | `{name, version, protocol, capabilities[], source}` |
| `list_objects` | `{dialect?, kind?, prefix?, changed_since?}` | `ObjectInfo[]` |
| `get_artifact` | `{id, offset, max_bytes?}` | `ArtifactChunk` |
| `get_pairs` | `{a_dialect?, b_dialect?}` | `Pair[]` |
| `get_composition` | `{id}` | `Composition` or `null` |
| `get_catalog` | `{schema?}` | `{tables[], routines[]}` |
| `get_history` | `{id}` | `HistoryEntry[]` |
| `get_settings` | `{schema?}` | `{name: value}` |
| `shutdown` | `{}` | `null` |

`describe` lists every other method it answers in `capabilities`; the core
does not call the rest.

**ObjectInfo**: `id` (stable identity), `kind` (`procedure`, `function`,
`trigger`, `view`, `table`, `index`, `script`), `dialect` (`tsql`,
`postgres`, `db2i`, or absent when unknown), `version`, `name` (used for
pairing), `content_hash` (normalized SHA-256, see the bundle format),
`bytes`, `last_changed` (RFC 3339), `stale` (generated output older than its
inputs; verdicts that depend on it are capped at *unknown*).

**ArtifactChunk**: `info`, `provenance` (`author_kind`: `human`,
`generated` or `unknown`; `author`, `time`, `revision`), `metadata`,
`offset`, `data`, `total_bytes`, `sha256`. Offsets count UTF-8 bytes and
chunks end on character boundaries. The core requests chunks at increasing
offsets until it holds `total_bytes` bytes, then checks the SHA-256 of the
exact text and rejects the artifact on any mismatch.

**Pair**: `a`, `b` (object ids), `key`, `rule` (what made the pair). An
adapter that pairs by itself overrides pack pairing rules.

**Catalog**: `tables[]` with `schema`, `name`, `columns[]` (`name`, `type`,
`nullable`, `collation`), `keys` and `indexes` (name to columns),
`triggers[]`; `routines[]` with `schema`, `name`, `kind`, `parameters[]`
(`name`, `type`, `mode`), `returns`.

**HistoryEntry**: `revision`, `time`, `author`, `description`,
`work_items[]` (from pack change patterns), `artifact_id` (pass to
`get_artifact` for that version's text).

**Composition**: `id`, `stale` (output older than its inputs; caps verdicts
at unknown), `parts[]` with `name`, byte `start` and `end` of the fragment,
`template`, `parameters`, optional `template_hash` (content hash of the
template it was generated from; check mode flags a part stale when the
current template differs). See [templates-and-composition.md](templates-and-composition.md).

## Reference adapters

| Source | Notes |
|---|---|
| Directory | Lists `.sql` files and files matched by `dialect_map`; skips `.git`, `target`, `node_modules` and `ignore` globs. Ids are relative paths. |
| `git:REPO[@REV][//SUBDIR]` | Reads the committed tree at `REV` (default `HEAD`) through `git`, never the working tree. `PATH@REV` ids read other revisions; `get_history` follows renames. |
| Bundle file | Serves everything captured in the snapshot. |
