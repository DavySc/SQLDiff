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
| `Sqldiff/Core/Syntax.lean` | The core language of phases C1 and C2a ([verified-conversion.md](../../docs/verified-conversion.md), decision 9): parameters with modes and defaults, locals, assignment, `if`, `while`, `return`, sequences, calls; expressions are slice expressions (`SExpr`: variables, 32-bit integer literals, NULL, `+ - * / %`, negation, comparisons, `AND OR NOT`, `IS [NOT] NULL`, `coalesce`, `CASE WHEN`) or uninterpreted operators |
| `Sqldiff/Core/Semantics.lean` | Values (NULL, three-valued booleans, 32-bit integers, opaque values), errors (overflow, division by zero), the meaning of slice expressions, the fuel-based, executable interpreter, what a caller observes (out parameters, returned value or error kind, world, call list; decision 1) and `Refines` |
| `Sqldiff/Core/Rename.lean` | An injective renaming of a routine's variables is observationally the same routine (`refines_rename`) |
| `Sqldiff/Core/Fuel.lean` | Once a run finishes (with a result or an error), more fuel gives the same observation (`obs_mono`) |
| `Sqldiff/Core/OpFree.lean` | A routine without uninterpreted operators means the same whatever they mean (`Routine.obs_opFree`) |
| `Sqldiff/Mirror/Lower.lean` | `lowerT` and `lowerP`: the T-SQL and PL/pgSQL spec of the C1 constructs and the C2a slice (each slice rule with its vendor citation and probe status), as lowerings of the generated `Sqldiff.Ast.Routine` into the core; partial (`none` is "outside the spec"); `sliceT`, `sliceP`, `abortsT`: whether a routine is in the C2a slice |
| `Sqldiff/Mirror/Check.lean` | The mirror check with an untrusted certificate (the variable renaming), `check_sound`, `check_sound_c2a` and the residual assumptions of a C2a answer |
| `Sqldiff/Mirror/Request.lean` | The fields of a `sqldiff-check/3` mirror request and their decoding; the extra answer fields (`scalars`, `residual`) |
| `Sqldiff/Mirror/Examples.lean` | Confirmed and refused pairs and interpreter runs, as `#guard`s checked on every build |
| `Main.lean` | The executable: one tab-separated request per line on stdin (`sqldiff-check/2`, with the schema hash; `sqldiff-check/3` for the mirror check, with the abstract syntax schema hash), one JSON answer per line on stdout (the answer format is documented there). A request whose schema hash is not the checker's, or whose trees or routines do not decode, is unconfirmed |
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

What a C1 confirmation does not cover: operators are uninterpreted, and
the same operator name is taken to mean the same function in both
dialects (`ASM-C1-OPERATORS` on the Rust side), so it is about the control
structure, variables, parameters, calls and the T-SQL return status, not
about real T-SQL against real PostgreSQL expressions. There is no data
(C3) and no error handling (C4).

### Phase C2a: the first scalar slice

The core now has values (NULL, three-valued booleans, 32-bit integers,
and opaque values for everything else), an error outcome (overflow,
division by zero) and a fixed meaning for the expressions of one slice
(`Sqldiff.Core.SExpr.eval`). An error ends the routine, and the caller
observes its decision-1 kind (`arithmetic`), the world and the calls made
before it. Each dialect's lowering types its expressions and lowers the
slice to `SExpr`; everything else stays on the C1 path, an uninterpreted
operator.

The slice (`Sqldiff/Mirror/Lower.lean`, one comment per rule with its
Microsoft Learn page, PostgreSQL manual section and probe status):

- variables declared T-SQL `int`/`integer`, PostgreSQL
  `integer`/`int`/`int4`; integer literals in the 32-bit range; NULL in a
  value position;
- `+ - * /` on two integers (overflow raises, division truncates toward
  zero, a zero divisor raises), `%` with a literal divisor other than 0
  and -1, unary minus;
- `= <> < > <= >=` on two integers, three-valued; `AND`, `OR`, `NOT` with
  the three-valued tables (`AND`/`OR` only when neither operand can
  raise); `IS [NOT] NULL`;
- `COALESCE` of integers, T-SQL `ISNULL(x, y)` of integers when `y`
  cannot raise (the same core term as `COALESCE(x, y)`), searched
  `CASE WHEN ... THEN ... [ELSE ...] END` of integers;
- `IF` and `WHILE` take the branch only on TRUE; assignment of an integer
  or NULL to an integer variable; `SET XACT_ABORT ON`.

An expression without variables that fails (`1/0`) is outside the slice:
PostgreSQL may raise it while planning, even in a branch not taken.

A confirmed `/3` answer says which case applies. When every expression
of both routines is in the slice, neither lowering has an uninterpreted
operator, and the T-SQL routine either starts with `SET XACT_ABORT ON` or
has no expression that can raise, the answer names
`Sqldiff.Mirror.check_sound_c2a` and carries `"scalars":"c2a"` and
`"residual":[...]`; otherwise it names `Sqldiff.Mirror.check_sound` and
carries `"scalars":"uninterpreted"`.

Trust statement. `check_sound_c2a`: both routines are in the slice, the
PL/pgSQL routine refines the T-SQL one for every world, and both mean the
same whatever the uninterpreted operators mean (`Routine.obs_opFree`), so
the confirmation does not rest on `ASM-C1-OPERATORS`. It rests instead on
the slice rules being right (trusted spec: each cites the vendor text and
says whether `eval/` probes cover it; most are not probed yet), and on the
T-SQL session facts in `residual`: `ASM-TSQL-ARITH-ERRORS` when an
expression can raise (`ANSI_WARNINGS`/`ARITHABORT` not both `OFF`, so
overflow and division by zero raise rather than give NULL) and
`ASM-TSQL-ANSI-NULLS` when it uses `=` or `<>` (the routine was created
with `ANSI_NULLS ON`). The other trusted inputs are unchanged (the
lowering, `dep`, `Γ`, the parsers).

What stays under `ASM-C1-OPERATORS` (a `check_sound` answer): every
other type (`bigint`, `decimal`, strings, dates, `bit`, ...), casts and
implicit conversions, function calls other than `COALESCE`/`ISNULL`,
`%` with a variable divisor, simple `CASE`, `IN`, `BETWEEN`, `LIKE`,
comparisons with a NULL literal, `AND`/`OR` over operands that can raise,
and T-SQL routines whose expressions can raise but that do not start with
`SET XACT_ABORT ON` (T-SQL goes on after a failed statement; PostgreSQL
stops). For those the core still evaluates the slice parts exactly, but
the answer also rests on the C1 caveat that runs raise no error.
