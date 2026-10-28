/-
The lowering of T-SQL and PL/pgSQL routines, as the Rust parser reads them
(`Sqldiff.Ast`, generated), into the core. Decision 9
(docs/verified-conversion.md): this file *is* the specification of the C1
constructs, of the C2a scalar slice with the C2b strings, of the C3a data statements and
of the C4 error handling and transaction control in both
dialects; there is no other T-SQL or PL/pgSQL semantics to prove it
against. One abstract syntax serves both dialects and the dialect is a
parameter. The lowering is partial: `none` means "outside the spec"
(queries, `MERGE`, data statements outside C3a, handlers and
transaction statements outside C4, other settings, ...), and a routine
outside it is never confirmed.

Errors (C2a): an expression of the slice can fail (overflow, division by
zero), and the core then ends the routine with an error observation. That
is what PL/pgSQL does without an `EXCEPTION` block, and what T-SQL does
under `SET XACT_ABORT ON`; a T-SQL statement that fails without it is
rolled back on its own and the procedure goes on. So the T-SQL lowering is
faithful for every run only when the routine aborts on errors
(`abortsT`), and otherwise for the runs that raise none, as in C1. A C2a
confirmation (`Sqldiff.Mirror.inC2a`) asks for one or the other. Errors
from elsewhere (a missing argument without a default, `OUTPUT` on a
parameter that is not one) are not modelled: the rule says what happens
otherwise. A failing callee is not modelled either (`WorldModel.call`
does not fail), but a call counts as a statement that can fail
(`Core.Stmt.failsHere`), so a T-SQL routine with a call is in the C2a
slice only when it aborts on errors: then a callee's error ends both
routines, as any error does. C4 handles errors (below) but still does
not model a callee's errors, so a call is outside the spec where an error
would be handled.

Data (C3a): single-table `INSERT ... VALUES`, `UPDATE` and `DELETE` on a
table the target catalog describes, with the columns of the row in scope
(section "The C3a slice: data statements", each rule with its vendor
citation and probe status). A data statement that fails has no effect
(statement atomicity) and then ends the routine like any error.

Errors and transactions (C4): T-SQL `TRY`/`CATCH` and PL/pgSQL
exception blocks, `THROW` and `RAISE`, `BEGIN TRAN`/`COMMIT` in a
bracket, the rollback-and-rethrow idiom, and PL/pgSQL `COMMIT` (section
"The C4 rules", each rule with its citation and probe status).

Read top to bottom: the trusted inputs, names in data statements, the C2a
slice with its vendor citations, the C2b strings, expressions, the C3a data statements,
statements, calls, the C4 rules, blocks, the two routines, and the test for "every
expression is in the slice".
-/
import Sqldiff.Ast
import Sqldiff.Core.Semantics
import Sqldiff.Mirror.Names

namespace Sqldiff.Mirror

open Sqldiff.Core (Var Stmt Arg seqList SExpr AOp COp)

/-! ## Trusted inputs -/

/-- Facts about the T-SQL deployment, from the deployment profile
(docs/verified-conversion.md, C0 item 6). Trusted: the check cannot see
the databases. -/
structure Deployment where
  /-- The observer profile reads rows-affected counts (decision 1; not
  observed across dialects by default, `ASM-OBSERVER-ROWCOUNT`). -/
  rowCounts : Bool
  /-- Each T-SQL routine a call may reach, `.`-joined as written, and the
  target routine it corresponds to (`dbo.p` to `p`: `ASM-DBO-SCHEMA`).
  Whether two names reach routines that behave alike is a fact about the
  two databases, so it is an input here, not a certificate hint. -/
  routines : List (String × String)
  /-- Each T-SQL table a data statement may name, `.`-joined as written,
  and the target table it corresponds to (`dbo.t` to `t`: the schema
  mapping of `ASM-DBO-SCHEMA`, and for an unqualified name the default
  schema of `ASM-DEFAULT-SCHEMA`), as for routines. Phase C3a. -/
  tables : List (String × String) := []

/-- A column of a target table (C3a): its name as the target catalog
spells it (folded as `pgName` folds; the core's column), its spelling in
the T-SQL table it corresponds to, and its constraints. Every column of a
catalog table is a 32-bit integer (T-SQL `int`, PostgreSQL `integer`):
the catalog encoding has no other type yet (`Sqldiff.Mirror.Request`). -/
structure CatCol where
  name : Core.Col
  tsql : String
  notNull : Bool
  key : Bool
deriving Repr

/-- A target table and how the T-SQL table it corresponds to spells its
columns (the state correspondence of decision 1: rows related column by
column, the identity on values). What the catalog asserts about the table
and about its T-SQL counterpart, which the check cannot see: both have
exactly these columns, all `int`/`integer`, with these `NOT NULL` and
primary key constraints; no other constraint (`CHECK`, `UNIQUE`, foreign
keys), no default, no identity, sequence, computed or generated column, no
trigger, rule or row-level security policy; the primary key is not
`DEFERRABLE INITIALLY DEFERRED` in PostgreSQL and its index does not
`IGNORE_DUP_KEY` in SQL Server; and the PostgreSQL table has no
inheritance children or partitions. The Rust side sends a table only when
its catalogs show all of this. -/
structure CatTable where
  name : String
  cols : List CatCol
deriving Repr

/-- The target catalog (trusted like every catalog fact; its hash goes in
the certificate): the parameter modes of the PostgreSQL routines a `CALL`
can reach (PL/pgSQL writes back to the variable in each `OUT` and `INOUT`
position, so the lowering of `CALL` needs them), and the tables data
statements may write (C3a). -/
structure Catalog where
  routines : List (String × List Ast.Mode)
  tables : List CatTable := []
  /-- (C2b) A fact about both databases, from the deployment profile:
  the T-SQL database that holds the routine has a binary default
  collation (a `_BIN` or `_BIN2` collation), and every collation a
  PostgreSQL comparison in the routine can take is deterministic (the
  database's default, and the collation of every argument a caller
  passes). Without it, `=` and `<>` on strings are outside the slice. The
  answer names it as `ASM-BINARY-COLLATION` where a comparison uses it.
  See "The C2b slice: strings". -/
  binaryCollation : Bool := false

/-- The empty catalog. -/
instance : EmptyCollection Catalog := ⟨{ routines := [] }⟩

/-- A dialect and its trusted inputs. `vars` are the PL/pgSQL routine's
variable names (folded), which a name in a data statement may clash with
(`resolveName`). -/
inductive Dialect where
  | tsql (dep : Deployment) (Γ : Catalog)
  | pg (Γ : Catalog) (vars : List Var)

def dotted (parts : List String) : String := ".".intercalate parts

/-- A PostgreSQL name as PostgreSQL resolves it. The Rust parser keeps
names as written, except that a quoted name whose case matters arrives
with a leading `"` (`"Foo"` as `"Foo`; a quoted all-lower-case name
arrives without the quote, `sqldiff_syntax::abstract_syntax::quoted_name`).
PostgreSQL folds unquoted names to lower case, so `X` and `x` are one
variable: a name with the leading `"` is kept, every other name is
lower-cased. PostgreSQL folds only ASCII letters, and so does
`String.toLower`; non-ASCII names are outside the spec anyway
(`asciiNames`), since an unquoted `Ä` and a quoted `"Ä"` are one name to
PostgreSQL but not to this rule. Catalog keys are names folded by this
rule. -/
def pgName (n : String) : String :=
  if n.startsWith "\"" then n else n.toLower

/-! ## Names in data statements (C3a)

A data statement brings columns into scope (`Scope`): the target table's,
in `UPDATE` and `DELETE`; none in `INSERT ... VALUES`, whose values are
evaluated before any row exists. A one-part name (`Ast.Expr.Name [x]`) in
an expression resolves as follows (`resolveName`).

* **T-SQL**: a variable is spelt `@x` (`Ast.Expr.Var`), so a name is a
  column, or outside the spec when it is none of the target's. A name
  matches a column only when it is spelt exactly as the catalog's T-SQL
  spelling of the column (`CatCol.tsql`); `wellFormed` asks those to be
  ASCII and distinct up to case, so the match is the same under a
  case-sensitive and a case-insensitive collation. Microsoft Learn,
  "Database identifiers" (identifier comparison follows the collation).
  Not probed.
* **PL/pgSQL**: a name is folded (`pgName`). If it is a column of the
  target and also a variable of the routine (a parameter, a local, or
  `found`: "FOUND is a local variable within each PL/pgSQL function",
  PL/pgSQL "Obtaining the Result Status"), it is outside the spec: "By default, PL/pgSQL will
  report an error if a name in an SQL statement could refer to either a
  variable or a table column" (PL/pgSQL "Variable Substitution"; the
  `plpgsql.variable_conflict` setting or a `#variable_conflict` option
  would resolve it either way instead, so refusing every such name makes
  the lowering independent of that setting and no
  `ASM-PG-VARIABLE-CONFLICT-ERROR` is needed). A column that is not a
  variable is the column; a name that is the table's own reference name
  (the last part of the target's name) and not a column is a whole-row
  reference, outside the spec (and an ambiguity error, too, when a
  variable has that name); any other name is a variable, which must be
  declared (`wellScoped`). Not probed.
* In both dialects, the column list of `INSERT` and the column a `SET`
  assigns are columns, never variables ("Variable Substitution": in
  `INSERT INTO dest (col) ...`, "col must be a column of dest"), resolved
  the same way
  (`colOf`). A two-part name (`t.c`) is outside the spec. -/

/-- The columns in scope: each as a statement may write it (the T-SQL
spelling, or the folded PostgreSQL name) and the core column; and, for
PostgreSQL, the target's reference name. Empty outside data statements. -/
structure Scope where
  cols : List (String × Core.Col) := []
  rel : Option String := none

/-- A one-part name: a column (`inl`), a variable (`inr`), or outside the
spec (`none`). Outside data statements a T-SQL name is outside the spec
and a PostgreSQL name is a variable, as in C1. -/
def resolveName (d : Dialect) (sc : Scope) (x : String) : Option (Sum Core.Col Var) :=
  match d with
  | .tsql _ _ => (lookup sc.cols x).map .inl
  | .pg _ vars =>
    let n := pgName x
    -- C4: the error variables of a handler are outside the spec.
    if n == "sqlstate" || n == "sqlerrm" then none else
    match lookup sc.cols n with
    | some c => if vars.contains n then none else some (.inl c)
    | none => if sc.rel == some n then none else some (.inr n)

/-! ## The C2a slice: types

Each rule of the slice below states its claim, the vendor text it rests
on (Microsoft Learn page title for T-SQL, PostgreSQL 18 manual section
for PostgreSQL), and whether the repository's engine probes cover it.
"eval/casts" is `eval/casts/{mssql,postgres}.sql` and their `.out` files,
run on SQL Server 2022 and PostgreSQL 18 on 2026-10-26 with constant
operands in `SELECT`; "not probed" means no engine run in the repository
shows it. The rules are trusted content (decision 5 of
docs/verification-roadmap.md): an engine run can refute one, not confirm
it.

**Types.** The slice has one value type, the 32-bit signed integer, plus
the three-valued booleans of conditions and the NULL literal.

* T-SQL `int`, and its synonym `integer`, is a 32-bit integer from
  -2^31 to 2^31-1 (Microsoft Learn, "int, bigint, smallint, and tinyint
  (Transact-SQL)"; "Data type synonyms (Transact-SQL)": `integer` is
  `int`). The Rust parser upper-cases type names
  (`sqldiff_syntax::abstract_syntax::Parser::ty`); the match is
  case-insensitive. Not probed as a variable type.
* PostgreSQL `integer`, `int` and `int4` are one type with that range
  (PostgreSQL manual 8.1.1 "Integer Types", Table 8.2 and Table 8.1
  aliases). Not probed as a variable type. A user type named `int4`
  earlier in `search_path` than `pg_catalog` would shadow the last
  spelling; `pg_catalog` is searched first unless the path names it
  later (5 "Data Definition", "Schemas", "The System Catalog Schema"),
  which this rule assumes.
* Type arguments (`int(4)`) put a declaration outside the slice. -/

/-- The type of an expression of the slice. `nul` is the NULL literal,
which takes the integer type from where it stands (a value position:
assignment, `COALESCE`/`ISNULL` argument, `CASE` result). `str n wide`
is a string (C2b, "The C2b slice: strings"): `n` bounds the length of
every value it can have (`none`: no bound), in T-SQL in bytes for
`varchar` and in UTF-16 code units for `nvarchar` (so also in
characters), in PostgreSQL in characters; `wide` says that a T-SQL value
may hold characters outside the database's code page (it comes from an
`nvarchar`); it is always `false` in PostgreSQL. -/
inductive STy where
  | int
  | bool
  | nul
  | str (n : Option Nat) (wide : Bool)
deriving DecidableEq, Repr

/-- The declared types of a routine's variables, by lowered name. -/
abbrev TyEnv := List (Var × Ast.Ty)

/-- The declared type is the slice's integer type. -/
def intType : Dialect → Ast.Ty → Bool
  | .tsql _ _, .Ty n [] => n.toUpper == "INT" || n.toUpper == "INTEGER"
  | .pg _ _, .Ty n [] => n.toUpper == "INT" || n.toUpper == "INTEGER" || n.toUpper == "INT4"
  | _, _ => false

def intVar (d : Dialect) (env : TyEnv) (x : Var) : Bool :=
  match lookup env x with
  | some t => intType d t
  | none => false

/-- A value position takes an integer or the NULL literal. -/
def valTy : STy → Bool
  | .int => true
  | .nul => true
  | .bool => false
  | .str .. => false

/-! ## The C2a slice: literals

An integer literal is in the slice when it is decimal digits, optionally
after a `-` (the Rust parser folds a unary minus on a number into the
number's text, `-5` as `Num "-5"`), without a leading zero, and in the
32-bit range.

* T-SQL: an integer constant in the `int` range is `int`; one outside it
  is `numeric` (`decimal`), so `2147483648` is outside the slice
  (Microsoft Learn, "Constants (Transact-SQL)", integer constants).
  Probed (eval/casts: `2147483647` and `-2147483648` are `int`,
  `2147483648` is `numeric(10,0)`).
* PostgreSQL: "A numeric constant that contains neither a decimal point
  nor an exponent is initially presumed to be type integer if its value
  fits in type integer (32 bits)" (4.1.2.6 "Numeric Constants"); larger
  ones are `bigint` or `numeric`. Probed (eval/casts: `2147483647` and
  `-2147483648` are `integer`, `2147483648` is `bigint`).
* Underscores (PostgreSQL 16 and later accept `1_000`), hexadecimal,
  exponents and leading zeros are outside the slice: `String.toInt?`
  accepts underscores, so this rule reads the digits itself. -/

def digitsVal : List Char → Nat → Option Nat
  | [], acc => some acc
  | c :: cs, acc =>
    if '0' ≤ c && c ≤ '9' then digitsVal cs (acc * 10 + (c.toNat - '0'.toNat)) else none

def intLit? (s : String) : Option Int :=
  let (neg, ds) : Bool × List Char := match s.toList with
    | '-' :: ds => (true, ds)
    | ds => (false, ds)
  if ds.isEmpty || ds.length > 10 || (ds.length > 1 && ds.head? == some '0') then none
  else (digitsVal ds 0).bind fun n =>
    let v : Int := if neg then -(n : Int) else n
    if Core.intMin ≤ v ∧ v ≤ Core.intMax then some v else none

/-! ## The C2a slice: operators

**Constant subexpressions.** PostgreSQL's planner evaluates a
subexpression without variables when it plans the statement, even in a
`CASE` branch that is never taken: "`CASE WHEN x > 0 THEN 1 ELSE 1/0 END`
will likely result in a division-by-zero failure due to the planner
trying to simplify the constant subexpression" (9.18.1 "CASE"; 4.2.14
"Expression Evaluation Rules"). T-SQL makes no such promise either way. So
an expression without variables whose evaluation fails is outside the
slice (`closedSafe`); a constant one that does not fail means the same
folded or not. PL/pgSQL evaluates a simple expression with a generic plan,
in which variables are parameters, not constants, so only expressions
without variables are folded. Not probed.

**Evaluation order.** Neither vendor fixes the order in which operands
are evaluated, nor that an operand whose value is not needed is skipped:
PostgreSQL: "The order of evaluation of subexpressions is not defined"
and "if the result of an expression can be determined by evaluating only
some parts of it, then other subexpressions might not be evaluated at
all" (4.2.14). The core evaluates both operands of every strict operator,
left to right. That is safe because (1) the only errors in the slice are
arithmetic ones, which decision 1 compares by kind (`ErrClass`), so which
of two failing operands fails first is not observed, and (2) `AND` and
`OR`, the operators whose result one operand can decide, are in the slice
only when neither operand can raise (`SExpr.mayRaise`), so skipping one
changes nothing. `CASE`, `COALESCE` and `ISNULL` are lazy (below).

**Arithmetic** (`+ - * / %`) on two `int` operands is `int`, NULL if
either operand is NULL, and raises on overflow.

* T-SQL: "+ (Addition)", "- (Subtraction)", "* (Multiplication)" and
  "/ (Divide) (Transact-SQL)": the result has "the data type of the
  argument with the higher precedence" ("Data type precedence
  (Transact-SQL)"), so `int` for two `int`s; an `int` result out of range
  is "Arithmetic overflow error converting expression to data type int"
  (error 8115). Probed (eval/casts: `2147483647 + 1` and
  `CAST(-2147483648 AS int) / -1` raise 8115).
* PostgreSQL: Table 9.4 in 9.3 "Mathematical Functions and Operators";
  `integer` arithmetic raises "integer out of range" (SQLSTATE 22003).
  Probed (eval/casts: the same two raise it).

**Division** truncates toward zero; a zero divisor raises.

* T-SQL: "If an integer dividend is divided by an integer divisor, the
  result is an integer that has any fractional part of the result
  truncated" ("/ (Divide) (Transact-SQL)"); division by zero is error
  8134. Probed (eval/casts: `7 / 2` is 3, `-7 / 2` is -3, `1 / 0` raises
  8134).
* PostgreSQL: "Division (for integral types, division truncates the
  result towards zero)" (Table 9.4); SQLSTATE 22012 "division by zero".
  Probed (eval/casts: the same results).

**Remainder** takes the sign of the dividend. In the slice only with a
literal divisor other than 0 and -1, so it never raises.

* T-SQL: "% (Modulus) (Transact-SQL)". PostgreSQL: Table 9.4, "Modulo
  (remainder)". Probed (eval/casts: `7 % -2` is 1, `-7 % 2` is -1 on
  both).
* Why the divisor is restricted: `-2147483648 % -1` is 0 in PostgreSQL
  (`int4mod` special-cases -1) and not probed in SQL Server, and `x % 0`
  is not probed in either. With a variable divisor `%` stays on the C1
  path (`bin:%`).

**Negation** `-x` of an `int` is `int`; `-(-2147483648)` overflows.
T-SQL: "- (Negative) (Transact-SQL)"; PostgreSQL: Table 9.4 (`- integer`)
and 8.1.1. Not probed (the parser folds `-5` into a literal; this is
`-x` on a variable or an expression).

*Precedence.* The rule applies to the tree the parser builds, so the
parser must bind unary minus as each dialect does. T-SQL puts unary minus
at the level of binary `+` and `-`, below `*`, `/` and `%` ("Operator
precedence (Transact-SQL)": level 2 is `*` (Multiplication), `/`
(Division), `%` (Modulus); level 3 is `+` (Positive), `-` (Negative), `+`
(Addition), `+` (Concatenation), `-` (Subtraction), `&`, `^`, `|`; from
memory), so T-SQL `-@a * @b` is `-(@a * @b)`. PostgreSQL binds unary
minus tighter than `*` (4.1.6 "Operator Precedence", Table 4.2: unary
`+ -` above `* / %`), so `-a * b` is `(-a) * b`. The two trees differ in
when they overflow: on SQL Server `-@a * @b` with `@a = -2147483648` and
`@b = 0` returns 0 (probed, 2026-10-28), where `(-@a) * @b` would raise
8115 on the negation. The Rust parser reads T-SQL `-@a * @b` as
`-(@a * @b)` (`sqldiff_syntax`, being fixed at the time of writing) and
PostgreSQL `-a * b` as `(-a) * b`, so a conversion that copies the text
unchanged gives two different trees and is refused; `-(a * b)` on the
PostgreSQL side is confirmed. The negation rule itself needs no change:
it is the same in both dialects once the tree is right.

**Comparisons** `= <> < > <= >=` of two `int`s are three-valued: UNKNOWN
(NULL) when either side is NULL. The NULL literal as an operand is outside
the slice (`x = NULL` is UNKNOWN under the default settings, but PostgreSQL's
`transform_null_equals` and T-SQL's `ANSI_NULLS OFF` change it).

* T-SQL: "Comparison Operators (Transact-SQL)" and "SET ANSI_NULLS
  (Transact-SQL)": with `ANSI_NULLS ON`, a comparison with a NULL value
  is UNKNOWN. A routine keeps the `ANSI_NULLS` setting it was created with
  ("SET ANSI_NULLS": "the setting ... at the time the stored procedure is
  created"); `ON` is assumed (`ASM-TSQL-ANSI-NULLS` in a C2a answer that
  has `=` or `<>`). `!=` arrives as `<>`. Not probed.
* PostgreSQL: "Ordinary comparison operators yield null (signifying
  “unknown”), not true or false, when either input is null" (9.2
  "Comparison Functions and Operators"). Not probed.

**`AND`, `OR`, `NOT`** follow the three-valued truth tables: `AND` is
FALSE if either side is FALSE, else UNKNOWN if either is UNKNOWN; `OR`
dually; `NOT UNKNOWN` is UNKNOWN. T-SQL: "AND (Transact-SQL)", "OR
(Transact-SQL)", "NOT (Transact-SQL)" (their result tables);
PostgreSQL: 9.1 "Logical Operators" (the truth table). Operands that can
raise put the expression outside the slice (evaluation order, above).
Not probed.

**`IS NULL`, `IS NOT NULL`** of an `int` are TRUE or FALSE, never
UNKNOWN. T-SQL: "IS NULL (Transact-SQL)"; PostgreSQL: 9.2. Not probed.

**`COALESCE(x1, ..., xn)`**, n ≥ 2, every argument `int` or the NULL
literal and at least one `int`: the first argument that is not NULL, NULL
if all are; an argument after the first non-NULL one is not evaluated.
Lowered as `coalesce x1 (coalesce x2 ...)`, which means exactly that.

* T-SQL: "COALESCE (Transact-SQL)": the result type is "the data type of
  expression with the highest data type precedence", `int` here; "the
  COALESCE expression is a syntactic shortcut for the CASE expression"
  (`CASE WHEN x1 IS NOT NULL THEN x1 ... ELSE xn END`), so it evaluates
  in order and stops; `x1` may be evaluated twice, which changes nothing
  for an expression without side effects; all-NULL-literal arguments are
  an error, hence "at least one `int`". Not probed for `int` (eval/
  bit-boolean probes `bit` and the type rule).
* PostgreSQL: 9.18.2 "COALESCE": "Like a CASE expression, COALESCE only
  evaluates the arguments that are needed to determine the result"; the
  result type by 10.5 "UNION, CASE, and Related Constructs": `integer`
  for `integer` and unknown-type NULL arguments. Not probed.

**T-SQL `ISNULL(x, y)`** with `x` an `int` expression (not the NULL
literal), `y` an `int` expression or the NULL literal, and `y` unable to
raise (`SExpr.mayRaise`): the same core term as `COALESCE(x, y)`. "ISNULL
(Transact-SQL)": "Returns the same type as check_expression" and
replaces NULL with replacement_value, which "must be of a type that is
implicitly convertible to the type of check_expression". With `x` and
`y` both `int` there is no conversion, so the value is `x` if not NULL,
else `y`: what `COALESCE` gives. The page does not say whether `y` is
evaluated when `x` is not NULL; when `y` cannot raise and has no side
effects that is not observable, hence the precondition. (With other
types `ISNULL` converts `y` to `x`'s type and `COALESCE` does not:
`ASM-ISNULL-TYPE`, outside the slice.) Not probed for `int`
(eval/bit-boolean shows the type rule with `bit`).

**Searched `CASE WHEN c1 THEN r1 ... [ELSE e] END`**, every `ci` a
condition of the slice, every `ri` and `e` an `int` or the NULL literal,
at least one `int`: the first `ri` whose `ci` is TRUE (FALSE and UNKNOWN
go on), else `e`, else NULL. Lowered as `cond c1 r1 (cond c2 r2 ...)`.

* T-SQL: "CASE (Transact-SQL)": "Evaluates, in the order specified,
  Boolean_expression for each WHEN clause" and returns the
  result_expression of the first that is TRUE, else the ELSE value, else
  NULL; "The CASE expression evaluates its conditions sequentially and
  stops with the first condition whose condition is satisfied" (the
  exceptions it lists are aggregates, outside the slice); the result type
  is the highest-precedence type, `int` here. Not probed.
* PostgreSQL: 9.18.1 "CASE" (same rule; the constant-folding caveat is
  handled above) and 10.5 for the type. Not probed.
* The simple form `CASE x WHEN v THEN ...` stays on the C1 path.

**`IF` and `WHILE`** run their statements only when the condition is
TRUE: FALSE and UNKNOWN take the `ELSE` branch or leave the loop
(`Sqldiff.Core.isTrue`). T-SQL: "IF...ELSE (Transact-SQL)" and "WHILE
(Transact-SQL)" (the statement runs when the Boolean_expression is TRUE).
PostgreSQL: PL/pgSQL "Control Structures", "Conditionals" ("The
statements between THEN and END IF will be executed if the condition is
true. Otherwise, they are skipped") and "Simple Loops" (`WHILE` repeats
"so long as the boolean-expression evaluates to true"). Not probed.

**An error ends the routine.** PostgreSQL: "By default, any error
occurring in a PL/pgSQL function aborts execution of the function and the
surrounding transaction" (PL/pgSQL "Control Structures", "Trapping
Errors"). T-SQL only under `SET XACT_ABORT ON` (`lowerSetting`). The
caller observes the error's kind, not its number (decision 1; overflow
and division by zero are both `arithmetic`). Not probed in a routine
(eval/casts probes the errors in plain `SELECT`s). -/

/-! ## The C2b slice: strings

C2b adds character strings to the slice: variables and parameters of a
string type, string literals, concatenation, `=` and `<>` (under a
trusted collation fact only), `RTRIM`, `LEN`, `IS [NULL]`, and
`COALESCE`, `ISNULL` and `CASE` over strings. Each rule states its claim,
the vendor text, and its probe status, as for C2a. **Every vendor quote
in this section was written from memory**: the vendor pages could not be
reached when it was written (2026-10-28), so reviewers check each quote
against the page named before this section is relied on; "from memory"
below marks the ones the author is least sure of. A value is a sequence
of characters (`Sqldiff.Core.Val.str`); the empty string is not NULL in
either dialect (incidentally probed: eval/casts/assign, `''` into an
`int` column is 0 in SQL Server and an input error in PostgreSQL, not
NULL). Strings never meet integers in the slice: an operator,
assignment or `COALESCE` that would convert between them is outside it.

**Types, T-SQL.** `varchar(n)` (also spelt `char varying(n)`,
`character varying(n)`) and `nvarchar(n)`, `1 ≤ n ≤ 4000`. For `varchar`
`n` is "the string size in bytes" ("char and varchar (Transact-SQL)"),
for `nvarchar` "the string size in byte-pairs" ("nchar and nvarchar
(Transact-SQL)"); a character takes at least one byte or byte-pair, so
`n` also bounds the number of characters, and a `varchar` value holds
only characters of the code page of the database's collation. The cap of
4000 (not 8000 for `varchar`) keeps every value of the slice under both
concatenation limits below. Outside: `max`, no length (it means 1 in a
declaration: "When n isn't specified in a data definition or variable
declaration statement, the default length is 1"), `char(n)`/`nchar(n)`
(blank-padded), `sysname`, `text`/`ntext`. Probed: eval/casts
(`CAST('abcdef' AS varchar(3))` is `abc`, `CAST(N'abcdef' AS
nvarchar(3))` is `abc`; `CAST(N'éééé' AS varchar(3))` is `ééé` under the
probe server's single-byte collation).

**Types, PostgreSQL.** `varchar(n)`, `character varying(n)`, `char
varying(n)`, `1 ≤ n ≤ 10485760`: "strings up to n characters (not
bytes) in length"; `varchar` without `n` and `text`: "the type accepts
strings of any size" (8.3 "Character Types"). Outside: `char(n)`,
`bpchar` and PostgreSQL `N'..'` (type `character`, blank-padded; the Rust
parser refuses those literals), `name`, `citext`. A user type named
`text` or `varchar` earlier in `search_path` than `pg_catalog` would
shadow these names, as for `int4` above (assumed not to exist). Not
probed as variable types.

**PostgreSQL parameters carry no length.** "Parenthesized type modifiers
(e.g., the precision field for type numeric) are discarded by CREATE
FUNCTION" (CREATE FUNCTION, Notes; from memory, and that CREATE PROCEDURE
does the same): a `varchar(10)` parameter holds any string. So a
PostgreSQL parameter is read as having no length bound (`envP`), and a
write to one is still held to its declared length (`envPDecl`), which is
sound whether or not the modifier is kept. Not probed.

**T-SQL parameters truncate.** A string argument longer than the
parameter is silently truncated (probed: eval/casts/assign, `EXEC #q
'abcdef'` with `@x varchar(3)` sees `abc`), and a `varchar` parameter
replaces characters outside its code page; PostgreSQL does neither. The
core binds arguments as they are, so the T-SQL lowering is faithful only
for arguments that fit: `ASM-TSQL-STRING-ARGS` (`Sqldiff.Mirror.residual`)
is the caller's promise that every string argument of a T-SQL string
parameter (including the value passed to an `OUTPUT` parameter) has at
most `n` bytes (`varchar(n)`) or byte-pairs (`nvarchar(n)`) and, for
`varchar`, only characters of the database's code page. Strings passed
to a call, and `OUTPUT` arguments that write back into a string
variable, are outside the slice (the conversions at the callee's
boundary are the callee's and the dialects differ there).

**Assignment converts, differently.** T-SQL truncates a string that is
too long for its target, silently (probed: eval/casts/assign, `DECLARE
@v varchar(3) = 'abcdef'` gives `abc`; "char and varchar": "values that
are too long for the new data type are truncated"), and replaces
characters a `varchar` cannot hold (from memory: with `?`); PostgreSQL
raises "value too long for type character varying(3)" (probed:
eval/casts/assign, `DECLARE v varchar(3) := 'abcdef'`), or truncates
when the excess characters are all spaces (8.3). So a string is stored
(`SET`/`:=`, a `DECLARE` default, a parameter default, a function's
`RETURN`) only when its bound is at most the target's length
(`fitsTy`), and in T-SQL a value that may hold characters outside the
code page (`wide`) is never stored in a `varchar`: then neither engine
changes the value. A PostgreSQL target without a length takes any
string. A function's declared return length is checked the same way on
both sides (for PostgreSQL that is sound whether or not the modifier is
kept, as for parameters).

**Literals.** `'...'` whose characters are all printable ASCII
(U+0020 to U+007E) and not a backslash, with bound `max 1 length` (an
empty T-SQL literal is typed `varchar(1)`, from memory; the bound only
needs to be at least the value's length). The Rust parser reads T-SQL
`'ab'` and `N'ab'` to the same `Str` (`sqldiff_syntax::abstract_syntax::
string_value`), so a literal must mean the same as either: ASCII
characters are themselves in every code page SQL Server uses and in
Unicode, and take one byte or byte-pair each (from memory; reviewers
check the code pages in use). PostgreSQL: with
`standard_conforming_strings` on (the default) a backslash is an
ordinary character and with it off it escapes (4.1.2.1, 4.1.2.2);
without backslashes both read the same. A doubled quote is one quote in
both. Probed: eval/casts (`N'ab'` is `nvarchar` in T-SQL and `character`
in PostgreSQL).

**Concatenation.** T-SQL `a + b` and PostgreSQL `a || b`, both operands
strings (not the NULL literal): the characters of `a` then those of `b`,
NULL if either is NULL.

* T-SQL: "+ (String Concatenation) (Transact-SQL)"; "If the result of
  the concatenation of strings exceeds the limit of 8,000 bytes, the
  result is truncated" (from memory), so every string expression of the
  slice has a bound of at most 4000, which is under 8000 bytes and 4000
  byte-pairs. NULL: "When SET CONCAT_NULL_YIELDS_NULL is ON,
  concatenating a null value with a string yields a NULL result"; with
  it OFF a NULL operand is an empty string ("SET CONCAT_NULL_YIELDS_NULL
  (Transact-SQL)", from memory). It is a session setting fixed at run time, not
  stored with the routine, so a confirmation that concatenates rests on
  `ASM-CONCAT-NULL`, unless the routine starts with `SET
  CONCAT_NULL_YIELDS_NULL ON` (`lowerSetting`, `concatNullOn`). The
  result type is the operands' type with the higher precedence
  (`nvarchar` over `varchar`: a lossless conversion). T-SQL `||` (SQL
  Server 2025) is outside. Not probed.
* PostgreSQL: 9.4 "String Functions and Operators", `text || text →
  text`; `varchar` operands are `text` (binary coercible); the operator's
  function is strict, so a NULL operand gives NULL (from memory, also for
  the `anynonarray` variants, which the slice never selects since both
  operands are strings). No length limit applies below 1 GB. Not probed.

**`=` and `<>` on strings**, only when the catalog says
`binaryCollation` (trusted; `ASM-BINARY-COLLATION` in the answer's
residual):

* T-SQL: variables, parameters and literals take the database's default
  collation ("Collation precedence (Transact-SQL)": coercible-default),
  which the fact says is `_BIN` or `_BIN2`: characters compare by code
  point (BIN2) or, for equality, by identity (BIN). Before comparing,
  SQL Server pads the shorter operand with spaces, whatever the
  collation, so `'a' = 'a '` is TRUE ("SQL Server follows the ANSI/ISO
  SQL-92 specification ... on how to compare strings with spaces",
  Microsoft support article "How SQL Server compares strings with
  trailing spaces"; from memory). Two strings are equal after padding
  exactly when they are equal without their trailing spaces, so T-SQL
  `a = b` is `streq (rtrim a) (rtrim b)`. A `varchar` operand is
  converted to `nvarchar` when the other is one, which keeps its
  characters. Not probed.
* PostgreSQL: on `text` and `varchar`, trailing spaces are
  "semantically significant" (8.3, from memory), and under a deterministic collation
  strings are equal "only if they consist of the same byte sequence"
  ("Collation Support", "Nondeterministic Collations"): `a = b` is
  `streq a b`. A
  PL/pgSQL parameter compares under the collation of the caller's
  argument ("Collation of PL/pgSQL Variables"), which is why the fact
  covers callers. Not probed.
* So T-SQL `@a = @b` matches PostgreSQL `rtrim(a) = rtrim(b)` and not
  `a = b`. `<>` is `NOT (... = ...)` on both sides. `mkRtrim` folds the
  `rtrim` of a literal and of an `rtrim` (`mkRtrim_eval`), so
  `@a = 'x'` matches `rtrim(a) = 'x'`. `ANSI_NULLS` applies as for
  integers (`ASM-TSQL-ANSI-NULLS`). `<`, `>`, `LIKE`, and every
  comparison under a case- or accent-insensitive collation (T-SQL's
  default) are outside the slice: their result depends on collation
  tables the core does not have (`ASM-CI-COLLATION` on the Rust side).

**`RTRIM(x)`**, one string argument: `x` without its trailing spaces
(U+0020 only). T-SQL: "RTRIM (Transact-SQL)": "Returns a character
string after truncating all trailing spaces" (from memory; the two-argument form of
SQL Server 2022 is outside). PostgreSQL: 9.4, `rtrim(string text [,
characters text])`: "Removes the longest string containing only
characters in characters (a space by default) from the end of string"
(one argument only). Not probed.

**`LEN(x)`** (T-SQL) and **`length(x)`**, `char_length`,
`character_length` (PostgreSQL), one string argument, an `int`. T-SQL:
"LEN (Transact-SQL)": "Returns the number of characters of the specified
string expression, excluding trailing spaces", and "If you use SC
collations, the returned integer value counts UTF-16 surrogate pairs as
a single character" (otherwise as two; from memory): lowered `len (rtrim x)`, which
counts code points, so a confirmation that uses `LEN` rests on
`ASM-TSQL-LEN-BMP` (no character outside the Basic Multilingual Plane
reaches `LEN`, or the collation is supplementary-character aware).
PostgreSQL: 9.4, `length(text)` "Returns the number of characters in the
string": lowered `len x`. So T-SQL `LEN(x)` matches PostgreSQL
`length(rtrim(x))`, which the converter writes. Not probed (eval/casts
uses `LEN` only to measure strings of `x`).

**`COALESCE`, `CASE`** over strings: as for integers, every value
argument a string or the NULL literal, at least one a string; the value
is the chosen argument's, unchanged. T-SQL: the result type is the
argument type of the highest precedence and, for strings of one type,
the largest length (from memory: the pages state the precedence, not
the length), so no argument is truncated. PostgreSQL: 10.5 resolves
`text`/`varchar`/unknown arguments to `text` or `varchar`, without a
length. The bound is the largest argument's bound. Not probed.

**T-SQL `ISNULL(x, y)`** over strings: "Returns the same type as
check_expression", so `y` is converted to `x`'s type and a longer `y` is
truncated ("ISNULL (Transact-SQL)": "replacement_value can be truncated
if replacement_value is longer than check_expression", from memory;
`ASM-ISNULL-TYPE` on the Rust side).
In the slice only when `x` is a variable or parameter, whose type is its
declaration (`varchar(n)`/`nvarchar(n)`), and `y` is the NULL literal or
a string that cannot raise, whose bound is at most `n`, and, when `x` is
a `varchar`, that is not `wide`: then the conversion keeps `y` as it is
and `ISNULL(x, y)` is `COALESCE(x, y)`. The result has `x`'s type. Not
probed for strings.

**`IS [NOT] NULL`** of a string: TRUE or FALSE, as for integers. -/

/-- Digits as a number; nothing else (no sign, no `MAX`). -/
def natArg? (s : String) : Option Nat :=
  if s.isEmpty then none else digitsVal s.toList 0

/-- The largest length of a T-SQL string in the slice. -/
def tsqlMaxLen : Nat := 4000

/-- The largest `n` of PostgreSQL `varchar(n)`. -/
def pgMaxLen : Nat := 10485760

/-- A T-SQL string type: its length and whether it is `nvarchar`. -/
def strTypeT : Ast.Ty → Option (Option Nat × Bool)
  | .Ty n [a] =>
    let u := n.toUpper
    (natArg? a).bind fun k =>
      if 1 ≤ k ∧ k ≤ tsqlMaxLen then
        if u == "VARCHAR" || u == "CHAR VARYING" || u == "CHARACTER VARYING" then some (some k, false)
        else if u == "NVARCHAR" then some (some k, true)
        else none
      else none
  | _ => none

def pgVarchar (u : String) : Bool :=
  u == "VARCHAR" || u == "CHAR VARYING" || u == "CHARACTER VARYING"

/-- A PostgreSQL string type: its length (`none`: any), never `wide`. -/
def strTypeP : Ast.Ty → Option (Option Nat × Bool)
  | .Ty n [] =>
    let u := n.toUpper
    if u == "TEXT" || pgVarchar u then some (none, false) else none
  | .Ty n [a] =>
    if pgVarchar n.toUpper then
      (natArg? a).bind fun k => if 1 ≤ k ∧ k ≤ pgMaxLen then some (some k, false) else none
    else none
  | _ => none

/-- The declared type is a string type of the slice: its length bound and
whether it is `wide`. -/
def strType : Dialect → Ast.Ty → Option (Option Nat × Bool)
  | .tsql _ _, t => strTypeT t
  | .pg _ _, t => strTypeP t

def strVar (d : Dialect) (env : TyEnv) (x : Var) : Option (Option Nat × Bool) :=
  (lookup env x).bind (strType d)

/-- The type of a variable of the slice: the integer type or a string
type (C2b). -/
def varTy (d : Dialect) (env : TyEnv) (x : Var) : Option STy :=
  if intVar d env x then some .int
  else (strVar d env x).map fun (n, w) => .str n w

/-- A string literal of the slice: printable ASCII, no backslash. -/
def strLitOk (s : String) : Bool :=
  s.toList.all fun c => ' ' ≤ c && c ≤ '~' && c != '\\'

def addB : Option Nat → Option Nat → Option Nat
  | some a, some b => some (a + b)
  | _, _ => none

def maxB : Option Nat → Option Nat → Option Nat
  | some a, some b => some (max a b)
  | _, _ => none

/-- A T-SQL string expression must stay within `tsqlMaxLen`. -/
def boundOk (d : Dialect) (n : Option Nat) : Bool :=
  match d, n with
  | .tsql _ _, some k => k ≤ tsqlMaxLen
  | .tsql _ _, none => false
  | .pg _ _, _ => true

/-- A value whose bound is `m` fits a target whose length is `n`. -/
def fitsLen : Option Nat → Option Nat → Bool
  | none, _ => true
  | some n, some m => m ≤ n
  | some _, none => false

/-- `rtrim`, folded on a literal and on an `rtrim`
(`Sqldiff.Mirror.mkRtrim_eval`: the same meaning). Both lowerings use it,
so a trimmed literal is a literal on both sides. -/
def mkRtrim : SExpr → SExpr
  | .str s => .str (Core.rtrimSp s)
  | .rtrim a => .rtrim a
  | a => .rtrim a

theorem dropWhile_idem {α : Type} (p : α → Bool) (l : List α) :
    (l.dropWhile p).dropWhile p = l.dropWhile p := by
  induction l with
  | nil => rfl
  | cons x xs ih =>
    by_cases h : p x
    · simp [h, ih]
    · simp [h]

theorem rtrimSp_idem (s : String) : Core.rtrimSp (Core.rtrimSp s) = Core.rtrimSp s := by
  simp [Core.rtrimSp, dropWhile_idem]

theorem mkRtrim_eval {U : Type} (σ ρ : Core.Store U) (a : SExpr) :
    (mkRtrim a).eval σ ρ = (SExpr.rtrim a).eval σ ρ := by
  cases a with
  | str s => rfl
  | rtrim b =>
    simp only [mkRtrim, SExpr.eval]
    cases b.eval σ ρ with
    | err k => rfl
    | ok v => cases v <;> simp [rtrimSp_idem, Core.typeErr]
  | _ => rfl

/-- The type `COALESCE` or `CASE` gives two value arguments, or `none`
when they do not go together (an integer and a string, or a boolean). -/
def joinTy : STy → STy → Option STy
  | .nul, .nul => some .nul
  | .nul, .int | .int, .nul | .int, .int => some .int
  | .nul, .str n w | .str n w, .nul => some (.str n w)
  | .str n w, .str m v => some (.str (maxB n m) (w || v))
  | _, _ => none

/-- A value type: an integer, a string or the NULL literal. -/
def isVal : STy → Bool
  | .bool => false
  | _ => true

/-- A value of type `vt` can be stored in a variable, parameter or result
declared `ty` without a conversion the core does not show: an integer
(or NULL) into the slice's integer type; a string (or NULL) that fits
into a string type ("Assignment converts, differently"). -/
def fitsTy (d : Dialect) (ty : Ast.Ty) (vt : STy) : Bool :=
  if intType d ty then vt == .int || vt == .nul
  else match strType d ty, vt with
    | some _, .nul => true
    | some (n, wt), .str m w => fitsLen n m && (wt || !w)
    | _, _ => false

def arithOp? : String → Option AOp
  | "+" => some .add
  | "-" => some .sub
  | "*" => some .mul
  | "/" => some .div
  | "%" => some .mod
  | _ => none

def cmpOp? : String → Option COp
  | "=" => some .eq
  | "<>" => some .ne
  | "<" => some .lt
  | ">" => some .gt
  | "<=" => some .le
  | ">=" => some .ge
  | _ => none

/-- An expression without variables (and without columns) is in the slice
only if evaluating it does not fail (constant subexpressions, above). -/
def closedSafe (s : SExpr) : Bool :=
  !s.vars.isEmpty || s.hasCol ||
    match s.eval (U := Unit) Core.nullStore Core.nullStore with
    | .err _ => false
    | .ok _ => true

def guardC (r : Option (SExpr × STy)) : Option (SExpr × STy) :=
  r.bind fun p => if closedSafe p.1 then some p else none

/-- String `=` or `<>` (C2b), given the operands as each side compares
them. -/
def eqForm (o : String) (e : SExpr) : Option (SExpr × STy) :=
  if o == "=" then some (e, .bool)
  else if o == "<>" then some (.not e, .bool)
  else none

def binTy (d : Dialect) (o : String) : SExpr × STy → SExpr × STy → Option (SExpr × STy)
  | (a, .int), (b, .int) =>
    match arithOp? o, cmpOp? o with
    | some .mod, _ => match b with
      | .int k => if k = 0 ∨ k = -1 then none else some (.arith .mod a b, .int)
      | _ => none
    | some op, _ => some (.arith op a b, .int)
    | none, some c => some (.cmp c a b, .bool)
    | none, none => none
  | (a, .bool), (b, .bool) =>
    if a.mayRaise || b.mayRaise then none
    else if o.toUpper == "AND" then some (.and a b, .bool)
    else if o.toUpper == "OR" then some (.or a b, .bool)
    else none
  | (a, .str n w), (b, .str m v) =>
    match d with
    | .tsql _ Γ =>
      if o == "+" then
        if boundOk d (addB n m) then some (.concat a b, .str (addB n m) (w || v)) else none
      else if Γ.binaryCollation then eqForm o (.streq (mkRtrim a) (mkRtrim b))
      else none
    | .pg Γ _ =>
      if o == "||" then some (.concat a b, .str (addB n m) false)
      else if Γ.binaryCollation then eqForm o (.streq a b)
      else none
  | _, _ => none

def unTy (o : String) : SExpr × STy → Option (SExpr × STy)
  | (a, .int) => if o == "-" then some (.neg a, .int) else none
  | (a, .bool) => if o.toUpper == "NOT" then some (.not a, .bool) else none
  | _ => none

/-- The type of a list of value arguments (`COALESCE`, `CASE` results),
starting from the NULL literal's. -/
def joinAll : List STy → Option STy
  | [] => some .nul
  | t :: ts => (joinAll ts).bind (joinTy t)

/-- A value type with a value: an integer or a string, not only NULL. -/
def typedVal : STy → Bool
  | .int | .str .. => true
  | _ => false

/-- `coalesce x1 (coalesce x2 ... xn)`. -/
def coalesceBuild : List SExpr → Option SExpr
  | [] => none
  | [a] => some a
  | a :: rest => (coalesceBuild rest).map (.coalesce a)

/-- A function call of the slice. `raw` are the arguments as written
(T-SQL `ISNULL` over strings asks its first one to be a variable). -/
def callTy (d : Dialect) (name : String) (raw : List Ast.Expr) (args : List (SExpr × STy)) :
    Option (SExpr × STy) :=
  match name.toUpper, args, d with
  | "COALESCE", _, _ =>
    if args.length ≥ 2 then
      (joinAll (args.map (·.2))).bind fun t =>
        if typedVal t then (coalesceBuild (args.map (·.1))).map (·, t) else none
    else none
  | "ISNULL", [(x, .int), (y, ty)], .tsql _ _ =>
    if valTy ty && !y.mayRaise then some (.coalesce x y, .int) else none
  | "ISNULL", [(x, .str (some n) wx), (y, ty)], .tsql _ _ =>
    let isVar := match raw with
      | .Var _ :: _ => true
      | _ => false
    let yOk := match ty with
      | .nul => true
      | .str m wy => fitsLen (some n) m && (wx || !wy)
      | _ => false
    if isVar && yOk && !y.mayRaise then some (.coalesce x y, .str (some n) wx) else none
  | "RTRIM", [(x, .str n w)], _ => some (mkRtrim x, .str n w)
  | "LEN", [(x, .str _ _)], .tsql _ _ => some (.len (mkRtrim x), .int)
  | "LENGTH", [(x, .str _ _)], .pg _ _ => some (.len x, .int)
  | "CHAR_LENGTH", [(x, .str _ _)], .pg _ _ => some (.len x, .int)
  | "CHARACTER_LENGTH", [(x, .str _ _)], .pg _ _ => some (.len x, .int)
  | _, _, _ => none

/-- `cond c1 r1 (cond c2 r2 ... e)`. -/
def caseBuild : List ((SExpr × STy) × (SExpr × STy)) → SExpr → Option SExpr
  | [], e => some e
  | ((c, .bool), (r, tr)) :: ws, e => if isVal tr then (caseBuild ws e).map (.cond c r) else none
  | _, _ => none

def caseTy (ws : List ((SExpr × STy) × (SExpr × STy))) (els : Option (SExpr × STy)) :
    Option (SExpr × STy) :=
  let tys := ws.map (·.2.2) ++ (els.map (·.2)).toList
  if ws.isEmpty then none
  else (joinAll tys).bind fun t =>
    if typedVal t then (caseBuild ws ((els.map (·.1)).getD .null)).map (·, t) else none

mutual
  /-- The slice form of an expression and its type, or `none` when the
  expression is outside the slice. A variable is in the slice when it is
  declared with the slice's integer type (`env`). -/
  def typed (d : Dialect) (env : TyEnv) (sc : Scope) : Ast.Expr → Option (SExpr × STy)
    | .Var v => match d with
      | .tsql _ _ => (varTy d env v).map (.var v, ·)
      | .pg _ _ => none
    | .Name [x] => match resolveName d sc x with
      | some (.inl c) => some (.col c, .int)
      | some (.inr v) => (varTy d env v).map (.var v, ·)
      | none => none
    | .Num s => (intLit? s).map fun n => (.int n, .int)
    | .Str s => if strLitOk s then some (.str s, .str (some (max 1 s.length)) false) else none
    | .Null => some (.null, .nul)
    | .Bin o a b => do
      let a' ← typed d env sc a
      let b' ← typed d env sc b
      guardC (binTy d o a' b')
    | .Unary o a => do
      let a' ← typed d env sc a
      guardC (unTy o a')
    | .IsNull x isNot => do
      let (x', tx) ← typed d env sc x
      if typedVal tx then guardC (some (.isNull x' isNot, .bool)) else none
    | .CallE (.CallF [n] args false [] false false false) => do
      let args' ← typedList d env sc args
      guardC (callTy d n args args')
    | .Case none whens els => do
      let ws ← typedWhens d env sc whens
      let e ← typedOpt d env sc els
      guardC (caseTy ws e)
    | _ => none

  def typedList (d : Dialect) (env : TyEnv) (sc : Scope) :
      List Ast.Expr → Option (List (SExpr × STy))
    | [] => some []
    | e :: es => do
      let e' ← typed d env sc e
      let es' ← typedList d env sc es
      pure (e' :: es')

  def typedOpt (d : Dialect) (env : TyEnv) (sc : Scope) :
      Option Ast.Expr → Option (Option (SExpr × STy))
    | none => some none
    | some e => (typed d env sc e).map some

  def typedWhens (d : Dialect) (env : TyEnv) (sc : Scope) :
      List Ast.When2 → Option (List ((SExpr × STy) × (SExpr × STy)))
    | [] => some []
    | .When2 c r :: ws => do
      let c' ← typed d env sc c
      let r' ← typed d env sc r
      let ws' ← typedWhens d env sc ws
      pure ((c', r') :: ws')
end

/-! ## Expressions

An expression of the slice lowers to its slice form (`typed`). Every
other expression is the C1 path: a variable read, an integer literal and
NULL are as in the slice; any other form is an operator of the core,
named by its syntactic form and its non-expression fields (`bin:+`,
`un:-`, `fn:coalesce`, `isnull:false`, `cast:int()`, `str:abc`,
`num:2147483648`, ...), applied to its sub-expressions, each lowered the
same way; distinct forms get distinct names, so two expressions outside
the slice lower to the same term exactly when they are the same
expression up to the renaming of variables.

The core leaves those operators uninterpreted, so a confirmation that
uses one is relative to **the same operator name meaning the same function
in both dialects** (`ASM-C1-OPERATORS`). T-SQL `@a + @b` and PostgreSQL
`a + b` on `varchar` variables agree on the C1 path; whether they agree
is later C2 work. Expressions that read data (subqueries, `EXISTS`,
`IN (SELECT ...)`, `*`, aggregates and window calls) are C3. -/

def flag (b : Bool) : String := if b then "1" else "0"

/-- T-SQL functions that read the error a `CATCH` handles, or the
transaction state (C4): outside the spec. -/
def errorFns : List String :=
  ["ERROR_NUMBER", "ERROR_MESSAGE", "ERROR_SEVERITY", "ERROR_STATE", "ERROR_LINE",
   "ERROR_PROCEDURE", "XACT_STATE"]

def tyName : Ast.Ty → String
  | .Ty n args => n.toLower ++ "(" ++ ",".intercalate args ++ ")"

/-- The slice form of `e` if it has one, else `c1`. -/
def withSlice (d : Dialect) (env : TyEnv) (sc : Scope) (e : Ast.Expr) (c1 : Option Core.Expr) :
    Option Core.Expr :=
  match typed d env sc e with
  | some (s, _) => some (.sc s)
  | none => c1

mutual
  def lowerExpr (d : Dialect) (env : TyEnv) (sc : Scope) : Ast.Expr → Option Core.Expr
    | .Var v => match d with
      | .tsql _ _ => if v.startsWith "@@" then none else some (.var v) -- `@@ERROR` etc.: C4
      | .pg _ _ => none
    | .Name [x] => match resolveName d sc x with
      | some (.inl c) => some (.sc (.col c))
      | some (.inr v) => some (.var v)
      | none => none
    | .Name _ => none
    | .Num s => some (match intLit? s with
      | some n => .lit n
      | none => .op ("num:" ++ s) [])
    | .Str s => withSlice d env sc (.Str s) (some (.op ("str:" ++ s) []))
    | .Null => some .null
    | .Star _ => none
    | .CallE (.CallF n args distinct order withinGroup over star) =>
      -- C4: the error functions are outside the spec ("The C4 rules").
      if (d matches .tsql _ _) && errorFns.contains (dotted n).toUpper then none else
      withSlice d env sc (.CallE (.CallF n args distinct order withinGroup over star))
        (if distinct || !order.isEmpty || withinGroup || over || star then none
        else (lowerExprs d env sc args).map (.op ("fn:" ++ (dotted n).toLower)))
    | .Bin o a b => withSlice d env sc (.Bin o a b) do
      let a' ← lowerExpr d env sc a
      let b' ← lowerExpr d env sc b
      pure (.op ("bin:" ++ o.toLower) [a', b'])
    | .Unary o a => withSlice d env sc (.Unary o a)
      ((lowerExpr d env sc a).map fun a' => .op ("un:" ++ o.toLower) [a'])
    | .IsNull x neg => withSlice d env sc (.IsNull x neg)
      ((lowerExpr d env sc x).map fun x' => .op ("isnull:" ++ flag neg) [x'])
    | .InList x xs neg => do
      let x' ← lowerExpr d env sc x
      let xs' ← lowerExprs d env sc xs
      pure (.op ("in:" ++ flag neg) (x' :: xs'))
    | .InQuery .. => none
    | .Between x lo hi neg => do
      let x' ← lowerExpr d env sc x
      let lo' ← lowerExpr d env sc lo
      let hi' ← lowerExpr d env sc hi
      pure (.op ("between:" ++ flag neg) [x', lo', hi'])
    | .Like x p neg ci => do
      let x' ← lowerExpr d env sc x
      let p' ← lowerExpr d env sc p
      pure (.op ("like:" ++ flag neg ++ flag ci) [x', p'])
    | .Cast x ty => (lowerExpr d env sc x).map fun x' => .op ("cast:" ++ tyName ty) [x']
    | .Case operand whens els => withSlice d env sc (.Case operand whens els) do
      -- `case:OE`: whether there is an operand and an `ELSE`; the `WHEN`
      -- pairs fill the arguments between them.
      let o' ← lowerOpt d env sc operand
      let ws ← lowerWhens d env sc whens
      let e' ← lowerOpt d env sc els
      pure (.op ("case:" ++ flag o'.isSome ++ flag e'.isSome) (o'.toList ++ ws ++ e'.toList))
    | .Exists _ | .Subquery _ => none
    | .Tuple xs => (lowerExprs d env sc xs).map (.op "tuple")
    | .Special n => some (.op ("special:" ++ n.toLower) [])
    | .Extract f x => (lowerExpr d env sc x).map fun x' => .op ("extract:" ++ f.toLower) [x']

  def lowerExprs (d : Dialect) (env : TyEnv) (sc : Scope) : List Ast.Expr → Option (List Core.Expr)
    | [] => some []
    | e :: es => do
      let e' ← lowerExpr d env sc e
      let es' ← lowerExprs d env sc es
      pure (e' :: es')

  /-- An optional part: `some none` when absent, `none` when unsupported. -/
  def lowerOpt (d : Dialect) (env : TyEnv) (sc : Scope) :
      Option Ast.Expr → Option (Option Core.Expr)
    | none => some none
    | some e => (lowerExpr d env sc e).map some

  def lowerWhens (d : Dialect) (env : TyEnv) (sc : Scope) : List Ast.When2 → Option (List Core.Expr)
    | [] => some []
    | .When2 c r :: ws => do
      let c' ← lowerExpr d env sc c
      let r' ← lowerExpr d env sc r
      let ws' ← lowerWhens d env sc ws
      pure (c' :: r' :: ws')
end

/-- The variable an assignment or an `OUTPUT` argument names. -/
def target : Dialect → Ast.Expr → Option Var
  | .tsql _ _, .Var v => if v.startsWith "@@" then none else some v
  | .pg _ _, .Name [x] => some (pgName x)
  | _, _ => none

/-! ## The T-SQL return status

A T-SQL procedure returns an integer status to its caller (`EXEC @rc =
p`): the value of `RETURN e`, and 0 when it ends by `RETURN` without a
value or by reaching the end; `RETURN NULL` returns 0 with a warning
(Microsoft Learn, "RETURN (Transact-SQL)": "If no value is specified, 0 is
returned"; "Procedures ... cannot return a null value. If a procedure
tries to return a null value ... a warning message is generated and a
value of 0 is returned").

In the core the status is one more parameter, last, `inout`, named
`statusVar` (a name no T-SQL variable can have): the body starts with
`status := 0`, and `RETURN e` is `status := coalesce(e, 0); return`, with
the slice's `coalesce` when `e` is in the slice and the C1 operator
`fn:coalesce` otherwise. So a caller observes the status as the last out
value. Its entry value is never read, so its mode and default have no
effect; they are `inout` and `NULL` to match the converter's `INOUT
return_code integer DEFAULT NULL`. The PL/pgSQL mirror must therefore set
`return_code := 0` first and write `return_code := COALESCE(e, 0)`: a
bare `return_code := e` differs when `e` is NULL, and without the first
assignment a procedure that ends without `RETURN e` leaves the caller's
value (NULL by default), not 0. -/

def statusVar : Var := "%status"

def statusParam : Core.Param := ⟨statusVar, .inout, some .null⟩

def statusExpr (d : Dialect) (env : TyEnv) (e : Ast.Expr) (e' : Core.Expr) : Core.Expr :=
  match typed d env {} e with
  | some (s, t) => if valTy t then .sc (.coalesce s (.int 0)) else .op "fn:coalesce" [e', .lit 0]
  | none => .op "fn:coalesce" [e', .lit 0]

/-! ## The C3a slice: data statements

Single-table `INSERT ... VALUES`, `UPDATE` and `DELETE` in a procedure,
on a table of the target catalog (`CatTable`), lowered to the core's
`insert`, `update` and `delete` (`Sqldiff.Core.insertRows`,
`updateRows`, `deleteRows`). Expressions are lowered as everywhere else,
with the columns in scope (`Scope`, `resolveName`); a column of the
catalog is an `int` in the slice's sense. Each rule states the claim, the
vendor text and the probe status, as for C2a. "eval/casts/assign" is
`eval/casts/assign-{mssql,postgres}.sql` with their `.out` files, and
"eval/bit-boolean" is `eval/bit-boolean/{mssql,postgres}.sql`; both
write tables only incidentally, to probe conversions, so most rules below
are not probed.

**Tables.** A T-SQL table name, `.`-joined as written (`dbo.t`), reaches
the target table the deployment maps it to (`Deployment.tables`,
`ASM-DBO-SCHEMA`); a PostgreSQL table name is folded (`pgName`) and
`.`-joined, and must be a catalog table of that name (the routine's
`search_path` finds it: the same trust as for `CALL`). A catalog table is
usable only if its column names are ASCII, distinct, and their T-SQL
spellings distinct up to case (`CatTable.wellFormed`). Not probed.

**Where data statements may stand.** Only in a procedure: a T-SQL
function cannot change a table ("CREATE FUNCTION (Transact-SQL)":
user-defined functions "can't be used to perform actions that modify the
database state"), so a T-SQL function with one is not a function SQL
Server accepts, and its PostgreSQL mirror has no counterpart.

**Row counts.** A T-SQL `INSERT`, `UPDATE` or `DELETE` sends its
rows-affected count unless `SET NOCOUNT ON` ("SET NOCOUNT
(Transact-SQL)"); a PL/pgSQL procedure sends none (a `CALL` reports only
its own command tag). So when the deployment's observer reads row counts
(`Deployment.rowCounts`), every T-SQL data statement is outside the spec
(and C1 keeps `SET NOCOUNT` outside then too); when it does not, counts
are not observed (`ASM-OBSERVER-ROWCOUNT`, the deployment's choice).

**`INSERT INTO t (c1, ..., cn) VALUES (e1, ..., en), ...`**: a column
list of distinct columns of `t`, one or more rows of exactly `n` values;
each row gets the values in the listed columns and NULL in every other
column. No query source, `DEFAULT VALUES`, `RETURNING`, `ON CONFLICT` or
column list left out (the order of a table's columns is not in the
correspondence).

* T-SQL: "INSERT (Transact-SQL)": "If a column isn't in column_list, the
  Database Engine must be able to provide a value based on the definition
  of the column ... [it] automatically provides a value for the column if
  the column: ... Is nullable. A null value is used." (the other cases,
  identity, default, `timestamp`, computed, are excluded by the catalog).
  A column without a value that is `NOT NULL` is error 515. Incidentally
  probed: eval/casts/assign (`INSERT INTO #a (dt) VALUES (...)` leaves
  the unlisted `dt2` NULL).
* PostgreSQL: "INSERT": "Each column not present in the explicit or
  implicit column list will be filled with a default value, either its
  declared default value or null if there is none." Not probed.
* The values see no columns: T-SQL refuses a column name in `VALUES`,
  and PostgreSQL resolves it to nothing ("there is a column named ... but
  it cannot be referenced from this part of the query"), so in PL/pgSQL a
  name there is a variable even when the table has a column of that name
  (`resolveName` with the empty scope). Not probed.

**`UPDATE t SET c1 = e1, ... [WHERE p]`**: distinct one-part columns of
`t`, none a key column; no alias, no `FROM` (T-SQL `UPDATE ... FROM`, which
takes an unspecified source row, is outside C3a), no `OUTPUT`/`RETURNING`.
Each row where `p` is TRUE gets every `ei` evaluated on the row's old
values; FALSE and UNKNOWN leave the row as it is.

* All assignments see the old values. PostgreSQL: "UPDATE", `expression`:
  "The expression can use the old values of this and other columns in the
  table." T-SQL: "UPDATE (Transact-SQL)", on `@variable = column =
  expression`: "This differs from SET @variable = column, column =
  expression, which sets the variable to the pre-update value of the
  column", i.e. a `SET` list reads the row as it was before the statement
  (the "all-at-once" rule; the page says it for variables, not in so many
  words for columns: reviewers check). Probed, eval/errors-txn P13: `SET n
  = m, m = n` swaps the two columns on both engines.
* `WHERE` keeps the rows where it is TRUE. PostgreSQL: "UPDATE":
  "Only rows for which this expression returns true will be updated."
  T-SQL: "UPDATE (Transact-SQL)", `WHERE`: "Specifies the conditions that
  limit the rows that are updated", with "Search condition
  (Transact-SQL)" for UNKNOWN. Incidentally probed: eval/bit-boolean
  (`UPDATE #f SET c = 7 WHERE id = 2` changes row 2 only, SQL Server).
* A key column is never set: PostgreSQL checks a non-deferrable primary
  key "immediately whenever a row is inserted or modified", not at the
  end of the statement as the standard says, so `SET k = k + 1` can fail
  part-way depending on the order rows are visited ("CREATE TABLE",
  "Compatibility", "Non-Deferred Uniqueness Constraints"), where SQL
  Server checks the statement's result. Outside the spec.

**`DELETE FROM t [WHERE p]`**: no alias, no T-SQL second `FROM`, no
PostgreSQL `USING`, no `OUTPUT`/`RETURNING`; removes the rows where `p` is
TRUE. PostgreSQL: "DELETE": "Only rows for which this expression returns
true will be deleted." T-SQL: "DELETE (Transact-SQL)", `WHERE`. Not
probed.

**Constraint violations.** A NULL in a `NOT NULL` or key column (T-SQL
515, SQLSTATE 23502 `not_null_violation`) and a key that two rows share
(T-SQL 2627, SQLSTATE 23505 `unique_violation`) are errors of the kind
`constraint` (decision 1 compares the kind, so which of two violations is
reported is not observed). PostgreSQL checks `NOT NULL` and a
non-deferrable primary key for each row as it is written ("CREATE
TABLE"); for `INSERT` that fails exactly when the statement's result has
a NULL there or a duplicate key, which is when SQL Server fails. Not
probed.

**Statement atomicity.** A data statement that fails has no effect: T-SQL
rolls back at least the failing statement ("SET XACT_ABORT
(Transact-SQL)": "When SET XACT_ABORT is OFF, in some cases only the
Transact-SQL statement that raised the error is rolled back and the
transaction continues processing"; with it `ON`, "the entire transaction
is terminated and rolled back"); PostgreSQL "treats every SQL statement
as being executed within a transaction" ("Transactions", 3.4) and an
error aborts it. The core keeps the state from before the statement
(`Sqldiff.Core.exec_error_atomic`). The error then ends the routine (C2a:
under `XACT_ABORT ON` in T-SQL, `abortsT`); what the caller sees of the
database afterwards differs between the dialects and is not observed
(`Sqldiff.Core.Obs`; `ASM-STATE-AFTER-ERROR` in `residual`). Probed,
eval/errors-txn P12: a multi-row `INSERT` with a duplicate in its second
row inserts nothing, an `UPDATE` overflowing on its second row changes
nothing, on both engines.

**Row order and which error.** Neither vendor fixes the order in which a
statement visits rows. A statement whose rows could fail with errors of
two kinds (an arithmetic error on one row, a constraint violation on
another) reports whichever comes first, so it is outside the slice
(`Sqldiff.Core.Stmt.dmlSafe`); within one kind the core's order is not
observed.

**Where the expressions are evaluated.** A `WHERE` that can raise is
outside the slice: which rows an engine tests depends on the access path
(an index on `k` tests `k = @k AND 1/c > 0` only on the rows with that
key). The values of `SET` are evaluated, in the core, only on the rows the
`WHERE` keeps. For PostgreSQL that is SQL's logical order: "The WHERE
Clause" (7.2.2) discards the rows for which the condition is not true,
and what is computed from the rows (the select list, 7.3; an `UPDATE`'s
values) is computed on what remains; the manual does not say it of
`UPDATE` in so many words (reviewers check). SQL Server does not promise
it, but the core's choice is one it may make, and T-SQL is the reference
side, whose possible outcomes the core needs only to include one of. Not
probed.

**Values written to a column** are `int` expressions of the slice or the
NULL literal, in both dialects; anything else is outside the spec, even
on the C1 path. The column is `int`, so no conversion happens on the
write. Of another type, the value would be converted on assignment, and
the dialects convert differently (T-SQL truncates a `decimal` to `int`,
PostgreSQL rounds: probed, eval/casts/assign, `INSERT` and `UPDATE`),
which an uninterpreted operator does not show. The `WHERE` of `UPDATE`
and `DELETE` may be any condition (an uninterpreted operator on the C1
path).

**Constant parts of a data statement.** PL/pgSQL runs a data statement
through a prepared plan and may plan it with the variables' current
values as constants (PL/pgSQL "Plan Caching"; "PREPARE", custom plans),
and the planner folds constant subexpressions, raising their errors even
when no row is written (9.18.1 "CASE", 4.2.14 "Expression Evaluation
Rules"). So a part of an `UPDATE`'s values that reads no column and can
raise is outside the slice (`Sqldiff.Core.SExpr.constRaise`). `INSERT ...
VALUES` evaluates every value in any case, and a `WHERE` cannot raise.
Not probed. -/

/-- Spelling of a column as a statement of the dialect writes it. -/
def colKey (d : Dialect) (c : CatCol) : String :=
  match d with
  | .tsql _ _ => c.tsql
  | .pg _ _ => c.name

/-- The table can be used: names ASCII and distinct; T-SQL spellings
distinct up to case. -/
def CatTable.wellFormed (t : CatTable) : Bool :=
  !t.cols.isEmpty && t.cols.all (fun c => c.name.toList.all fun (ch : Char) => ch.toNat < 128) &&
    distinct (t.cols.map (·.name)) &&
    caseSafe (t.cols.map (·.tsql))

/-- The catalog table a data statement names, if it can be used. -/
def tableOf (d : Dialect) (target : List String) : Option CatTable :=
  let (Γ, key) : Catalog × Option String := match d with
    | .tsql dep Γ => (Γ, lookup dep.tables (dotted target))
    | .pg Γ _ => (Γ, some (dotted (target.map pgName)))
  key.bind fun n =>
    (Γ.tables.find? (·.name == n)).bind fun t => if t.wellFormed then some t else none

/-- A column named in an `INSERT` column list or as a `SET` target. -/
def colOf (d : Dialect) (t : CatTable) (x : String) : Option Core.Col :=
  let x := match d with
    | .tsql _ _ => x
    | .pg _ _ => pgName x
  lookup (t.cols.map fun c => (colKey d c, c.name)) x

/-- The columns an `UPDATE` or `DELETE` brings into scope, and for
PostgreSQL the table's reference name (the last part of its name). -/
def scopeOf (d : Dialect) (t : CatTable) (target : List String) : Scope :=
  { cols := t.cols.map fun c => (colKey d c, c.name)
    rel := match d with
      | .tsql _ _ => none
      | .pg _ _ => target.getLast?.map pgName }

def CatTable.toDef (t : CatTable) : Core.TableDef :=
  ⟨t.name, t.cols.map fun c => ⟨c.name, c.notNull, c.key⟩⟩

def CatTable.isKey (t : CatTable) (c : Core.Col) : Bool :=
  t.cols.any fun d => d.name == c && d.key

/-- A value written to a column: an `int` expression of the slice or the
NULL literal ("Values written to a column" above), else outside the spec. -/
def colValue (d : Dialect) (env : TyEnv) (sc : Scope) (e : Ast.Expr) : Option Core.Expr :=
  match typed d env sc e with
  | some (s, t) => if valTy t then some (.sc s) else none
  | none => none

/-- A data statement may stand here: in a procedure, and in T-SQL only
when row counts are not observed. -/
def dataOk (d : Dialect) (k : Ast.RoutineKind) : Bool :=
  (k matches .Procedure) &&
    match d with
    | .tsql dep _ => !dep.rowCounts
    | .pg _ _ => true

/-! ## Statements -/

/-- One statement, as a list of core statements (a block contributes its
statements in place, so the result does not depend on how `BEGIN ... END`
nests).

* Assignment (T-SQL `SET @x = e`, PL/pgSQL `x := e`) of an `int`
  expression or NULL to an `int` variable stores the value as it is: no
  conversion between equal types (Microsoft Learn, "SET @local_variable
  (Transact-SQL)"; PostgreSQL manual, PL/pgSQL "Basic Statements",
  "Assignment": the value is converted only "if the expression's result
  data type doesn't match the variable's data type"). Not probed. If `e` fails, the variable keeps its value and the error
  ends the routine (see the file header for T-SQL).
* T-SQL `DECLARE @x t` does nothing at run time: every variable is NULL
  from the start of the routine, and a `DECLARE` inside a loop does not
  reset it. `DECLARE @x t = e` assigns, where it stands, each time it
  runs. So the PL/pgSQL mirror declares `x` at the top without a default
  and assigns `x := e` at the same position.
* PL/pgSQL declarations belong to the `DECLARE` section (`lowerP`).
* `RETURN e` in a function returns `e`; in a T-SQL procedure it sets the
  status (above); a PostgreSQL procedure cannot return a value.
* `INSERT ... VALUES`, `UPDATE` and `DELETE` as in "The C3a slice: data
  statements" above; every other form of them is outside the spec.
* Queries, `MERGE` and messages are outside the spec. -/
def lowerStmt (d : Dialect) (env : TyEnv) (k : Ast.RoutineKind) : Ast.Stmt → Option (List Stmt)
  | .Assign t v => do
    let x ← target d t
    let v' ← lowerExpr d env {} v
    pure [.assign x v']
  | .Return none => some [.ret none]
  | .Return (some e) => do
    let e' ← lowerExpr d env {} e
    match d, k with
    | _, .Function => pure [.ret (some e')]
    | .tsql _ _, .Procedure => pure [.assign statusVar (statusExpr d env e e'), .ret none]
    | .pg _ _, .Procedure => none
  | .Declare x _ dflt => match d, dflt with
    | .tsql _ _, none => some []
    | .tsql _ _, some e => do
      let e' ← lowerExpr d env {} e
      pure [.assign x e']
    | .pg _ _, _ => none
  | .Insert tgt cols (.SrcValues rows) [] [] none => do
    if !dataOk d k then none else
    let t ← tableOf d tgt
    let cs ← cols.mapM (colOf d t)
    if cs.isEmpty || !distinct cs || rows.isEmpty || !rows.all (·.length == cs.length) then none
    else
      let rows' ← rows.mapM (·.mapM (colValue d env {}))
      pure [.insert t.toDef cs rows']
  | .Update tgt none sets [] wh => do
    if !dataOk d k then none else
    let t ← tableOf d tgt
    let sc := scopeOf d t tgt
    let sets' ← sets.mapM fun
      | .Set [c] e => do
        let c' ← colOf d t c
        if t.isKey c' then none else
          let e' ← colValue d env sc e
          pure (c', e')
      | _ => none
    if sets'.isEmpty || !distinct (sets'.map (·.1)) then none
    else
      let wh' ← lowerOpt d env sc wh
      pure [.update t.toDef sets' wh']
  | .Delete tgt none [] wh => do
    if !dataOk d k then none else
    let t ← tableOf d tgt
    let wh' ← lowerOpt d env (scopeOf d t tgt) wh
    pure [.delete t.toDef wh']
  | .SSelect .. | .Insert .. | .Update .. | .Delete .. | .Merge .. | .Message .. => none

/-! ## Calls -/

/-- T-SQL `EXEC p a, @x OUTPUT`: positional arguments only; an `OUTPUT`
argument is a variable that receives the parameter's final value; without
`OUTPUT` nothing is copied back, even to an `OUTPUT` parameter. The callee
is renamed through the deployment. `EXEC @rc = p` is outside the spec: the
status is the callee's *last* parameter in the core (above), so placing
`@rc` needs the callee's arity, which the check does not take as input. -/
def lowerTsqlArg (d : Dialect) (env : TyEnv) : Ast.Arg → Option Arg
  | .Arg (some _) _ _ => none -- named arguments: the core binds by position
  | .Arg none e true => (target d e).map .ref
  | .Arg none e false => (lowerExpr d env {} e).map .val

/-- PostgreSQL `CALL p(args)`: positional arguments only; an argument in
an `OUT` or `INOUT` position (the catalog says which) must be a variable,
and receives the parameter's final value; trailing parameters without an
argument take their defaults. -/
def lowerPgArg (d : Dialect) (env : TyEnv) : Ast.Mode × Ast.Arg → Option Arg
  | (_, .Arg (some _) _ _) => none
  | (_, .Arg none _ true) => none -- `OUTPUT` is T-SQL syntax
  | (.In, .Arg none e false) => (lowerExpr d env {} e).map .val
  | (_, .Arg none e false) => (target d e).map .ref

def lowerCall (d : Dialect) (env : TyEnv) (n : List String) (args : List Ast.Arg)
    (ret : Option Ast.Expr) : Option Stmt :=
  match d with
  | .tsql dep _ => do
    if ret.isSome then none else
    let callee ← lookup dep.routines (dotted n)
    let args' ← args.mapM (lowerTsqlArg d env)
    pure (.call callee args')
  | .pg Γ _ => do
    let callee := dotted (n.map pgName)
    let modes ← lookup Γ.routines callee
    if args.length ≤ modes.length && ret.isNone then
      let args' ← (modes.zip args).mapM (lowerPgArg d env)
      pure (.call callee args')
    else none

/-- `SET NOCOUNT ON|OFF` only changes rows-affected counts, of this
routine and of what it calls, so it is nothing when the observer does not
read them and outside the spec when it does.

`SET XACT_ABORT ON` makes a run-time error end the batch ("SET XACT_ABORT
(Transact-SQL)": "When SET XACT_ABORT is ON, if a Transact-SQL statement
raises a run-time error, the entire transaction is terminated and rolled
back"), which is what the core does with every error, so it lowers to
nothing. Where it is the routine's first statement (`abortsT`), every
error of the routine ends it; elsewhere it changes nothing for runs that
raise no error, and a C2a confirmation then needs a routine that cannot
raise. What the caller's transaction sees is "The C4 rules" (`ASM-NO-CALLER-TXN`). `XACT_ABORT OFF`
and other settings (`ARITHABORT`, `ANSI_WARNINGS`, ...) change error
behaviour: outside the spec.

`SET CONCAT_NULL_YIELDS_NULL ON` (C2b) makes `+` on a NULL string give
NULL from there on, which is what the core's `concat` always does, so it
lowers to nothing; at the start of the routine it discharges
`ASM-CONCAT-NULL` (`concatNullOn`). `OFF` is outside the spec. -/
def lowerSetting (d : Dialect) (name : String) (on : Bool) : Option (List Stmt) :=
  match d with
  | .tsql dep _ =>
    if name.toUpper = "NOCOUNT" && !dep.rowCounts then some []
    else if name.toUpper = "XACT_ABORT" && on then some []
    else if name.toUpper = "CONCAT_NULL_YIELDS_NULL" && on then some []
    else none
  | .pg _ _ => none

/-! ## The C4 rules: errors and transactions

Phase C4 adds T-SQL `TRY`/`CATCH`, `THROW` and `BEGIN TRAN`/`COMMIT`,
and PL/pgSQL exception blocks, `RAISE` and `COMMIT`, lowered to the
core's `tryCatch`, `raise`, `reraise` and `commit`
(`Sqldiff.Core.Stmt`). Each rule states its claim, the vendor text and
the probe status, as for C2a. "V10", "V17" and "V18" are the vendor facts
of docs/vendor-facts-verification.md, checked against the vendor pages on
2026-10-23/24; **every other vendor quote in this section was written
from memory** (the vendor sites were not reachable when it was written,
2026-10-28) and reviewers check it against the page named. **Probed** on SQL
Server 2022 (16.0.4305.5) and PostgreSQL 18.6, 2026-10-28:
`eval/errors-txn/{mssql,postgres}.sql` with their `.out` files, probes
P1 to P13 named below; every behaviour each rule claims was observed, and
P9d found one the rules did not state (the doomed caller transaction,
under "Transactions, T-SQL").

**Which errors exist.** The core's errors are integer overflow and
division by zero (`arithmetic`), `NOT NULL` and primary key violations
(`constraint`), errors the routine raises (`user`, below) and an
uninterpreted operator's (`other`, C1 path only). Both handlers catch
every one of them: T-SQL "TRY...CATCH (Transact-SQL)": "A TRY...CATCH
construct catches all execution errors that have a severity higher than
10 that do not close the database connection" (from memory); 8115, 8134,
515 and `THROW` have severity 16, 2627 severity 14 (probed, P6: all five
caught, with those severities).
PL/pgSQL: "The special condition name OTHERS matches every error type
except QUERY_CANCELED and ASSERT_FAILURE" (V18; probed, P6: 22012, 22003,
23502, 23505 and P0001 caught, an `ASSERT` failure (P0004) not). What either engine does
not catch (T-SQL: severity 20 and above, attentions, `KILL`, and at the
same level compile, recompilation and deferred-name-resolution errors,
V10; PL/pgSQL: `query_canceled`, `assert_failure`, FATAL and PANIC, V18)
comes from events the core does not model (cancellation, timeouts,
resource limits, a catalog other than the one given): a confirmation of
a routine with a handler rests on `ASM-CATCH-CORE-ERRORS`, that inside a
handled block no error is raised that the core does not model, or that
such an error is caught, or not, alike by both. A T-SQL routine with
`RAISERROR` is outside the spec (below).

**`TRY`/`CATCH` against `EXCEPTION WHEN OTHERS`.**
T-SQL `BEGIN TRY b END TRY BEGIN CATCH h END CATCH` lowers to
`tryCatch false b h`; PL/pgSQL `BEGIN b EXCEPTION WHEN OTHERS THEN h END`
(a nested block without `DECLARE`, one handler, condition `OTHERS`)
lowers to `tryCatch true b h`. Any other condition list (`WHEN
division_by_zero`, `SQLSTATE '22012'`, several handlers) is outside the
spec.

* Control: when `b` raises, the rest of `b` is skipped and `h` runs;
  when `b` finishes, `h` is skipped. T-SQL: "If there is an error in the
  code that is enclosed in a TRY block, control passes to the first
  statement in the associated CATCH block", and after a TRY block without
  error, "control passes to the statement immediately after the
  associated END CATCH statement" (from memory). PL/pgSQL "Trapping
  Errors": "if an error occurs within the statements, further processing
  of the statements is abandoned, and control passes to the EXCEPTION
  list ... the corresponding handler_statements are executed, and then
  control passes to the next statement after END" (V18's page; quoted
  from memory). Probed, P1, both engines.
* An error inside `h` is not caught by the same handler and leaves it:
  T-SQL "TRY...CATCH": errors in a CATCH block go to an enclosing TRY or
  to the caller (from memory); PL/pgSQL: "If a new error occurs within
  the selected handler_statements, it cannot be caught by this EXCEPTION
  clause, but is propagated out" (from memory). Handlers nest in both, as
  `tryCatch` does. Probed, P2, both engines.
* Variables keep the values the error left in both: PL/pgSQL "the local
  variables of the PL/pgSQL function remain as they were when the error
  occurred" (V18); T-SQL variables are not transactional (from memory:
  "Transactions (Transact-SQL)", table variables and variables are not
  affected by a rollback). Probed, P3, both engines.
* **The database differs**: PL/pgSQL "all changes to persistent database
  state within the block are rolled back" (V18), which is `undo`; T-SQL
  rolls back only the failing statement, and keeps what `b` wrote
  before it, committed when no transaction is open. With `XACT_ABORT ON`
  in an open transaction, the error makes the transaction uncommittable
  instead (`XACT_STATE() = -1`: "An error can make the transaction
  uncommittable", V10), and every write of `h` then fails (error 3930).
  Probed: P4 (autocommit, `XACT_ABORT` OFF and ON alike: the row written
  before the error is kept, `XACT_STATE() = 0` and `@@TRANCOUNT = 0` in
  `CATCH`, a write in `CATCH` succeeds; PostgreSQL rolls the block's row
  back and keeps the handler's), P5 (a caller transaction under
  `XACT_ABORT ON`: `XACT_STATE() = -1` in `CATCH`, the write fails with
  3930; with `XACT_ABORT OFF` the transaction stays committable, P5b). So the T-SQL lowering is faithful only when no
  transaction is open when the routine is called and the routine opens
  none around a handled error: `ASM-NO-CALLER-TXN` (the caller holds no
  open transaction, and `IMPLICIT_TRANSACTIONS` is `OFF` so that a
  statement does not open one), named whenever an error can reach a
  handler; and `BEGIN TRAN` is outside the spec inside a `TRY` (below).
  The two lowerings then differ only by `undo`, and the check settles it
  (`Sqldiff.Core.Stmt.settle`, theorem `Sqldiff.Core.exec_settle`): a
  T-SQL `TRY` whose body cannot fail after it may have written
  (`Sqldiff.Core.Stmt.undoSafe`) means the same with `undo`, and is
  confirmed against an `EXCEPTION` block; one whose body can, is refused.
* A call inside a handled body (at any depth) is outside the spec: a
  callee's own errors are not in the core (`WorldModel.call` does not
  fail), and its writes before its error are kept by T-SQL and rolled
  back by PL/pgSQL.

**`THROW` and `RAISE EXCEPTION`** raise an error of the kind `user`.

* T-SQL `THROW n, m, s` with `n` an integer literal from 50000 to
  2147483647, `m` a string literal and `s` an integer literal from 0 to
  255: "THROW (Transact-SQL)": error_number "must be greater than or
  equal to 50000 and less than or equal to 2147483647", state is
  `tinyint`, "The severity is always set to 16", and "If a TRY...CATCH
  construct is not available, the statement batch is terminated" (from
  memory); so it ends the routine whatever `XACT_ABORT` says, which the
  core does with every error. Probed: P6 (severity 16, the number and
  state given), P7 (ends the procedure with `XACT_ABORT OFF`; a statement
  error there does not, P7d), P7c (49999 is refused with error 35100). Variables as arguments are outside the
  spec (a NULL or out-of-range value fails differently).
* PL/pgSQL `RAISE [EXCEPTION] 'f'` with `f` without `%`, or `RAISE
  [EXCEPTION] USING MESSAGE = 'm'`, optionally with `ERRCODE = 'P0001'`:
  "Errors and Messages": the level `EXCEPTION` is the default and "raises
  an error (which normally aborts the current transaction)"; "If no
  condition name nor SQLSTATE is specified in a RAISE EXCEPTION command,
  the default is to use raise_exception (P0001)" (from memory; probed, P6: `RAISE EXCEPTION
  'm'`, `RAISE 'm'` and `USING MESSAGE = 'm', ERRCODE = 'P0001'` all raise
  P0001 with message `m`). Other
  levels (`NOTICE`, ...) send a message, not an error: outside the spec.
  A `%` in the format, arguments, or any other option (`DETAIL`,
  `HINT`, another `ERRCODE`) are outside it too. The code must be spelt
  `'P0001'` exactly: PostgreSQL reads an `ERRCODE` of five digits and
  upper-case letters as an SQLSTATE and anything else as a condition
  name, so `'p0001'` is an unknown condition and the `RAISE` fails with
  42704 ("unrecognized exception condition"; plpgsql `pl_exec.c`, from
  memory; not probed).
* The caller sees error number `n` from T-SQL and SQLSTATE P0001 from
  PostgreSQL, with the same message: decision 1 compares the kind, not
  the number or the text, so a confirmation that raises rests on
  `ASM-THROW-ERRCODE` (callers do not tell user errors apart by number).
* `RAISERROR` is outside the spec: its severity decides whether it is an
  error at all (0 to 10 are messages), and it does not end the routine
  under `XACT_ABORT ON` ("THROW honours XACT_ABORT; RAISERROR does not",
  V10). The converter refuses it too.

**Re-raising.** T-SQL `THROW;` and PL/pgSQL `RAISE;` re-raise the error
the handler handles, with its kind: "THROW (Transact-SQL)": "If the
THROW statement is specified without parameters, it must appear inside a
CATCH block. This causes the caught exception to be raised" (from
memory); PL/pgSQL: the form without parameters "can only be used inside
a BEGIN block's EXCEPTION clause; it causes the error currently being
handled to be re-thrown" (from memory). They lower to `reraise` only
directly in a handler, at any depth of `IF`/`WHILE` but not inside a
`TRY` or block nested in the handler (which error it means there is not
written down for both: outside the spec). Probed, P8: the caller sees
the original error (8134 severity 16; SQLSTATE 22012).

**Error functions.** `ERROR_NUMBER()`, `ERROR_MESSAGE()`,
`ERROR_SEVERITY()`, `ERROR_STATE()`, `ERROR_LINE()`,
`ERROR_PROCEDURE()`, `XACT_STATE()` and `@@ERROR` (T-SQL), and `SQLSTATE`
and `SQLERRM` (PL/pgSQL) are outside the spec wherever they stand: the
converter maps `ERROR_NUMBER()` to `SQLSTATE`, a number against a
five-character code, and no mapping between them is exact.

**Transactions, T-SQL.** `BEGIN TRAN` and `COMMIT` (without a name) only
in a T-SQL routine that starts with `SET XACT_ABORT ON` (`abortsT`), and
only as a bracket (`txnBracket`): in the routine's own statement list
(or in the body of the `TRY` of the idiom below), each `BEGIN TRAN`
followed in the same list by a `COMMIT`, not nested, and between them no
`RETURN`, call, `TRY` or other transaction statement at any depth. Under
`ASM-NO-CALLER-TXN` the transaction is then the routine's own, from
`BEGIN TRAN` (`@@TRANCOUNT` 0 to 1, "BEGIN TRANSACTION (Transact-SQL)",
from memory) to `COMMIT` (1 to 0, which "makes all data modifications
... a permanent part of the database", "COMMIT TRANSACTION
(Transact-SQL)", from memory); an error between them under `XACT_ABORT
ON` rolls it back and ends the routine ("SET XACT_ABORT": "the entire
transaction is terminated and rolled back", V10), which the core shows as
the failed call it is, the database after it not observed
(`ASM-STATE-AFTER-ERROR` as before). Probed: P9c, a caller without
`TRY`: the batch ends and the transaction is rolled back (the statements
before `BEGIN TRAN` stay). **But** P9 and P9d: a caller that catches the
error gets control back with the routine's transaction still open and
doomed (`@@TRANCOUNT = 1`, `XACT_STATE() = -1`, the routine's rows still
visible to it, its own writes failing with 3930 until it rolls back; the
batch rolls it back at its end, error 3998). PostgreSQL leaves no
transaction open after a failed `CALL` (P9). A failed observation does
not show the caller's transaction state, so the Rust side names this for
every routine with a `BEGIN TRAN` outside a `TRY`
(`ASM-TSQL-DOOMED-TXN`, crates/sqldiff-convert/src/cert.rs). A finished call shows the database
with everything committed, as without the transaction, so `BEGIN TRAN`
lowers to nothing and `COMMIT` to the core's `commit`, which does
nothing. Outside the spec: a `COMMIT` without its `BEGIN TRAN` (error
3902), a `BEGIN TRAN` without its `COMMIT` (error 266 at the end of the
procedure; probed, P10, with `@@TRANCOUNT = 1` left to the caller), nesting (`@@TRANCOUNT` 2), a `RETURN` between
them, `ROLLBACK` except in the idiom below, `SAVE TRAN`, named
transactions, and any of them inside `TRY`, `CATCH`, `IF`, `WHILE` or a
nested `BEGIN ... END` (the idiom below included: it stands only in the
routine's own statement list or in another idiom's body, `Ctx.txn`).

**The rollback-and-rethrow idiom.** T-SQL `BEGIN TRY b END TRY BEGIN
CATCH IF @@TRANCOUNT > 0 ROLLBACK; THROW; END CATCH` (or `ROLLBACK;
THROW;` when `b` is exactly one bracket: it begins with `BEGIN TRAN`,
ends with `COMMIT`, and has no other transaction statement, so that a
transaction is open whenever `b` fails), not inside any other `TRY` or
`CATCH`, and not inside an `IF`, a `WHILE` or a nested block, lowers to
`b` itself, at transaction level (its `BEGIN TRAN` and `COMMIT` as
above). With two brackets in `b`, an error between them (after the first
`COMMIT`, before the second `BEGIN TRAN`) finds no transaction open: the
unguarded `ROLLBACK` then raises 3903 ("The ROLLBACK TRANSACTION request
has no corresponding BEGIN TRANSACTION") and `THROW;` never runs, so the
caller sees 3903, not the body's error (probed on SQL Server during
review, 2026-10-28: `BEGIN TRAN; INSERT; COMMIT; SET @x = 10/@x; BEGIN TRAN;
INSERT; COMMIT` with `@x = 0`); that form is outside the spec. When `b` fails, the handler rolls back the open
transaction ("ROLLBACK TRANSACTION (Transact-SQL)": back to the
outermost `BEGIN TRAN`, `@@TRANCOUNT` to 0, from memory) and re-raises
the same error, so the routine fails with that error's kind, which is
what the core's `b` does; the rollback changes only the database after a
failed call, which a failed observation does not carry. When `b` does
not fail, the handler does not run. The converter writes `b` without the
handler (rule `idiom.rollback-rethrow`). Probed, P11: the caller sees
2627 with `@@TRANCOUNT = 0` and no row of `b` left, so no doomed
transaction reaches it.

**Transactions, PL/pgSQL.** `COMMIT` lowers to `commit`, only in a
procedure that runs with its caller's privileges (not `SECURITY
DEFINER`), outside every exception block and handler: "A transaction
cannot be ended inside a block with exception handlers"; "A SECURITY
DEFINER procedure cannot execute transaction control statements";
"If CALL is executed in a transaction block, then the called procedure
cannot execute transaction control statements" (V17). The last is
`ASM-NO-CALLER-TXN` again: the procedure is called by a top-level `CALL`
outside a transaction block, from a chain of `CALL`s and `DO`s only.
Then `COMMIT` cannot fail and changes nothing a finished call shows.
`ROLLBACK`, `COMMIT AND CHAIN` and the rest are outside the spec.
Probed: P9 (an error ends a top-level `CALL` and rolls back all its
writes), P9b (a `COMMIT` keeps what came before it), P13 (`COMMIT` in a
block with a handler: "cannot commit while a subtransaction is active";
in a `SECURITY DEFINER` procedure and in a `CALL` inside a transaction
block: "invalid transaction termination").

**Only in procedures.** A T-SQL function cannot contain `TRY`/`CATCH`,
`THROW` or transaction statements ("CREATE FUNCTION (Transact-SQL)": the
statements valid in a function include "Control-of-flow statements
except TRY...CATCH statements", from memory), so in a T-SQL function
they are outside the spec. A PL/pgSQL function may raise and handle
errors, but its T-SQL counterpart is a function too (the check's header
test), which has neither, so the lowered routines differ; PL/pgSQL
`COMMIT` is refused in a function by V17 (`Ctx.commit`). -/

/-- Where a node stands. -/
structure Ctx where
  /-- Inside the body of a `TRY` or an exception block, at any depth
  (handlers of blocks nested in it included): an error there is handled. -/
  handled : Bool := false
  /-- Directly in a handler (not inside a block or `TRY` nested in it):
  `THROW;` and `RAISE;` re-raise the error it handles. -/
  inHandler : Bool := false
  /-- Inside any `TRY`, `CATCH`, or exception block or handler. -/
  nested : Bool := false
  /-- (T-SQL) In a list whose `BEGIN TRAN` and `COMMIT` form a bracket
  (`txnBracket`). -/
  txn : Bool := false
  /-- (PL/pgSQL) `COMMIT` may stand here: a procedure with the caller's
  privileges, outside every exception block. -/
  commit : Bool := false

/-- Inside an `IF`, a `WHILE` or a block: no transaction level. -/
def Ctx.inner (cx : Ctx) : Ctx := { cx with txn := false }

/-- The body of a `TRY` or an exception block. -/
def Ctx.body (cx : Ctx) : Ctx :=
  { cx with handled := true, inHandler := false, nested := true, txn := false, commit := false }

/-- A handler. -/
def Ctx.handler (cx : Ctx) : Ctx :=
  { cx with inHandler := true, nested := true, txn := false, commit := false }

/-- A node in which nothing is a transaction statement, a `RETURN`, a call
or a `TRY`, at any depth: what may stand inside an open T-SQL
transaction. -/
def txnInner : Ast.Node → Bool
  | .NStmt (.Return _) => false
  | .NStmt _ => true
  | .Setting .. => true
  | .If _ t e => txnInnerList t && txnInnerList e
  | .While _ b => txnInnerList b
  | .NBlock (.Block _ b) => txnInnerList b
  | .Call .. | .Try .. | .Txn .. => false
  | .Throw _ | .Raise .. => true
where
  txnInnerList : List Ast.Node → Bool
    | [] => true
    | n :: ns => txnInner n && txnInnerList ns

def isTxn (op : String) : Ast.Node → Bool
  | .Txn o none => o.toUpper == op
  | _ => false

/-- The `BEGIN TRAN`s and `COMMIT`s of a T-SQL statement list form a
bracket: each `BEGIN TRAN` is closed by a `COMMIT` later in the list,
none is nested, every `COMMIT` closes one, and what stands between them
may stand in an open transaction (`txnInner`). Transaction statements
deeper in the list are refused where they stand (`Ctx.txn`). -/
def txnBracket (ns : List Ast.Node) : Bool :=
  go false ns
where
  go (open_ : Bool) : List Ast.Node → Bool
    | [] => !open_
    | n :: ns =>
      if isTxn "BEGIN" n then !open_ && go true ns
      else if isTxn "COMMIT" n then open_ && go false ns
      else (!open_ || txnInner n) && go open_ ns

/-- `@@TRANCOUNT > 0`. -/
def trancountPos : Ast.Expr → Bool
  | .Bin ">" (.Var v) (.Num "0") => v.toUpper == "@@TRANCOUNT"
  | _ => false

/-- `b` is exactly one bracket: `BEGIN TRAN` first, `COMMIT` last, and no
other transaction statement in the list (deeper ones are refused by
`txnBracket`), so a transaction is open wherever `b` can fail. -/
def oneBracket (b : List Ast.Node) : Bool :=
  (b.head?.map (isTxn "BEGIN")).getD false && (b.getLast?.map (isTxn "COMMIT")).getD false &&
    (b.filter fun | .Txn .. => true | _ => false).length == 2

/-- The handler of the rollback-and-rethrow idiom, for a `TRY` with body
`b`: `IF @@TRANCOUNT > 0 ROLLBACK; THROW;`, or `ROLLBACK; THROW;` when
`b` is exactly one bracket (`oneBracket`). -/
def rollbackRethrow (b h : List Ast.Node) : Bool :=
  match h with
  | [.If c [r] [], .Throw []] => trancountPos c && isTxn "ROLLBACK" r
  | [r, .Throw []] => isTxn "ROLLBACK" r && oneBracket b
  | _ => false

/-- The handlers of a `TRY` with body `b` are the idiom's. -/
def idiomHandler (b : List Ast.Node) : List Ast.Handler → Bool
  | [.Handler [] h] => rollbackRethrow b h
  | _ => false

/-- T-SQL `THROW n, m, s` of the spec. -/
def throwOk : List Ast.Expr → Bool
  | [.Num n, .Str _, .Num s] =>
    (match intLit? n with
      | some v => decide (50000 ≤ v)
      | none => false) &&
    (match intLit? s with
      | some v => decide (0 ≤ v ∧ v ≤ 255)
      | none => false)
  | _ => false

/-- The options of a PL/pgSQL `RAISE EXCEPTION` of the spec: `MESSAGE`
a string literal, `ERRCODE` the literal `'P0001'`, spelt exactly so (an
SQLSTATE in `ERRCODE` is case-sensitive: `'p0001'` names no condition and
PostgreSQL raises 42704 instead), each at most once, nothing else. The
option names are keywords, matched without case. -/
def raiseOptsOk (fmt : Option String) (opts : List Ast.RaiseOpt) : Bool :=
  let names := opts.map fun | .RaiseOpt n _ => n.toUpper
  distinct names && !(fmt.isSome && names.contains "MESSAGE") &&
    opts.all fun
      | .RaiseOpt n (.Str v) =>
        n.toUpper == "MESSAGE" || (n.toUpper == "ERRCODE" && v == "P0001")
      | _ => false

/-- PL/pgSQL `RAISE [EXCEPTION] ...` of the spec (not the bare re-raise). -/
def raiseOk (lvl fmt : Option String) (args : List Ast.Expr) (opts : List Ast.RaiseOpt) : Bool :=
  (lvl.map (·.toUpper == "EXCEPTION")).getD true && args.isEmpty &&
    (fmt.map fun f => !f.toList.contains '%').getD true &&
    (fmt.isSome || !opts.isEmpty) && raiseOptsOk fmt opts

/-- A transaction statement. -/
def lowerTxn (d : Dialect) (cx : Ctx) (op : String) (name : Option String) : Option (List Stmt) :=
  if name.isSome then none
  else match d, op.toUpper with
    | .tsql _ _, "BEGIN" => if cx.txn then some [] else none
    | .tsql _ _, "COMMIT" => if cx.txn then some [.commit] else none
    | .pg _ _, "COMMIT" => if cx.commit then some [.commit] else none
    | _, _ => none

/-- `THROW` (T-SQL) and `RAISE` (PL/pgSQL). -/
def lowerThrow (d : Dialect) (cx : Ctx) (args : List Ast.Expr) : Option (List Stmt) :=
  match d, args with
  | .tsql _ _, [] => if cx.inHandler then some [.reraise] else none
  | .tsql _ _, args => if throwOk args then some [.raise .user] else none
  | .pg _ _, _ => none

def lowerRaise (d : Dialect) (cx : Ctx) (lvl fmt : Option String) (args : List Ast.Expr)
    (opts : List Ast.RaiseOpt) : Option (List Stmt) :=
  match d, lvl, fmt, args, opts with
  | .pg _ _, none, none, [], [] => if cx.inHandler then some [.reraise] else none
  | .pg _ _, lvl, fmt, args, opts => if raiseOk lvl fmt args opts then some [.raise .user] else none
  | .tsql _ _, _, _, _, _ => none

mutual
  /-- Some node of a T-SQL statement tree is a transaction statement. -/
  def hasTxnNode : Ast.Node → Bool
    | .Txn .. => true
    | .If _ t e => hasTxnNodes t || hasTxnNodes e
    | .While _ b => hasTxnNodes b
    | .NBlock (.Block _ b) => hasTxnNodes b
    | .Try b hs => hasTxnNodes b || hasTxnHandlers hs
    | _ => false

  def hasTxnNodes : List Ast.Node → Bool
    | [] => false
    | n :: ns => hasTxnNode n || hasTxnNodes ns

  def hasTxnHandlers : List Ast.Handler → Bool
    | [] => false
    | .Handler _ b :: hs => hasTxnNodes b || hasTxnHandlers hs
end

/-- The settings a body starts with, in order. -/
def leadingSettings : List Ast.Node → List (String × Bool)
  | .Setting n on :: ns => (n.toUpper, on) :: leadingSettings ns
  | _ => []

/-! ## Blocks -/

mutual
  def lowerNode (d : Dialect) (env : TyEnv) (k : Ast.RoutineKind) (cx : Ctx) :
      Ast.Node → Option (List Stmt)
    | .NStmt s => lowerStmt d env k s
    | .NBlock b => lowerInner d env k cx.inner b
    | .If c t e => do
      let c' ← lowerExpr d env {} c
      let t' ← lowerNodes d env k cx.inner t
      let e' ← lowerNodes d env k cx.inner e
      pure [.ite c' (seqList t') (seqList e')]
    | .While c b => do
      let c' ← lowerExpr d env {} c
      let b' ← lowerNodes d env k cx.inner b
      pure [.while c' (seqList b')]
    | .Call n args ret =>
      -- C4: no call in a handled body.
      if cx.handled then none else (lowerCall d env n args ret).map fun s => [s]
    | .Setting n on => lowerSetting d n on
    | .Try b hs => match d, hs with
      | .tsql _ _, [.Handler [] h] =>
        if !(k matches .Procedure) then none
        else if !cx.nested && cx.txn && rollbackRethrow b h then
          -- The rollback-and-rethrow idiom: the body, at transaction level,
          -- only where a transaction statement may stand (`Ctx.txn`: not
          -- inside `IF`, `WHILE` or a nested block).
          if txnBracket b then lowerNodes d env k { cx with txn := true } b else none
        else do
          let b' ← lowerNodes d env k cx.body b
          let h' ← lowerNodes d env k cx.handler h
          pure [.tryCatch false (seqList b') (seqList h')]
      | .pg _ _, [.Handler ["others"] h] => do
        let b' ← lowerNodes d env k cx.body b
        let h' ← lowerNodes d env k cx.handler h
        pure [.tryCatch true (seqList b') (seqList h')]
      | _, _ => none
    | .Throw args => if k matches .Procedure then lowerThrow d cx args else none
    | .Raise lvl fmt args opts => lowerRaise d cx lvl fmt args opts
    | .Txn op name => if k matches .Procedure then lowerTxn d cx op name else none

  def lowerNodes (d : Dialect) (env : TyEnv) (k : Ast.RoutineKind) (cx : Ctx) :
      List Ast.Node → Option (List Stmt)
    | [] => some []
    | n :: ns => do
      let s ← lowerNode d env k cx n
      let ss ← lowerNodes d env k cx ns
      pure (s ++ ss)

  /-- A block inside the body. With declarations it is outside the spec: a
  PL/pgSQL nested `DECLARE` re-initialises on every entry and can shadow.
  Without, it is its statements (T-SQL `BEGIN ... END` has none). -/
  def lowerInner (d : Dialect) (env : TyEnv) (k : Ast.RoutineKind) (cx : Ctx) :
      Ast.Block → Option (List Stmt)
    | .Block [] body => lowerNodes d env k cx body
    | .Block (_ :: _) _ => none
end

/-- The names a list of statements declares, in order, with their types. -/
def declTys (ss : List Ast.Stmt) : List (String × Option Ast.Ty) :=
  ss.filterMap fun
    | .Declare x ty _ => some (x, ty)
    | _ => none

mutual
  /-- The names T-SQL `DECLARE`s anywhere in a node, in order, with their
  types. -/
  def declsNode : Ast.Node → List (String × Option Ast.Ty)
    | .NStmt (.Declare x ty _) => [(x, ty)]
    | .NBlock b => declsBlock b
    | .If _ t e => declsNodes t ++ declsNodes e
    | .While _ b => declsNodes b
    | .Try b hs => declsNodes b ++ declsHandlers hs
    | .NStmt _ | .Call .. | .Setting .. | .Throw _ | .Raise .. | .Txn .. => []

  def declsNodes : List Ast.Node → List (String × Option Ast.Ty)
    | [] => []
    | n :: ns => declsNode n ++ declsNodes ns

  def declsHandlers : List Ast.Handler → List (String × Option Ast.Ty)
    | [] => []
    | .Handler _ b :: hs => declsNodes b ++ declsHandlers hs

  def declsBlock : Ast.Block → List (String × Option Ast.Ty)
    | .Block ds body => declTys ds ++ declsNodes body
end

/-- The typed names, for `TyEnv`. -/
def typedNames (xs : List (String × Option Ast.Ty)) : TyEnv :=
  xs.filterMap fun (x, t) => t.map (x, ·)

/-! ## Routines -/

def lowerMode : Ast.Mode → Core.Mode
  | .In => .inp
  | .Out => .out
  | .InOut => .inout

/-- A parameter default of the spec: none, the NULL literal, or a value of
the slice that fits the parameter's declared type (`fitsTy`, as
`valueFits` below), so that neither engine converts it ("Parameter and
return types" below). -/
def dfltOk (d : Dialect) (env : TyEnv) (ty : Ast.Ty) : Option Ast.Expr → Bool
  | none => true
  | some .Null => true
  | some e => match typed d env {} e with
    | some (_, t) => fitsTy d ty t
    | none => false

def lowerParam (d : Dialect) (env : TyEnv) : Ast.Param → Option Core.Param
  | .Param name ty mode dflt =>
    let name := match d with
      | .tsql _ _ => name
      | .pg _ _ => pgName name
    if !dfltOk d env ty dflt then none else
    -- A default that reads a variable is refused by both engines.
    match lowerOpt d env {} dflt with
    | some e => if (e.map Core.Expr.vars).getD [] = [] then some ⟨name, lowerMode mode, e⟩ else none
    | none => none

/-- Every name is ASCII. -/
def asciiNames (xs : List String) : Bool :=
  xs.all fun x => x.toList.all fun (c : Char) => c.toNat < 128

/-- Every variable the routine reads or writes is a parameter or a local,
and none is declared twice. In both engines an undeclared name is an
error (SQL Server refuses the `CREATE`; PL/pgSQL fails at run time with
"column ... does not exist"), while the core would read NULL; T-SQL
system functions such as `@@ROWCOUNT` are not variables either. -/
def wellScoped (r : Core.Routine) : Bool :=
  let declared := r.params.map (·.name) ++ r.locals
  distinct declared && r.vars.all declared.contains

/-- The declared types of a T-SQL routine's variables. -/
def envT : Ast.Routine → TyEnv
  | .Routine _ _ params _ _ _ body =>
    params.map (fun | .Param n ty _ _ => (n, ty)) ++ typedNames (declsBlock body)

/-- A T-SQL routine. Outside the spec, besides the statements above:

* a name declared twice, or a local with a parameter's name (SQL Server
  refuses both, error 134), or a variable never declared (`wellScoped`);
* two different names equal up to case, or a non-ASCII name: whether
  `@a` and `@A` are one variable depends on the instance's collation, so
  the lowering, which keeps names as written, is faithful only when no
  two names of the routine could be confused;
* (C4) a transaction statement in a routine that does not start with `SET
  XACT_ABORT ON`, or `BEGIN TRAN` and `COMMIT` that do not form a bracket
  in the routine's statement list ("The C4 rules"). -/
def lowerT (dep : Deployment) (Γ : Catalog) (a : Ast.Routine) : Option Core.Routine :=
  match a with
  | .Routine kind _ params _ _ _ body => do
    let d := Dialect.tsql dep Γ
    let env := envT a
    let ps ← params.mapM (lowerParam d env)
    let nodes ← match body with
      | .Block [] ns => some ns
      | .Block (_ :: _) _ => none
    if hasTxnNodes nodes && !(leadingSettings nodes).contains ("XACT_ABORT", true) then none else
    if !txnBracket nodes then none else
    let stmts ← lowerNodes d env kind { txn := true } nodes
    let (ps, stmts) : List Core.Param × List Stmt := match kind with
      | .Procedure => (ps ++ [statusParam], Stmt.assign statusVar (.lit 0) :: stmts)
      | .Function => (ps, stmts)
    let r : Core.Routine :=
      { params := ps, locals := (declsBlock body).map (·.1), body := seqList stmts }
    if wellScoped r && caseSafe r.vars then some r else none

/-- The deployment convention for the T-SQL return status of a converted
procedure (`ASM-RETURN-CODE`): callers read it from the trailing `INOUT
return_code` parameter, and a procedure without one has status 0, which
is the status of a T-SQL procedure that never says `RETURN e`. So a
PL/pgSQL procedure whose last parameter is `INOUT return_code` lowers as
written (the certificate pairs `statusVar` with it), and one without is
given the status parameter, set to 0 on entry and never changed. A
procedure that does have the parameter must set it to 0 itself before
anything else can end the routine. -/
def statusName : Var := "return_code"

def hasStatus (ps : List Core.Param) : Bool :=
  match ps.getLast? with
  | some p => p.name == statusName && p.mode == .inout
  | none => false

/-- A PL/pgSQL declaration `x t [:= e]`: the name, and the assignment the
block runs on entry, in declaration order, when there is a default. -/
def lowerDecl (d : Dialect) (env : TyEnv) : Ast.Stmt → Option (Var × List Stmt)
  | .Declare x _ none => some (pgName x, [])
  | .Declare x _ (some e) =>
    (lowerExpr d env {} e).map fun e' => (pgName x, [.assign (pgName x) e'])
  | _ => none

/-- The declared types of a PL/pgSQL routine's variables, folded: what a
write to them must fit (C2b). -/
def envPDecl : Ast.Routine → TyEnv
  | .Routine _ _ params _ _ _ (.Block decls _) =>
    params.map (fun | .Param n ty _ _ => (pgName n, ty)) ++
      typedNames ((declTys decls).map fun (x, t) => (pgName x, t))

/-- A parameter's type as PostgreSQL keeps it: without the length of a
`varchar` ("PostgreSQL parameters carry no length", C2b). -/
def stripMod : Ast.Ty → Ast.Ty
  | .Ty n args => if pgVarchar n.toUpper then .Ty n [] else .Ty n args

/-- The types of a PL/pgSQL routine's variables as reads see them,
folded: parameters without a `varchar` length (C2b). The lowering types
expressions with these. -/
def envP : Ast.Routine → TyEnv
  | .Routine _ _ params _ _ _ (.Block decls _) =>
    params.map (fun | .Param n ty _ _ => (pgName n, stripMod ty)) ++
      typedNames ((declTys decls).map fun (x, t) => (pgName x, t))

/-- The names that are variables in a PL/pgSQL routine, folded: its
parameters, its declarations, and `found` (`resolveName`). -/
def pgVars : Ast.Routine → List Var
  | .Routine _ _ params _ _ _ (.Block decls _) =>
    params.map (fun | .Param n _ _ _ => pgName n) ++ (declTys decls).map (pgName ·.1) ++ ["found"]

/-- A PL/pgSQL routine: one block, its `DECLARE` section, its statements.
Every name is folded by `pgName`. Outside the spec, besides the
statements above: a non-ASCII name; a name declared twice, or a local
with a parameter's name (PostgreSQL lets the local shadow the parameter;
the core has one variable per name); a variable never declared
(`wellScoped`). -/
def lowerP (Γ : Catalog) (b : Ast.Routine) : Option Core.Routine :=
  match b with
  | .Routine kind _ params _ sec _ (.Block decls stmts) => do
    let d := Dialect.pg Γ (pgVars b)
    let env := envP b
    let ps ← params.mapM (lowerParam d env)
    let ds ← decls.mapM (lowerDecl d env)
    -- C4: `COMMIT` only in a procedure with its caller's privileges.
    let cx : Ctx := { commit := (kind matches .Procedure) && (sec matches .Caller) }
    let body ← lowerNodes d env kind cx stmts
    let body := (ds.map (·.2)).flatten ++ body
    -- The status of a procedure without `return_code` (see below).
    let (ps, body) : List Core.Param × List Stmt :=
      if kind matches .Procedure && !hasStatus ps then
        (ps ++ [statusParam], Stmt.assign statusVar (.lit 0) :: body)
      else (ps, body)
    let r : Core.Routine := { params := ps, locals := ds.map (·.1), body := seqList body }
    if wellScoped r && asciiNames r.vars then some r else none

/-- The routine kind and the privileges it runs with: the check asks both
sides to agree on them. -/
def header : Ast.Routine → Bool × Bool
  | .Routine kind _ _ _ sec _ _ =>
    (match kind with | .Procedure => true | .Function => false,
     match sec with | .Caller => true | .Owner => false)

/-! ## Parameter and return types

The lowered routines carry no types: the core binds a value as it is. So
the check also asks each T-SQL parameter and the PL/pgSQL parameter in
its position, and the two return types of a function, to be declared
with corresponding types (`sigCorr`), else the pair is outside the spec.
Without this, T-SQL `f(@a varchar(10)) RETURNS varchar(10)` and
PL/pgSQL `f(a integer) RETURNS integer` lower alike, though a caller
passing `'x'` gets a value from one and a conversion error from the
other. Corresponding types (`tyCorr`), as the converter writes them:

* T-SQL `int`/`integer` and PostgreSQL `integer`/`int`/`int4` (the C2a
  integer type, "The C2a slice: types");
* a T-SQL string type of the slice (`varchar(n)`, `char varying(n)`,
  `character varying(n)`, `nvarchar(n)`, `1 ≤ n ≤ 4000`) and a
  PostgreSQL one (`varchar(m)`, `character varying(m)`, `char
  varying(m)`, `varchar`, `text`), the C2b types (`strTypeT`,
  `strTypeP`); the converter writes `varchar(n)` for both T-SQL types
  (crates/sqldiff-convert/src/tsql.rs, `ty_pg`). The lengths need not
  agree: PostgreSQL drops the length of a parameter and of a function
  result (below), so `varchar(m)`, `varchar` and `text` there all take
  and give any string; what a body stores in either is held to that
  side's declared length by the slice (`fitsTy`), and an argument to the
  T-SQL length by `ASM-TSQL-STRING-ARGS`. (On the C1 path a stored string
  is not held to a length on either side; that is the C1 path's
  `ASM-C1-OPERATORS` caveat, as for a C2b assignment that does not fit;
  `ExamplesC2b`.) `max` lengths, `char(n)`, `bpchar` and the rest are
  outside;
* on the C1 path only (these types are not in the slice, so an answer
  with them names `check_sound`): T-SQL `bigint` and PostgreSQL `bigint`
  or `int8`, `smallint` and `smallint` or `int2`, without type
  arguments. Each is the same range of integers in both ("int, bigint,
  smallint, and tinyint (Transact-SQL)"; PostgreSQL 8.1.1, Table 8.2), so
  neither side converts an argument the other passes through. Every other
  type is
  outside the spec as a parameter or return type, also where the
  converter maps it: `tinyint` to `smallint` (the range widens), `bit` to
  `boolean`, `decimal(p,s)`/`numeric(p,s)` (PostgreSQL drops a parameter's
  type modifiers, so T-SQL rounds an argument to scale `s` and PostgreSQL
  does not), `datetime`, `datetime2`, `time` (precision), `char(n)`
  (padding), `date` (PostgreSQL's range is wider), `varchar(max)` to
  `text`, `uniqueidentifier` to `uuid` (text form), and user types.

The trailing PL/pgSQL `INOUT return_code` parameter of a procedure (the
T-SQL status, "The T-SQL return status") has no T-SQL counterpart; it
must be the integer type, as the status is.

**Parameter defaults** (`dfltOk`, in `lowerParam`) must be the NULL
literal or a value of the slice that fits the parameter's declared type
(`fitsTy`: an integer or a string literal within the declared length),
else the routine is outside the spec. T-SQL converts a default to the
parameter's type, truncating a string (`@s varchar(3) = 'abcdef'` is
`'abc'` in the body, as for `DECLARE`, "Assignment converts,
differently"); PostgreSQL drops a parameter's length modifier ("Parenthesized
type modifiers ... are discarded by CREATE FUNCTION", CREATE FUNCTION,
Notes; from memory), so `s varchar(3) DEFAULT 'abcdef'` keeps all six
characters. A default that fits is the same value in both. For
arguments the same dropped modifier is why a PostgreSQL parameter is read
as unbounded ("PostgreSQL parameters carry no length", C2b): an argument
longer than `n` reaches the PostgreSQL body whole and the T-SQL body
truncated, which `ASM-TSQL-STRING-ARGS` excludes (callers pass values
that fit). Not probed. -/

/-- A T-SQL type and the PostgreSQL type it corresponds to on the C1 path
only (upper-case names, no type arguments). -/
def c1Types : List (String × List String) :=
  [("BIGINT", ["BIGINT", "INT8"]), ("SMALLINT", ["SMALLINT", "INT2"])]

/-- A T-SQL parameter or return type and a PostgreSQL one correspond. -/
def tyCorr (ta tp : Ast.Ty) : Bool :=
  let int : Bool := match ta, tp with
    | .Ty a [], .Ty p [] =>
      (a.toUpper == "INT" || a.toUpper == "INTEGER") &&
        (p.toUpper == "INT" || p.toUpper == "INTEGER" || p.toUpper == "INT4")
    | _, _ => false
  let str : Bool := (strTypeT ta).isSome && (strTypeP tp).isSome
  let c1 : Bool := match ta, tp with
    | .Ty a [], .Ty p [] => ((lookup c1Types a.toUpper).map (·.contains p.toUpper)).getD false
    | _, _ => false
  int || str || c1

def paramTy : Ast.Param → Ast.Ty
  | .Param _ t _ _ => t

/-- The parameters in the same position have corresponding types, any
further PostgreSQL parameter (only the status `return_code` can lower
alike) is an integer, and the return types correspond. -/
def sigCorr : Ast.Routine → Ast.Routine → Bool
  | .Routine _ _ pa ra _ _ _, .Routine _ _ pb rb _ _ _ =>
    pa.length ≤ pb.length &&
    (pa.zip pb).all (fun (x, y) => tyCorr (paramTy x) (paramTy y)) &&
    (pb.drop pa.length).all (fun y => tyCorr (.Ty "INT" []) (paramTy y)) &&
    match ra, rb with
    | none, none => true
    | some x, some y => tyCorr x y
    | _, _ => false

/-! ## Every expression in the slice (C2a, C2b)

A routine is in the C2a slice when every expression it evaluates is a
slice expression of the right kind and every value it stores keeps its
type: conditions are booleans of the slice; assigned values, `RETURN`
values and defaults fit their target (`fitsTy`: an integer or NULL into
an integer, a string that fits or NULL into a string type); call
arguments are `int` expressions or NULL, and `OUTPUT` arguments `int`
variables; a function returns `int` or a string type. Then its lowering
has no uninterpreted operator, and no conversion happens where the core
does not see one. `env` types the expressions (reads); `wenv` gives the
declared type of a target (writes), which differs from `env` only for
PostgreSQL parameters ("PostgreSQL parameters carry no length"). `ret`
is a function's return type, `none` in a procedure (whose `RETURN e`
sets the integer status). -/

/-- A value position: an `int` expression or the NULL literal. -/
def valueOk (d : Dialect) (env : TyEnv) (sc : Scope) (e : Ast.Expr) : Bool :=
  match typed d env sc e with
  | some (_, t) => valTy t
  | none => false

/-- A value stored in something declared `ty`. -/
def valueFits (d : Dialect) (env : TyEnv) (sc : Scope) (ty : Ast.Ty) (e : Ast.Expr) : Bool :=
  match typed d env sc e with
  | some (_, t) => fitsTy d ty t
  | none => false

/-- A condition: a three-valued boolean of the slice. -/
def condOk (d : Dialect) (env : TyEnv) (sc : Scope) (e : Ast.Expr) : Bool :=
  match typed d env sc e with
  | some (_, .bool) => true
  | _ => false

/-- An assignment `t := v`: the value fits the target's declared type. -/
def assignOk (d : Dialect) (env wenv : TyEnv) (t v : Ast.Expr) : Bool :=
  match target d t with
  | some x => match lookup wenv x with
    | some ty => valueFits d env {} ty v
    | none => false
  | none => false

def sliceStmt (d : Dialect) (env wenv : TyEnv) (ret : Option Ast.Ty) : Ast.Stmt → Bool
  | .Assign t v => assignOk d env wenv t v
  | .Return none => true
  | .Return (some e) => match ret with
    | some ty => valueFits d env {} ty e
    | none => valueOk d env {} e
  | .Declare _ ty dflt => match dflt with
    | none => true
    | some e => (ty.map fun t => valueFits d env {} t e).getD false
  | .Insert _ _ (.SrcValues rows) _ _ _ => rows.all (·.all (valueOk d env {}))
  | .Update tgt _ sets _ wh => match tableOf d tgt with
    | some t =>
      let sc := scopeOf d t tgt
      sets.all (fun | .Set _ e => valueOk d env sc e) && wh.all (condOk d env sc)
    | none => false
  | .Delete tgt _ _ wh => match tableOf d tgt with
    | some t => wh.all (condOk d env (scopeOf d t tgt))
    | none => false
  | _ => false

mutual
  def sliceNode (d : Dialect) (env wenv : TyEnv) (ret : Option Ast.Ty) : Ast.Node → Bool
    | .NStmt s => sliceStmt d env wenv ret s
    | .NBlock b => sliceBlock d env wenv ret b
    | .If c t e => condOk d env {} c && sliceNodes d env wenv ret t && sliceNodes d env wenv ret e
    | .While c b => condOk d env {} c && sliceNodes d env wenv ret b
    | .Call _ args _ => args.all fun | .Arg _ e _ => valueOk d env {} e
    | .Setting .. => true
    -- The handler of the rollback-and-rethrow idiom is not lowered.
    | .Try b hs =>
      sliceNodes d env wenv ret b && (idiomHandler b hs || sliceHandlers d env wenv ret hs)
    | .Throw _ | .Raise .. | .Txn .. => true

  def sliceNodes (d : Dialect) (env wenv : TyEnv) (ret : Option Ast.Ty) : List Ast.Node → Bool
    | [] => true
    | n :: ns => sliceNode d env wenv ret n && sliceNodes d env wenv ret ns

  def sliceHandlers (d : Dialect) (env wenv : TyEnv) (ret : Option Ast.Ty) :
      List Ast.Handler → Bool
    | [] => true
    | .Handler _ b :: hs => sliceNodes d env wenv ret b && sliceHandlers d env wenv ret hs

  def sliceBlock (d : Dialect) (env wenv : TyEnv) (ret : Option Ast.Ty) : Ast.Block → Bool
    | .Block ds body => ds.all (sliceStmt d env wenv ret) && sliceNodes d env wenv ret body
end

def sliceParam (d : Dialect) (env : TyEnv) : Ast.Param → Bool
  | .Param _ _ _ none => true
  | .Param _ _ _ (some .Null) => true
  | .Param _ ty _ (some e) => valueFits d env {} ty e

/-- A type a function of the slice may return. -/
def retTypeOk (d : Dialect) (ty : Ast.Ty) : Bool := intType d ty || (strType d ty).isSome

def sliceRoutine (d : Dialect) (env wenv : TyEnv) : Ast.Routine → Bool
  | .Routine kind _ params returns _ _ body =>
    let ret := match kind with
      | .Function => returns
      | .Procedure => none
    params.all (sliceParam d env) && sliceBlock d env wenv ret body &&
      match kind with
      | .Function => (returns.map (retTypeOk d)).getD false
      | .Procedure => true

def sliceT (dep : Deployment) (Γ : Catalog) (a : Ast.Routine) : Bool :=
  sliceRoutine (.tsql dep Γ) (envT a) (envT a) a

def sliceP (Γ : Catalog) (b : Ast.Routine) : Bool :=
  sliceRoutine (.pg Γ (pgVars b)) (envP b) (envPDecl b) b

/-- The T-SQL routine aborts on every error: `SET XACT_ABORT ON` comes
before any statement that could fail. -/
def abortsT : Ast.Routine → Bool
  | .Routine _ _ _ _ _ _ (.Block _ body) => (leadingSettings body).contains ("XACT_ABORT", true)

/-- The T-SQL routine starts by setting `CONCAT_NULL_YIELDS_NULL ON`, so
every concatenation runs under it (C2b). -/
def concatNullOn : Ast.Routine → Bool
  | .Routine _ _ _ _ _ _ (.Block _ body) =>
    (leadingSettings body).contains ("CONCAT_NULL_YIELDS_NULL", true)

/-- The T-SQL routine has a parameter of a string type of the slice: the
confirmation rests on the arguments fitting it (`ASM-TSQL-STRING-ARGS`,
C2b). -/
def tsqlStringParams : Ast.Routine → Bool
  | .Routine _ _ params _ _ _ _ => params.any fun | .Param _ ty _ _ => (strTypeT ty).isSome

/-- The T-SQL routine has a transaction statement (C4): `BEGIN TRAN`,
`COMMIT`, or the `ROLLBACK` of the rollback-and-rethrow idiom. -/
def tsqlTxn : Ast.Routine → Bool
  | .Routine _ _ _ _ _ _ (.Block _ body) => hasTxnNodes body

end Sqldiff.Mirror
