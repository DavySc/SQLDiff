# Formatter

`sqldiff fmt` lays out T-SQL, PostgreSQL and Db2 for i files: indentation by
block structure, a line per clause of a query or DML statement, and keyword
case.

```sh
sqldiff fmt proc.tsql.sql                    # print the result
sqldiff fmt --write db/**/*.sql              # rewrite files in place
sqldiff fmt --check db/**/*.sql              # exit 1 and list the files that are not formatted
sqldiff fmt -d postgres --keyword-case lower --indent 2 f.sql
```

Without `-d` a file's dialect comes from `dialect_map` or the path
convention, as for `diff` and `check`.

## What it may change

Only two things: the whitespace between tokens, and the letter case of
keywords. Every other byte stays as written: identifiers, literals,
comments, quoting and terminators. The formatter never adds or removes a
token.

Keyword case is changed only where it cannot change a name:

- T-SQL compares identifiers under the database collation, which can be
  case sensitive, so only reserved keywords are recased (`SELECT`, `FROM`,
  `BEGIN`, ...). A word that is not reserved, such as `Name`, `Date`, `int`
  or `NOCOUNT`, keeps its case.
- PostgreSQL and Db2 for i fold unquoted words before looking them up, so
  their keyword list also covers type names and procedural keywords.
- A word next to a dot (`t.select`) or before a colon (a label) is a name
  and keeps its case.

Some gaps keep their exact shape:

- the line break after a `--` comment;
- the lines around T-SQL `GO`, which only separates batches alone on its
  line;
- the gap between two string literals: PostgreSQL joins `'a'` and `'b'`
  into one constant when a line break separates them;
- gaps that hold anything other than spaces, tabs and line breaks;
- statements the parser could not read;
- PostgreSQL routine bodies between dollar quotes, unless `postgres_bodies`
  is on: the server stores the body text exactly as written.

No space is ever inserted next to another operator: T-SQL reads `*=` as one
operator and `* =` as two.

## How each result is checked

The formatter does not trust its own layout. Before it returns a result:

1. The output is parsed again and must give the same tokens in the same
   order with the same text; only keywords from the list above may differ,
   and only in case.
2. The tree must have the same shape: the same statements, blocks and
   inferred terminators.
3. The parser must report the same problems.
4. The engine compares each changed top-level statement with what it was
   and must find it equal. When the engine cannot reach `equal` even for
   the statement compared with itself, the output must give exactly what
   the input gives.

A statement that fails a check is kept exactly as written and listed on
stderr (`file:12-30: kept as written: <reason>`). If a failure cannot be
pinned to a statement, the whole file is returned unchanged.

## Configuration

Layout is read from a `[format]` section in `sqldiff.toml`, the user config
or a pack, merged key by key (a higher layer replaces only the keys it
sets). `[format.tsql]`, `[format.postgres]` and `[format.db2i]` override
keys for one dialect. Command-line flags come last.

```toml
[format]
indent = 4                  # spaces per level (1 to 16)
indent_style = "spaces"     # or "tabs"
keyword_case = "upper"      # "lower", "preserve"
clause_breaks = true        # FROM, WHERE, JOIN, GROUP BY, ... on their own line
one_item_per_line = false   # each select-list / SET / ORDER BY item on its own line
space_after_comma = true
space_around_comparisons = true
max_blank_lines = 1         # 0 to 10
line_ending = "keep"        # "lf", "crlf"; keep = what most of the file uses
final_newline = true
join_lines = false          # off: line breaks inside a statement are kept, only re-indented
postgres_bodies = false     # also lay out PostgreSQL routine bodies

[format.postgres]
keyword_case = "lower"
```

| Flag | Key |
|---|---|
| `--indent N` | `indent` |
| `--tabs` | `indent_style = "tabs"` |
| `--keyword-case upper\|lower\|preserve` | `keyword_case` |
| `--no-clause-breaks` | `clause_breaks = false` |
| `--one-item-per-line` | `one_item_per_line = true` |
| `--join-lines` | `join_lines = true` |
| `--postgres-bodies` | `postgres_bodies = true` |
| `--line-ending keep\|lf\|crlf` | `line_ending` |
| `--max-blank-lines N` | `max_blank_lines` |

Generator templates (files containing the pack's `[template]` open
delimiter) are not formatted: their placeholders are outside what the check
can prove.

## Not yet

Expressions are a flat row of tokens in the tree, so long `CASE`
expressions, nested subqueries and long argument lists keep the line breaks
they were written with. The GUI and the MCP server do not offer formatting
yet, and conversion output is not formatted.

The engine already ignores layout when it compares (a reformatted file
gives no findings), so the diff does not format first.
