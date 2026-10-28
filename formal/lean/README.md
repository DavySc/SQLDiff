# Lean checker (`sqldiff-check`)

The verified gate of [the verification roadmap](../../docs/verification-roadmap.md)
(decision 2): SQLDiff reports "equal" only when this checker confirms it, in
`require` mode (decision 3).

| File | What it is |
|---|---|
| `Sqldiff/Syntax.lean` | Generated from the Rust syntax kinds (`sqldiff_core::proof::lean_syntax`), with the schema hash. Do not edit: `SQLDIFF_BLESS=1 cargo test -p sqldiff-core --test lean_syntax` writes it, and that test fails while it is stale |
| `Sqldiff/Ast.lean` | Generated from the abstract syntax schema (`sqldiff_syntax::encode::SCHEMA`, `sqldiff_core::proof::lean_ast`): routines, statements and expressions as Lean types, and the decoder for their one-line encoding. Do not edit; the same bless command writes it |
| `Sqldiff/Tree.lean` | The syntax tree as Lean types and its decoder: an unknown kind, a token where a node belongs or the reverse, or any other text fails to decode |
| `Sqldiff/Gate.lean` | Programs, refinement (decision 1), the check, and `check_sound`: a confirmed answer means B refines A, for every semantics |
| `Sqldiff/Core/Syntax.lean` | The core language of phase C1 ([verified-conversion.md](../../docs/verified-conversion.md), decision 9): parameters with modes and defaults, locals, assignment, `if`, `while`, `return`, sequences, calls; expressions are variables, integer literals, NULL and uninterpreted operators |
| `Sqldiff/Core/Semantics.lean` | The fuel-based, executable interpreter, what a caller observes (out parameters, returned value, world, call list; decision 1) and `Refines` |
| `Sqldiff/Core/Rename.lean` | An injective renaming of a routine's variables is observationally the same routine (`refines_rename`) |
| `Sqldiff/Core/Fuel.lean` | Once a run finishes, more fuel gives the same observation (`obs_mono`) |
| `Sqldiff/Mirror/Lower.lean` | `lowerT` and `lowerP`: the T-SQL and PL/pgSQL spec of the C1 constructs, as lowerings of the generated `Sqldiff.Ast.Routine` into the core; partial (`none` is "outside C1") |
| `Sqldiff/Mirror/Check.lean` | The mirror check with an untrusted certificate (the variable renaming) and `check_sound` |
| `Sqldiff/Mirror/Request.lean` | The fields of a `sqldiff-check/3` mirror request and their decoding |
| `Sqldiff/Mirror/Examples.lean` | Confirmed and refused pairs and interpreter runs, as `#guard`s checked on every build |
| `Main.lean` | The executable: one tab-separated request per line on stdin (`sqldiff-check/2`, with the schema hash; `sqldiff-check/3` for the mirror check, with the abstract syntax schema hash), one JSON answer per line on stdout. A request whose schema hash is not the checker's, or whose trees or routines do not decode, is unconfirmed |
| `Axioms.lean`, `check_axioms.py` | `#print axioms` for every theorem the gate relies on, checked against the allowlist (Lean's three standard axioms; vendor facts only through a signed `SPEC.lock`, decision 5) |
| `test_gate.py` | Protocol tests against the built binary |
| `measure.py` | Start-up, throughput, size and linked libraries (decision 2 budgets) |

Phase 0 covers the trivial fragment only: both sides in the same dialect,
analysed for the same target, with identical syntax trees once whitespace
and comments are left out. The proof is reflexivity, so it needs no
specification and no axioms. What it trusts: SQLDiff's parser,
`sqldiff_core::proof::tree_line`, which writes a tree injectively, and
`Tree.decode`, which reads it back. A syntax kind added, removed or renamed
in Rust changes the schema hash, so an old checker refuses (unconfirmed)
until `Syntax.lean` is generated again and the checker rebuilt.

The toolchain is pinned in `lean-toolchain`. Executable code imports only
`Init`. `.github/workflows/formal-lean.yml` builds and tests it on every
release target, and on Linux runs the Rust CLI's proof tests against it
(`SQLDIFF_REAL_CHECK`).

### Phase C1: the mirror check (`sqldiff-check/3`)

`Sqldiff.Mirror.check dep Γ cert a b`, served as the `sqldiff-check/3`
request kind (`Main.lean`; `/2` is unchanged), confirms a T-SQL routine `a` and a
PL/pgSQL routine `b` when `lowerP Γ b` is `lowerT dep a` with its variables
renamed by `cert`, the renaming injective on `a`'s variables.
`check_sound` says a confirmed answer means `b` refines `a` for every
meaning of the operators and every world. Trusted, beyond the above: the
lowering (it is the spec), the deployment facts `dep` (whether row counts
are observed, which T-SQL routine name reaches which target routine) and
the target catalog `Γ` (callee parameter modes). Not trusted: the
certificate.

What a confirmation does not cover yet: operators are uninterpreted, and
the same operator name is taken to mean the same function in both
dialects, so a C1 confirmation is about the control structure, variables,
parameters, calls and the T-SQL return status, not about real T-SQL
against real PostgreSQL expressions (C2). There are no errors (C4) and no
data (C3).
