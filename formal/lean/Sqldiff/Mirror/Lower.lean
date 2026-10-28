/-
The lowering of T-SQL and PL/pgSQL routines, as the Rust parser reads them
(`Sqldiff.Ast`, generated), into the core. Decision 9
(docs/verified-conversion.md): this file *is* the specification of the C1
constructs and of the C2a scalar slice in both dialects; there is no other
T-SQL or PL/pgSQL semantics to prove it against. One abstract syntax
serves both dialects and the dialect is a parameter. The lowering is
partial: `none` means "outside the spec" (queries, DML, error handlers,
other settings, ...), and a routine outside it is never confirmed.

Errors (C2a): an expression of the slice can fail (overflow, division by
zero), and the core then ends the routine with an error observation. That
is what PL/pgSQL does without an `EXCEPTION` block, and what T-SQL does
under `SET XACT_ABORT ON`; a T-SQL statement that fails without it is
rolled back on its own and the procedure goes on. So the T-SQL lowering is
faithful for every run only when the routine aborts on errors
(`abortsT`), and otherwise for the runs that raise none, as in C1. A C2a
confirmation (`Sqldiff.Mirror.inC2a`) asks for one or the other. Errors
from elsewhere (a missing argument without a default, `OUTPUT` on a
parameter that is not one, a failing callee) are C4: the rule says what
happens otherwise. No data either: names are variables, never columns.

Read top to bottom: the trusted inputs, the C2a slice with its vendor
citations, expressions, statements, calls, blocks, the two routines, and
the test for "every expression is in the slice".
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

/-- The parameter modes of the PostgreSQL routines a `CALL` can reach,
from the target catalog (trusted like every catalog fact; its hash goes in
the certificate). PL/pgSQL writes back to the variable in each `OUT` and
`INOUT` position, so the lowering of `CALL` needs them. -/
abbrev Catalog := List (String × List Ast.Mode)

inductive Dialect where
  | tsql (dep : Deployment)
  | pg (Γ : Catalog)

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
assignment, `COALESCE`/`ISNULL` argument, `CASE` result). -/
inductive STy where
  | int
  | bool
  | nul
deriving DecidableEq, Repr

/-- The declared types of a routine's variables, by lowered name. -/
abbrev TyEnv := List (Var × Ast.Ty)

/-- The declared type is the slice's integer type. -/
def intType : Dialect → Ast.Ty → Bool
  | .tsql _, .Ty n [] => n.toUpper == "INT" || n.toUpper == "INTEGER"
  | .pg _, .Ty n [] => n.toUpper == "INT" || n.toUpper == "INTEGER" || n.toUpper == "INT4"
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

/-- An expression without variables is in the slice only if evaluating it
does not fail (constant subexpressions, above). -/
def closedSafe (s : SExpr) : Bool :=
  !s.vars.isEmpty ||
    match s.eval (U := Unit) Core.nullStore with
    | .err _ => false
    | .ok _ => true

def guardC (r : Option (SExpr × STy)) : Option (SExpr × STy) :=
  r.bind fun p => if closedSafe p.1 then some p else none

def binTy (o : String) : SExpr × STy → SExpr × STy → Option (SExpr × STy)
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
  | _, _ => none

def unTy (o : String) : SExpr × STy → Option (SExpr × STy)
  | (a, .int) => if o == "-" then some (.neg a, .int) else none
  | (a, .bool) => if o.toUpper == "NOT" then some (.not a, .bool) else none
  | _ => none

/-- `coalesce x1 (coalesce x2 ... xn)`, every argument a value. -/
def coalesceTy : List (SExpr × STy) → Option SExpr
  | [] => none
  | [(a, t)] => if valTy t then some a else none
  | (a, t) :: rest => if valTy t then (coalesceTy rest).map (.coalesce a) else none

def callTy (d : Dialect) (name : String) (args : List (SExpr × STy)) : Option (SExpr × STy) :=
  match name.toUpper, args, d with
  | "COALESCE", _, _ =>
    if args.length ≥ 2 && args.any (·.2 == .int) then (coalesceTy args).map (·, .int) else none
  | "ISNULL", [(x, .int), (y, ty)], .tsql _ =>
    if valTy ty && !y.mayRaise then some (.coalesce x y, .int) else none
  | _, _, _ => none

/-- `cond c1 r1 (cond c2 r2 ... e)`. -/
def caseBuild : List ((SExpr × STy) × (SExpr × STy)) → SExpr → Option SExpr
  | [], e => some e
  | ((c, .bool), (r, tr)) :: ws, e => if valTy tr then (caseBuild ws e).map (.cond c r) else none
  | _, _ => none

def caseTy (ws : List ((SExpr × STy) × (SExpr × STy))) (els : Option (SExpr × STy)) :
    Option (SExpr × STy) :=
  let elsOk := match els with
    | some (_, t) => valTy t
    | none => true
  let anyInt := ws.any (·.2.2 == .int) || els.any (·.2 == .int)
  if ws.isEmpty || !elsOk || !anyInt then none
  else (caseBuild ws ((els.map (·.1)).getD .null)).map (·, .int)

mutual
  /-- The slice form of an expression and its type, or `none` when the
  expression is outside the slice. A variable is in the slice when it is
  declared with the slice's integer type (`env`). -/
  def typed (d : Dialect) (env : TyEnv) : Ast.Expr → Option (SExpr × STy)
    | .Var v => match d with
      | .tsql _ => if intVar d env v then some (.var v, .int) else none
      | .pg _ => none
    | .Name [x] => match d with
      | .pg _ => if intVar d env (pgName x) then some (.var (pgName x), .int) else none
      | .tsql _ => none
    | .Num s => (intLit? s).map fun n => (.int n, .int)
    | .Null => some (.null, .nul)
    | .Bin o a b => do
      let a' ← typed d env a
      let b' ← typed d env b
      guardC (binTy o a' b')
    | .Unary o a => do
      let a' ← typed d env a
      guardC (unTy o a')
    | .IsNull x isNot => do
      let (x', tx) ← typed d env x
      if tx == .int then guardC (some (.isNull x' isNot, .bool)) else none
    | .CallE (.CallF [n] args false [] false false false) => do
      let args' ← typedList d env args
      guardC (callTy d n args')
    | .Case none whens els => do
      let ws ← typedWhens d env whens
      let e ← typedOpt d env els
      guardC (caseTy ws e)
    | _ => none

  def typedList (d : Dialect) (env : TyEnv) : List Ast.Expr → Option (List (SExpr × STy))
    | [] => some []
    | e :: es => do
      let e' ← typed d env e
      let es' ← typedList d env es
      pure (e' :: es')

  def typedOpt (d : Dialect) (env : TyEnv) : Option Ast.Expr → Option (Option (SExpr × STy))
    | none => some none
    | some e => (typed d env e).map some

  def typedWhens (d : Dialect) (env : TyEnv) :
      List Ast.When2 → Option (List ((SExpr × STy) × (SExpr × STy)))
    | [] => some []
    | .When2 c r :: ws => do
      let c' ← typed d env c
      let r' ← typed d env r
      let ws' ← typedWhens d env ws
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

def tyName : Ast.Ty → String
  | .Ty n args => n.toLower ++ "(" ++ ",".intercalate args ++ ")"

/-- The slice form of `e` if it has one, else `c1`. -/
def withSlice (d : Dialect) (env : TyEnv) (e : Ast.Expr) (c1 : Option Core.Expr) :
    Option Core.Expr :=
  match typed d env e with
  | some (s, _) => some (.sc s)
  | none => c1

mutual
  def lowerExpr (d : Dialect) (env : TyEnv) : Ast.Expr → Option Core.Expr
    | .Var v => match d with
      | .tsql _ => if v.startsWith "@@" then none else some (.var v) -- `@@ERROR` etc.: C4
      | .pg _ => none
    | .Name [x] => match d with
      | .pg _ => some (.var (pgName x))
      | .tsql _ => none -- a column: data, C3
    | .Name _ => none
    | .Num s => some (match intLit? s with
      | some n => .lit n
      | none => .op ("num:" ++ s) [])
    | .Str s => some (.op ("str:" ++ s) [])
    | .Null => some .null
    | .Star _ => none
    | .CallE (.CallF n args distinct order withinGroup over star) =>
      withSlice d env (.CallE (.CallF n args distinct order withinGroup over star))
        (if distinct || !order.isEmpty || withinGroup || over || star then none
        else (lowerExprs d env args).map (.op ("fn:" ++ (dotted n).toLower)))
    | .Bin o a b => withSlice d env (.Bin o a b) do
      let a' ← lowerExpr d env a
      let b' ← lowerExpr d env b
      pure (.op ("bin:" ++ o.toLower) [a', b'])
    | .Unary o a => withSlice d env (.Unary o a)
      ((lowerExpr d env a).map fun a' => .op ("un:" ++ o.toLower) [a'])
    | .IsNull x neg => withSlice d env (.IsNull x neg)
      ((lowerExpr d env x).map fun x' => .op ("isnull:" ++ flag neg) [x'])
    | .InList x xs neg => do
      let x' ← lowerExpr d env x
      let xs' ← lowerExprs d env xs
      pure (.op ("in:" ++ flag neg) (x' :: xs'))
    | .InQuery .. => none
    | .Between x lo hi neg => do
      let x' ← lowerExpr d env x
      let lo' ← lowerExpr d env lo
      let hi' ← lowerExpr d env hi
      pure (.op ("between:" ++ flag neg) [x', lo', hi'])
    | .Like x p neg ci => do
      let x' ← lowerExpr d env x
      let p' ← lowerExpr d env p
      pure (.op ("like:" ++ flag neg ++ flag ci) [x', p'])
    | .Cast x ty => (lowerExpr d env x).map fun x' => .op ("cast:" ++ tyName ty) [x']
    | .Case operand whens els => withSlice d env (.Case operand whens els) do
      -- `case:OE`: whether there is an operand and an `ELSE`; the `WHEN`
      -- pairs fill the arguments between them.
      let o' ← lowerOpt d env operand
      let ws ← lowerWhens d env whens
      let e' ← lowerOpt d env els
      pure (.op ("case:" ++ flag o'.isSome ++ flag e'.isSome) (o'.toList ++ ws ++ e'.toList))
    | .Exists _ | .Subquery _ => none
    | .Tuple xs => (lowerExprs d env xs).map (.op "tuple")
    | .Special n => some (.op ("special:" ++ n.toLower) [])
    | .Extract f x => (lowerExpr d env x).map fun x' => .op ("extract:" ++ f.toLower) [x']

  def lowerExprs (d : Dialect) (env : TyEnv) : List Ast.Expr → Option (List Core.Expr)
    | [] => some []
    | e :: es => do
      let e' ← lowerExpr d env e
      let es' ← lowerExprs d env es
      pure (e' :: es')

  /-- An optional part: `some none` when absent, `none` when unsupported. -/
  def lowerOpt (d : Dialect) (env : TyEnv) : Option Ast.Expr → Option (Option Core.Expr)
    | none => some none
    | some e => (lowerExpr d env e).map some

  def lowerWhens (d : Dialect) (env : TyEnv) : List Ast.When2 → Option (List Core.Expr)
    | [] => some []
    | .When2 c r :: ws => do
      let c' ← lowerExpr d env c
      let r' ← lowerExpr d env r
      let ws' ← lowerWhens d env ws
      pure (c' :: r' :: ws')
end

/-- The variable an assignment or an `OUTPUT` argument names. -/
def target : Dialect → Ast.Expr → Option Var
  | .tsql _, .Var v => if v.startsWith "@@" then none else some v
  | .pg _, .Name [x] => some (pgName x)
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
  match typed d env e with
  | some (s, t) => if valTy t then .sc (.coalesce s (.int 0)) else .op "fn:coalesce" [e', .lit 0]
  | none => .op "fn:coalesce" [e', .lit 0]

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
* Queries, DML and messages are outside the spec. -/
def lowerStmt (d : Dialect) (env : TyEnv) (k : Ast.RoutineKind) : Ast.Stmt → Option (List Stmt)
  | .Assign t v => do
    let x ← target d t
    let v' ← lowerExpr d env v
    pure [.assign x v']
  | .Return none => some [.ret none]
  | .Return (some e) => do
    let e' ← lowerExpr d env e
    match d, k with
    | _, .Function => pure [.ret (some e')]
    | .tsql _, .Procedure => pure [.assign statusVar (statusExpr d env e e'), .ret none]
    | .pg _, .Procedure => none
  | .Declare x _ dflt => match d, dflt with
    | .tsql _, none => some []
    | .tsql _, some e => do
      let e' ← lowerExpr d env e
      pure [.assign x e']
    | .pg _, _ => none
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
  | .Arg none e false => (lowerExpr d env e).map .val

/-- PostgreSQL `CALL p(args)`: positional arguments only; an argument in
an `OUT` or `INOUT` position (the catalog says which) must be a variable,
and receives the parameter's final value; trailing parameters without an
argument take their defaults. -/
def lowerPgArg (d : Dialect) (env : TyEnv) : Ast.Mode × Ast.Arg → Option Arg
  | (_, .Arg (some _) _ _) => none
  | (_, .Arg none _ true) => none -- `OUTPUT` is T-SQL syntax
  | (.In, .Arg none e false) => (lowerExpr d env e).map .val
  | (_, .Arg none e false) => (target d e).map .ref

def lowerCall (d : Dialect) (env : TyEnv) (n : List String) (args : List Ast.Arg)
    (ret : Option Ast.Expr) : Option Stmt :=
  match d with
  | .tsql dep => do
    if ret.isSome then none else
    let callee ← lookup dep.routines (dotted n)
    let args' ← args.mapM (lowerTsqlArg d env)
    pure (.call callee args')
  | .pg Γ => do
    let callee := dotted (n.map pgName)
    let modes ← lookup Γ callee
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
raise. The rollback of the caller's transaction is C4. `XACT_ABORT OFF`
and other settings (`ARITHABORT`, `ANSI_WARNINGS`, ...) change error
behaviour: outside the spec. -/
def lowerSetting (d : Dialect) (name : String) (on : Bool) : Option (List Stmt) :=
  match d with
  | .tsql dep =>
    if name.toUpper = "NOCOUNT" && !dep.rowCounts then some []
    else if name.toUpper = "XACT_ABORT" && on then some []
    else none
  | .pg _ => none

/-! ## Blocks -/

mutual
  def lowerNode (d : Dialect) (env : TyEnv) (k : Ast.RoutineKind) : Ast.Node → Option (List Stmt)
    | .NStmt s => lowerStmt d env k s
    | .NBlock b => lowerInner d env k b
    | .If c t e => do
      let c' ← lowerExpr d env c
      let t' ← lowerNodes d env k t
      let e' ← lowerNodes d env k e
      pure [.ite c' (seqList t') (seqList e')]
    | .While c b => do
      let c' ← lowerExpr d env c
      let b' ← lowerNodes d env k b
      pure [.while c' (seqList b')]
    | .Call n args ret => (lowerCall d env n args ret).map fun s => [s]
    | .Setting n on => lowerSetting d n on

  def lowerNodes (d : Dialect) (env : TyEnv) (k : Ast.RoutineKind) :
      List Ast.Node → Option (List Stmt)
    | [] => some []
    | n :: ns => do
      let s ← lowerNode d env k n
      let ss ← lowerNodes d env k ns
      pure (s ++ ss)

  /-- A block inside the body. With declarations it is outside the spec: a
  PL/pgSQL nested `DECLARE` re-initialises on every entry and can shadow.
  Without, it is its statements (T-SQL `BEGIN ... END` has none). -/
  def lowerInner (d : Dialect) (env : TyEnv) (k : Ast.RoutineKind) :
      Ast.Block → Option (List Stmt)
    | .Block [] body => lowerNodes d env k body
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
    | .NStmt _ | .Call .. | .Setting .. => []

  def declsNodes : List Ast.Node → List (String × Option Ast.Ty)
    | [] => []
    | n :: ns => declsNode n ++ declsNodes ns

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

def lowerParam (d : Dialect) (env : TyEnv) : Ast.Param → Option Core.Param
  | .Param name _ mode dflt =>
    let name := match d with
      | .tsql _ => name
      | .pg _ => pgName name
    -- A default that reads a variable is refused by both engines.
    match lowerOpt d env dflt with
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
  two names of the routine could be confused. -/
def lowerT (dep : Deployment) (a : Ast.Routine) : Option Core.Routine :=
  match a with
  | .Routine kind _ params _ _ _ body => do
    let d := Dialect.tsql dep
    let env := envT a
    let ps ← params.mapM (lowerParam d env)
    let stmts ← lowerInner d env kind body
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
    (lowerExpr d env e).map fun e' => (pgName x, [.assign (pgName x) e'])
  | _ => none

/-- The declared types of a PL/pgSQL routine's variables, folded. -/
def envP : Ast.Routine → TyEnv
  | .Routine _ _ params _ _ _ (.Block decls _) =>
    params.map (fun | .Param n ty _ _ => (pgName n, ty)) ++
      typedNames ((declTys decls).map fun (x, t) => (pgName x, t))

/-- A PL/pgSQL routine: one block, its `DECLARE` section, its statements.
Every name is folded by `pgName`. Outside the spec, besides the
statements above: a non-ASCII name; a name declared twice, or a local
with a parameter's name (PostgreSQL lets the local shadow the parameter;
the core has one variable per name); a variable never declared
(`wellScoped`). -/
def lowerP (Γ : Catalog) (b : Ast.Routine) : Option Core.Routine :=
  match b with
  | .Routine kind _ params _ _ _ (.Block decls stmts) => do
    let d := Dialect.pg Γ
    let env := envP b
    let ps ← params.mapM (lowerParam d env)
    let ds ← decls.mapM (lowerDecl d env)
    let body ← lowerNodes d env kind stmts
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

/-! ## Every expression in the slice (C2a)

A routine is in the C2a slice when every expression it evaluates is a
slice expression of the right kind and every value it stores keeps its
type: conditions are booleans of the slice; assigned values, `RETURN`
values, call arguments and defaults are `int` expressions or NULL;
targets and `OUTPUT` arguments are `int` variables; a function returns
`int`. (A default may be NULL whatever the parameter's type.) Then its
lowering has no uninterpreted operator, and no conversion happens where
the core does not see one. -/

/-- A value position: an `int` expression or the NULL literal. -/
def valueOk (d : Dialect) (env : TyEnv) (e : Ast.Expr) : Bool :=
  match typed d env e with
  | some (_, t) => valTy t
  | none => false

/-- A condition: a three-valued boolean of the slice. -/
def condOk (d : Dialect) (env : TyEnv) (e : Ast.Expr) : Bool :=
  match typed d env e with
  | some (_, .bool) => true
  | _ => false

def intTarget (d : Dialect) (env : TyEnv) (t : Ast.Expr) : Bool :=
  match target d t with
  | some x => intVar d env x
  | none => false

def sliceStmt (d : Dialect) (env : TyEnv) : Ast.Stmt → Bool
  | .Assign t v => intTarget d env t && valueOk d env v
  | .Return none => true
  | .Return (some e) => valueOk d env e
  | .Declare _ ty dflt => match dflt with
    | none => true
    | some e => (ty.map (intType d)).getD false && valueOk d env e
  | _ => false

mutual
  def sliceNode (d : Dialect) (env : TyEnv) : Ast.Node → Bool
    | .NStmt s => sliceStmt d env s
    | .NBlock b => sliceBlock d env b
    | .If c t e => condOk d env c && sliceNodes d env t && sliceNodes d env e
    | .While c b => condOk d env c && sliceNodes d env b
    | .Call _ args _ => args.all fun | .Arg _ e _ => valueOk d env e
    | .Setting .. => true

  def sliceNodes (d : Dialect) (env : TyEnv) : List Ast.Node → Bool
    | [] => true
    | n :: ns => sliceNode d env n && sliceNodes d env ns

  def sliceBlock (d : Dialect) (env : TyEnv) : Ast.Block → Bool
    | .Block ds body => ds.all (sliceStmt d env) && sliceNodes d env body
end

def sliceParam (d : Dialect) (env : TyEnv) : Ast.Param → Bool
  | .Param _ _ _ none => true
  | .Param _ _ _ (some .Null) => true
  | .Param _ ty _ (some e) => intType d ty && valueOk d env e

def sliceRoutine (d : Dialect) (env : TyEnv) : Ast.Routine → Bool
  | .Routine kind _ params returns _ _ body =>
    params.all (sliceParam d env) && sliceBlock d env body &&
      match kind with
      | .Function => (returns.map (intType d)).getD false
      | .Procedure => true

def sliceT (dep : Deployment) (a : Ast.Routine) : Bool := sliceRoutine (.tsql dep) (envT a) a

def sliceP (Γ : Catalog) (b : Ast.Routine) : Bool := sliceRoutine (.pg Γ) (envP b) b

/-- The settings a body starts with, in order. -/
def leadingSettings : List Ast.Node → List (String × Bool)
  | .Setting n on :: ns => (n.toUpper, on) :: leadingSettings ns
  | _ => []

/-- The T-SQL routine aborts on every error: `SET XACT_ABORT ON` comes
before any statement that could fail. -/
def abortsT : Ast.Routine → Bool
  | .Routine _ _ _ _ _ _ (.Block _ body) => (leadingSettings body).contains ("XACT_ABORT", true)

end Sqldiff.Mirror
