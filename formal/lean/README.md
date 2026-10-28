# Lean checker (`sqldiff-check`)

The verified gate of [the verification roadmap](../../docs/verification-roadmap.md)
(decision 2): SQLDiff reports "equal" only when this checker confirms it, in
`require` mode (decision 3).

| File | What it is |
|---|---|
| `Sqldiff/Gate.lean` | Programs, refinement (decision 1), the check, and `check_sound`: a confirmed answer means B refines A, for every semantics |
| `Main.lean` | The executable: one tab-separated request per line on stdin, one JSON answer per line on stdout |
| `Axioms.lean`, `check_axioms.py` | `#print axioms` for every theorem the gate relies on, checked against the allowlist (Lean's three standard axioms; vendor facts only through a signed `SPEC.lock`, decision 5) |
| `test_gate.py` | Protocol tests against the built binary |
| `measure.py` | Start-up, throughput, size and linked libraries (decision 2 budgets) |

Phase 0 covers the trivial fragment only: both sides in the same dialect,
analysed for the same target, with identical syntax trees once whitespace
and comments are left out. The proof is reflexivity, so it needs no
specification and no axioms. What it trusts: SQLDiff's parser, and
`sqldiff_core::proof::tree_line`, which writes a tree injectively.

The toolchain is pinned in `lean-toolchain`. Executable code imports only
`Init`. `.github/workflows/formal-lean.yml` builds and tests it on every
release target.
