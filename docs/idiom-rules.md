# Idiom rules (design section 8, milestone M6)

Rules are data: S-expression files in `crates/sqldiff-core/profiles/rules/`, loaded by `rules::rule::RuleSet`. Packs can add rule files with `RuleSet::load` (wiring them into pack loading is a follow-up).

| File | What |
|---|---|
| `idioms.rules` | Canonical idioms: upsert (IF EXISTS, locked IF EXISTS, MERGE, ON CONFLICT), insert-if-absent, delete-orphans, insert-identity (RETURNING or lastval), advisory lock |
| `tsql-pg.rules` | Direct T-SQL ↔ PostgreSQL translations (ISNULL, LEN, CHARINDEX, IIF, NEWID, SYSDATETIME, string `+`, STRING_AGG, DATEDIFF(day), TOP, `SELECT @v =`, NOLOCK, `dbo.`) and the functions each dialect may print |
| `lints.rules` | Lints: unrestricted correlated UPDATE (m01), upsert and insert-if-absent races (m03), TOP without ORDER BY |

## How a rule works

The matcher works on **surface terms** (`rules/surface.rs`): the statement as an S-expression, built from the equivalence parser's AST (`equiv::ast`) and, for IF and BEGIN/END, from the CST. Table aliases are replaced by the table name (and a column qualified by the only table in scope is unqualified), so equal metavariables mean alpha-equivalent code.

An **idiom** has one canonical node and a form per dialect:

```lisp
(idiom upsert/on-conflict
  (canon (upsert (target ?t) (key ?k...) (cols ?c...) (row ?v...) (on-match ?sets...) (concurrency ?conc)))
  (form postgres
    (insert (table ?t) (cols ?c...) (values (row ?v...)) (on-conflict (cols ?k...) (do-update (set ?sets...))))
    (bind ?conc atomic) (emit-any ?conc))
  (require postgres (unique ?t ?k...) ASM-UPSERT-KEY-UNIQUE "...")
  (note postgres decision "ON CONFLICT is atomic ..." (if ?conc race)))
```

- **Lift**: the form matches, its bindings build the canon.
- **Emit**: the canon matches, its bindings build the form.
- Forms are two-way unless marked `(lift-only)` or `(emit-only)`. `let` and `emit-let` compute bindings with helpers (`key-cols`, `key-pred`, `inline`, ...); `(?fact identity-col ?t)` asks the catalog.
- **Conversion** of a statement is lift with a source form, then emit with any target form whose canon matches, outermost first; the values taken from the source are converted in turn. A node no rule matches keeps its head (shared vocabulary), and the printer (`rules/print.rs`) refuses anything without a target spelling, so a statement is converted completely or not at all.
- A **translate** rule is an idiom with an implied canon `(~ID vars...)`, for direct rewrites such as `ISNULL` ↔ `coalesce`.
- **Side conditions**: `require` asks the catalog (proven: discharged; refuted: the rule does not apply; unknown: an assumption), `assume` records an assumption, `unless` / `unless-contains` / `if-contains` test the bindings.
- **Notes** carry the conversion status (`converted`, `assumptions`, `decision`, `differs`) the live conversion view shows.

Pattern syntax: `?x`, `?x...` (a run of siblings), `_`, `_...`, `(?or p q)`, `(?opt p)`, `(?block p)` (a statement or a BEGIN/END holding only it), `(seq p q)` (consecutive statements). The full grammar is in the module docs of `rules/rule.rs` and `rules/pattern.rs`.

## Verification is not circular

Rules generate SQL; they never prove it. `tests/rules.rs` converts every rule's example and diffs the input against the output with the normal engine, whose equivalence lowering (`equiv/canon.rs`) is hand-written and does not read the rules. Each example states the verdict the engine must reach (`(expect ...)`), and a rule that declares no decision may never meet a "different". Examples the engine cannot cover yet (IF statements, named locks) say `unknown` and are listed by the test; the container oracle from WP2 is the next check for them.

Other gates in the same test file:
- **Confluence**: two rules that can apply to the same term in the same direction must have different priorities (`rewrite::overlaps`).
- **Two-way**: for rules with two-way forms in both dialects, PostgreSQL → T-SQL → PostgreSQL gives the example back.
- **Near misses**: a pattern that matches except for a metavariable bound to two different terms is reported by `sqldiff check` ("almost the upsert/if-exists idiom: pred is `customer_id = @Id` in one place and `customer_id = @Other` in another"), ranked by tree edit distance.
- **Lints in diffs**: in a same-dialect diff, a lint hit that only the new version has is a finding; with `(diff-verdict different)` it makes the pair different (m01 now passes the corpus test).

## Using it for conversion

```rust
use sqldiff_core::rules::convert::{convert_nodes, Options};
let pieces = convert_nodes(&statement_nodes, Dialect::Tsql, Dialect::Postgres, &Options {
    rules: RuleSet::builtin(), facts: &schema, vars: &|v| my_name_for(v),
});
// Each piece: source units covered, output SQL or the reason it is not converted,
// status, notes, assumptions, rules used.
```

`cargo run -p sqldiff-core --example rules_convert -- FILE.sql` shows the surface terms and the conversion of a file's statements.

## Not done yet

- Pack loading of rule files, and Db2 for i forms.
- Cardinality contracts and lock intent beyond the lints above (m04, m10, m11 are caught by the equivalence engine today).
- The equivalence engine still has its own hard-coded lowering of the same idioms (upsert, anti-join delete, SCOPE_IDENTITY). That is deliberate for now: it is the independent check of the conversion rules.
