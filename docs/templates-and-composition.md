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
placeholders = "parameters"                   # or "pattern": the generator's naming convention decides
unquoted_name_pattern = "[A-Za-z_][A-Za-z0-9_]*"  # where the delimiters are not SQL (default: name_pattern)
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
identifier for an object named `TAB`. The delimiters alone cannot tell.
`sqldiff template`, `diff --template` and pair diffs from a source all decide
the same way (`compose::Placeholders`): every delimited token is a candidate,
and these rules decide what it is.

| Mode | Placeholder | Reported as `template.unresolved` | Plain SQL |
|---|---|---|---|
| `placeholders = "parameters"` (default), with a parameter list | a listed name | a name that matches the pattern, or is glued, but is not listed | anything else |
| `placeholders = "parameters"`, no list | a name that matches the pattern, or is glued | none | anything else |
| `placeholders = "pattern"` (a declared naming convention) | a name that matches the pattern as written, a listed name, or a glued name | none | anything else |

- **The parameter list** comes from the source (`template.parameters` on the
  artifact, see [adapter-protocol.md](adapter-protocol.md)) or from
  `--parameter NAME` on `diff --template` (`--param` with `--known-only` on
  `sqldiff template`). The pack's `known_parameters`, names the generator
  always supplies itself, are added to a given list; on their own they do
  not make one. A listed name counts in every dialect, even where the
  pattern would not match it.
- **The pattern** is `name_pattern`, or `unquoted_name_pattern` in dialects
  where the delimiters are not SQL syntax (`[...]` outside T-SQL). A
  delimited name there cannot be a quoted identifier, so a wider pattern
  (for example with lower case) is safe.
- **Glued**: a delimited name directly attached to `@`, to identifier
  characters or to another delimited name (`@[x]`, `a_[x]_b`, `[x][y]`). A
  quoted identifier always stands apart, so where the delimiters also quote
  names a glued name can only be a placeholder, whatever its case.
- **`ignore_case = true`** is for generators that resolve names in any letter
  case: the pattern then matches in any case, `[comma]` is listed parameter
  `COMMA`, a value given as `comma` fills `[COMMA]`, and names are kept in
  upper case, so the same placeholder spelled differently on two sides
  compares equal. A declared convention is about how names are written, so
  under `placeholders = "pattern"` the convention check stays case-sensitive.
- **What the report says.** Where the delimiters also quote names in that
  dialect (`[...]` in T-SQL, `"..."` in any dialect) and the pattern decided:
  without a list, an unknown reason names the placeholders it made, so such a
  pair is never plain *equal*; under a declared convention, the convention is
  recorded as a declared assumption (`ASM-TEMPLATE-PLACEHOLDER-PATTERN-<side>`),
  so the pair can be *equal under assumptions*.

```sh
sqldiff diff --template a.tpl.sql b.tpl.sql -d tsql --pack gen-pack --parameter TAB --parameter COL
```

`pairs --diff`, the queue and the MCP `diff` compare a pair as templates when
the source marks both sides as templates and the config has a `[template]`
syntax; compositions (adapter or pack markers) apply to them as to any pair.
If only one side is marked, or no syntax is configured, the pair is compared
as plain SQL and the report says why.

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
