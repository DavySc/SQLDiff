# Snapshot bundle format, version 1

A snapshot bundle is an immutable, content-hashed capture of a source
(design sections 3 and 5.4). It is the offline mode, the exchange format
and the test fixture format. `sqldiff snapshot SOURCE -o FILE` writes one;
any command accepts the file as a SOURCE.

## Encoding

One UTF-8 JSON file in canonical form: object keys sorted, two-space
indentation, LF line endings, a final newline. Artifact text is stored
exactly, including its line endings and any BOM.

```json
{
  "catalog": { "routines": [], "tables": [] },
  "compositions": {},
  "format": "sqldiff-bundle",
  "format_version": 1,
  "history": { "<object id>": [ { "revision": "...", "description": "...", "work_items": [], "artifact_id": "id@rev" } ] },
  "objects": [
    {
      "info": {
        "id": "tsql/get_stock.sql", "kind": "procedure", "name": "get_stock",
        "dialect": "tsql", "version": "2022",
        "content_hash": "<sha256, normalized>", "bytes": 120,
        "last_changed": "2026-10-23T14:05:18+00:00", "stale": false
      },
      "metadata": {},
      "provenance": { "author_kind": "unknown", "revision": "..." },
      "sha256": "<sha256 of the exact text bytes>",
      "text": "CREATE PROCEDURE ..."
    }
  ],
  "pairs": [ { "a": "tsql/get_stock.sql", "b": "pg/get_stock.sql", "key": "getstock", "rule": "default" } ],
  "provenance": [ { "kind": "builtin", "name": "builtin", "version": "0.1.0", "hash": "..." } ],
  "settings": {},
  "snapshot_id": "<sha256>",
  "source": { "name": "git", "version": "0.1.0", "protocol": 1, "capabilities": ["..."], "source": "repo@rev (commit)" },
  "taken_at": "2026-10-23T14:30:00Z"
}
```

Field types are those of the adapter protocol
([adapter-protocol.md](adapter-protocol.md)).

## Integrity

- `objects` is sorted by `id`, ids are unique, `pairs` is sorted.
- `info.bytes` and `sha256` are the length and SHA-256 of `text`.
- `info.content_hash` is SHA-256 of the text with the BOM removed and each
  line's trailing whitespace and line ending normalized to `\n`; it is
  what findings and caches key on.
- `snapshot_id` is the SHA-256 of the compact canonical JSON of the bundle
  with `snapshot_id` set to `""` and `taken_at` removed. Two captures of the
  same content under the same configuration have the same id.
- `provenance` lists the packs and config layers in force at capture.

A reader rejects a bundle whose format is not `sqldiff-bundle`, whose
`format_version` is newer than it reads, or that fails any check above.
Unknown fields are rejected; new fields come with a new format version.

## As a source

The bundle adapter serves every protocol method from the file. Pairs
captured with the snapshot are returned as they are; a bundle without pairs
is paired by the currently loaded pack rules.
