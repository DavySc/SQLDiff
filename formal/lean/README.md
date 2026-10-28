# Lean checker (`sqldiff-check`)

The verified gate of [the verification roadmap](../../docs/verification-roadmap.md)
(decision 2): SQLDiff reports "equal" only when this checker confirms it, in
`require` mode (decision 3).

| File | What it is |
|---|---|
| `Sqldiff/Syntax.lean` | Generated from the Rust syntax kinds (`sqldiff_core::proof::lean_syntax`), with the schema hash. Do not edit: `SQLDIFF_BLESS=1 cargo test -p sqldiff-core --test lean_syntax` writes it, and that test fails while it is stale |
| `Sqldiff/Tree.lean` | The syntax tree as Lean types and its decoder: an unknown kind, a token where a node belongs or the reverse, or any other text fails to decode |
| `Sqldiff/Gate.lean` | Programs, refinement (decision 1), the check, and `check_sound`: a confirmed answer means B refines A, for every semantics |
| `Main.lean` | The executable: one tab-separated request per line on stdin (`sqldiff-check/2`, with the schema hash), one JSON answer per line on stdout. A request whose schema hash is not `Syntax.schemaHash`, or whose trees do not decode, is unconfirmed |
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
