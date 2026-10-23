# Templates and composition

Design §1 (two levels), §3 layer 1 (template layer), §3 layer 4 (composition),
§5.3 and §9 ("proven from composed output"). The core knows no generator: the
template syntax and the provenance markers come from a pack.

## Template layer (`sqldiff_syntax::template`)

A pack's `[template]` section defines the syntax:

```toml
[template]
open = "{{"
close = "}}"
name_pattern = "[A-Z][A-Z0-9_]*"
special_forms = [{ name = "include", pattern = "<%@include\\s+(\\S+)\\s*%>" }]
parameter_sources = ["composition", "cli"]   # lookup order
collision = "error"                           # first | last | error
known_parameters = ["SCHEMA"]                 # supplied by the generator itself
ignore_case = false                           # true: [schema] is parameter SCHEMA
```

A template is split losslessly into SQL spans, placeholders, special forms
(their capture groups are arguments) and unresolved candidates, so printing
the segments gives the input back.

- **Fragment**: placeholders become identifiers `TPL_<NAME>` and special forms
  become block comments, so the SQL parser reads the template as SQL. An offset
  map takes every fragment offset back to the template.
- **Substitution**: parameter values give the generator's output, with an offset
  map template ↔ output. A missing value or a special form (generator logic) is
  reported, never expanded by guess.
- **Collisions with SQL**: when the known parameters are given, a match that
  names none of them (`[qty]` with `open = "["`) stays SQL and is reported as
  unresolved (`template.unresolved`).
- **Parameter sources**: values from several sources merge in
  `parameter_sources` order; under `collision = "error"` two different values are
  an input problem, shown in the report.

```sh
sqldiff template proc.tpl.sql -d tsql --pack gen-pack            # placeholders, fragment diagnostics, round trip
sqldiff template proc.tpl.sql -d tsql --pack gen-pack --param SCHEMA=dbo --render
sqldiff diff --template a.tpl.sql b.tpl.sql -d tsql --pack gen-pack   # template-level diff
```

### Placeholder or quoted name?

With `open = "["`, `[TAB]` can be a placeholder or, in T-SQL, a quoted
identifier for an object named `TAB`. The delimiters alone cannot tell, so
the parameter names decide:

| Parameter names given by | Then |
|---|---|
| the source (`template.parameters` on the artifact, see [adapter-protocol.md](adapter-protocol.md)) or `--parameter NAME` on `diff --template` | Only those names, plus the pack's `known_parameters`, are placeholders. Any other name that matches the pattern is read as SQL and reported as `template.unresolved`, with its location in the template. |
| nobody | The name pattern decides. Where the delimiters also quote names in that dialect (`[...]` in T-SQL, `"..."` in any dialect), the report says which names were taken as placeholders on that basis. This is an unknown reason, so such a pair is never reported as plain *equal*. |

`known_parameters` in `[template]` lists names the generator always supplies
(for example an object id or a column list), which a source does not list
per template. They are added to a given list; on their own they do not make
a list.

`ignore_case = true` is for generators that resolve placeholder names in any
letter case: `[comma]` then is parameter `COMMA`, and a value given as
`comma` fills `[COMMA]`. Names are kept in upper case, so the same
placeholder spelled differently on two sides compares equal. With a
parameter list, `name_pattern` can be as broad as the generator's names
(including lower case): the list decides, and a name outside it is SQL.

```sh
sqldiff diff --template a.tpl.sql b.tpl.sql -d tsql --pack gen-pack --parameter TAB --parameter COL
```

`pairs --diff`, the queue and the MCP `diff` compare a pair as templates when
the source marks both sides as templates and the config has a `[template]`
syntax. A template without that syntax is compared as plain SQL, and the
report says so.

A template-level diff compares the two fragments; locations point into the
templates and witnesses quote placeholders as the template spells them.

## Composition (`sqldiff_core::composition`)

Generated output is a sequence of parts: name, template, parameters and byte
span. Behaviour is analysed on the composed output; the composition adds:

| Mode | What happens |
|---|---|
| Read (authoritative) | Parts come from the adapter's `get_composition`, else from the pack's `[[composition]]` markers. Unbalanced or nested markers and out-of-range spans are listed under "Not proven". |
| Check (oracle) | With `--templates DIR` (templates by file stem), each part's template is filled with the part's parameters and compared with the part, line by line, ignoring trailing whitespace. A difference is `composition.substitution-mismatch` (unknown); a part that cannot be checked is listed, never skipped silently. |
| Part diff | A part on one side only is `composition.part-removed` / `composition.part-added`, with the class of the statements it holds; the statement findings inside it are folded into it. A changed template or parameters is an informational finding. |
| Wrapper facts | Session options, transaction starts, error handlers, ATOMIC blocks and the Db2 commit level found in a part other than the one holding the routine's writes are recorded as *proven from composed output* (`COMPOSED-<side>:<part>:<fact>`). They are discharged, so they do not turn EQUAL into "equal under assumptions". |
| Staleness | `stale` on an adapter composition, or a part whose `template_hash` differs from the current template in check mode, caps the verdict at unknown. |

Marker syntax:

```toml
[[composition]]
name = "part markers"
begin = '^-- \[part:(?P<part>[a-z0-9-]+) template=(?P<template>[a-z0-9-]+)\]$'
end = '^-- \[/part\]$'
```

Markers are matched per line. In `begin`, the named group `part` names the
part, `template` its template, and every other named group becomes a
parameter. Parts do not nest.

`diff`, `check`, `tree` and `pairs --diff` read compositions whenever the
loaded config has `[[composition]]` markers or the source answers
`get_composition`; `diff` and `check` take `--templates DIR` for check mode.

Corpus case m23 (`corpus/mutations/m23-composed-fragment-dropped`) is the gate:
with `corpus/packs/synthetic-generator.toml` the leading finding is the dropped
`session-options` part, class transactional, next to the atomicity witness.
