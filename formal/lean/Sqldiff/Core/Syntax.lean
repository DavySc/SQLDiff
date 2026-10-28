/-
The core language: phase C1 ("Skeleton") and the first slice of phase C2
("Scalars", C2a) of docs/verified-conversion.md.

Decision 9 there: one typed core with one semantics, and each dialect's
meaning *is* its lowering into this core (`Sqldiff.Mirror.lowerT`,
`Sqldiff.Mirror.lowerP`). C1 is the control skeleton of a routine:
parameters with modes and defaults, local declarations, assignment, `if`,
`while`, `return`, blocks and calls. C2a gives expressions of one slice a
meaning of their own (`SExpr`): NULL, three-valued booleans and 32-bit
integers with their operators. Every other expression is still an
operator applied to expressions (`Expr.op`), whose meaning is a parameter
of the semantics (`ExprSem.op`). Phase C3a ("Statements", mirror depth)
adds data: tables of rows of values, described by the target catalog
(`TableDef`), and three single-table statements, `insert`, `update` and
`delete`, whose expressions may read the columns of the row at hand
(`SExpr.col`). There are no transactions yet (C4).
-/

namespace Sqldiff.Core

/-- A variable name, as the lowering spells it. -/
abbrev Var := String

/-- A column, as the target catalog spells it (PostgreSQL's folded name,
`Sqldiff.Mirror.pgName`). Columns are never renamed: both sides name the
catalog's column. -/
abbrev Col := String

/-- A column of a table in the C3a slice: a 32-bit integer (T-SQL `int`,
PostgreSQL `integer`), `NOT NULL` or nullable, and whether it is part of
the primary key (a key column is `NOT NULL` whatever `notNull` says). -/
structure ColDef where
  name : Col
  notNull : Bool
  key : Bool
deriving DecidableEq, Repr

/-- A key column, or a `NOT NULL` one: NULL there is a constraint
violation. -/
def ColDef.required (c : ColDef) : Bool := c.notNull || c.key

/-- A table: its catalog name and its columns in declaration order. A row
of the table lists one value per column, in this order. -/
structure TableDef where
  name : String
  cols : List ColDef
deriving DecidableEq, Repr

def TableDef.hasKey (t : TableDef) : Bool := t.cols.any (·.key)

/-- Parameter modes. T-SQL `OUTPUT` is `inout` (the callee sees the value
the caller passed); PostgreSQL `OUT` is `out` (the callee sees NULL). -/
inductive Mode where
  | inp
  | out
  | inout
deriving DecidableEq, Repr

/-! ## The C2a slice -/

/-- Arithmetic on 32-bit signed integers. `div` truncates toward zero and
`mod` takes the sign of the dividend (`Sqldiff.Core.arithInt`). -/
inductive AOp where
  | add
  | sub
  | mul
  | div
  | mod
deriving DecidableEq, Repr

/-- Comparisons of 32-bit integers; the result is three-valued. -/
inductive COp where
  | eq
  | ne
  | lt
  | gt
  | le
  | ge
deriving DecidableEq, Repr

/-- Expressions of the C2a slice. Their meaning is fixed by the core
(`Sqldiff.Core.SExpr.eval`), not a parameter: a variable, a 32-bit integer
literal, NULL, integer arithmetic and negation, comparisons, `AND`, `OR`,
`NOT`, `IS [NOT] NULL` (`neg` is `IS NOT NULL`), `coalesce a b` (the
second argument evaluated only when the first is NULL) and `cond c a b`
(`CASE WHEN c THEN a ELSE b END`: `a` when `c` is TRUE, else `b`).
`col c` reads column `c` of the row a C3a statement is at (NULL outside
one): it is not a variable, so it is never renamed.

C2b adds strings: `str s` is a string literal, `concat a b` concatenation
(NULL if either operand is NULL), `rtrim a` the string without its
trailing spaces (U+0020 only), `streq a b` three-valued equality of two
strings as sequences of characters (no collation, no padding: the T-SQL
lowering puts the padding in with `rtrim`), and `len a` the number of
characters. Which dialect construct lowers to which is the lowering's
business (`Sqldiff.Mirror`, "The C2b slice: strings").

The core does not type them; the lowerings build them only from
expressions they have typed (`Sqldiff.Mirror.typed`). A variable read is
always `var`, whatever the variable's type, so a routine outside the slice
lowers as it did in C1. -/
inductive SExpr where
  | var (x : Var)
  | int (n : Int)
  | null
  | arith (o : AOp) (a b : SExpr)
  | neg (a : SExpr)
  | cmp (o : COp) (a b : SExpr)
  | and (a b : SExpr)
  | or (a b : SExpr)
  | not (a : SExpr)
  | isNull (a : SExpr) (neg : Bool)
  | coalesce (a b : SExpr)
  | cond (c a b : SExpr)
  | col (c : Col)
  | str (s : String)
  | concat (a b : SExpr)
  | rtrim (a : SExpr)
  | streq (a b : SExpr)
  | len (a : SExpr)
deriving DecidableEq, Repr

/-- Expressions. `sc s` is an expression of the slice. `op f args`
applies the operator named `f` (`bin:+`, `fn:coalesce`, ...) to its
arguments: the C1 path, for everything outside the slice. The core does
not know what `f` means: theorems hold for every meaning (`ExprSem.op`).
**The same `f` on both sides of a conversion is taken to mean the same
function** (`ASM-C1-OPERATORS` on the Rust side). A routine whose lowered
expressions contain no `op` does not depend on that
(`Sqldiff.Core.Routine.obs_opFree`). -/
inductive Expr where
  | sc (s : SExpr)
  | op (f : String) (args : List Expr)
deriving Repr

/-- A variable read. -/
def Expr.var (x : Var) : Expr := .sc (.var x)
/-- An integer literal. -/
def Expr.lit (n : Int) : Expr := .sc (.int n)
/-- NULL. -/
def Expr.null : Expr := .sc .null

-- `deriving DecidableEq` does not handle the nested `List Expr`.
mutual
  def Expr.decEq : (a b : Expr) → Decidable (a = b)
    | .sc s, .sc t =>
      if h : s = t then isTrue (by rw [h]) else isFalse (by intro e; cases e; exact h rfl)
    | .op f xs, .op g ys =>
      if hf : f = g then
        match Expr.decEqList xs ys with
        | isTrue h => isTrue (by rw [hf, h])
        | isFalse h => isFalse (by intro e; cases e; exact h rfl)
      else isFalse (by intro e; cases e; exact hf rfl)
    | .sc _, .op .. => isFalse (by intro e; cases e)
    | .op .., .sc _ => isFalse (by intro e; cases e)

  def Expr.decEqList : (xs ys : List Expr) → Decidable (xs = ys)
    | [], [] => isTrue rfl
    | x :: xs, y :: ys =>
      match Expr.decEq x y, Expr.decEqList xs ys with
      | isTrue h1, isTrue h2 => isTrue (by rw [h1, h2])
      | isFalse h, _ => isFalse (by intro e; cases e; exact h rfl)
      | _, isFalse h => isFalse (by intro e; cases e; exact h rfl)
    | [], _ :: _ => isFalse (by intro e; cases e)
    | _ :: _, [] => isFalse (by intro e; cases e)
end

instance : DecidableEq Expr := Expr.decEq

/-- An argument of a call: a value, or a variable the callee writes back
(T-SQL `@x OUTPUT`; a variable in an `OUT`/`INOUT` position of a
PostgreSQL `CALL`). A `ref` argument passes the variable's current value. -/
inductive Arg where
  | val (e : Expr)
  | ref (x : Var)
deriving DecidableEq, Repr

/-- Statements. A block is a sequence (`seq`); the empty block is
`skip`. Calls name another routine, which the world runs (see
`Sqldiff.Core.WorldModel`); they are never inlined.

The data statements of C3a carry the definition of the table they write
(from the catalog, the same on both sides):

* `insert t cs rows`: one row per element of `rows`, the values of the
  columns `cs` in order (`INSERT INTO t (cs) VALUES (...), ...`); every
  other column is NULL. The expressions see no columns.
* `update t sets c`: each row where `c` is TRUE (every row when `c` is
  `none`) gets, for each `(col, e)` of `sets`, the value of `e`
  evaluated on the row's old values (`UPDATE t SET col = e, ... WHERE c`).
* `delete t c`: removes each row where `c` is TRUE (every row when
  `none`).

Their meaning is `Sqldiff.Core.insertRows`, `updateRows` and
`deleteRows`. -/
inductive Stmt where
  | skip
  | assign (x : Var) (e : Expr)
  | seq (s t : Stmt)
  | ite (c : Expr) (s t : Stmt)
  | while (c : Expr) (body : Stmt)
  | ret (e : Option Expr)
  | call (proc : String) (args : List Arg)
  | insert (t : TableDef) (cols : List Col) (rows : List (List Expr))
  | update (t : TableDef) (sets : List (Col × Expr)) (cond : Option Expr)
  | delete (t : TableDef) (cond : Option Expr)
deriving DecidableEq, Repr

/-- A parameter: name, mode, and the default used when the caller passes
no argument for it. -/
structure Param where
  name : Var
  mode : Mode
  default : Option Expr
deriving DecidableEq, Repr

/-- A routine. `locals` are the declared local variables, in declaration
order; every variable starts as NULL (see `Sqldiff.Core.initStore`), so
the list has no run-time effect and is kept so that the check compares
the declarations too. Types are not here: the lowerings use them to
decide which expressions are in the slice, and they are not observed. The
routine's own name is not here either: a caller observes nothing by it,
and which name reaches which routine is the deployment's business
(`Sqldiff.Mirror.Deployment`). -/
structure Routine where
  params : List Param
  locals : List Var
  body : Stmt
deriving DecidableEq, Repr

/-- A list of statements as one statement: `skip` when empty, the
statement itself when there is one, otherwise right-nested `seq`. -/
def seqList : List Stmt → Stmt
  | [] => .skip
  | [s] => s
  | s :: ss => .seq s (seqList ss)

/-! ## Variables and renaming -/

def SExpr.vars : SExpr → List Var
  | .var x => [x]
  | .int _ => []
  | .null => []
  | .arith _ a b => a.vars ++ b.vars
  | .neg a => a.vars
  | .cmp _ a b => a.vars ++ b.vars
  | .and a b => a.vars ++ b.vars
  | .or a b => a.vars ++ b.vars
  | .not a => a.vars
  | .isNull a _ => a.vars
  | .coalesce a b => a.vars ++ b.vars
  | .cond c a b => c.vars ++ a.vars ++ b.vars
  | .col _ => []
  | .str _ => []
  | .concat a b => a.vars ++ b.vars
  | .rtrim a => a.vars
  | .streq a b => a.vars ++ b.vars
  | .len a => a.vars

mutual
  def Expr.vars : Expr → List Var
    | .sc s => s.vars
    | .op _ args => Expr.varsList args

  def Expr.varsList : List Expr → List Var
    | [] => []
    | e :: es => e.vars ++ Expr.varsList es
end

def Arg.vars : Arg → List Var
  | .val e => e.vars
  | .ref x => [x]

/-- The variables of an optional expression. -/
def Expr.varsOpt : Option Expr → List Var
  | some e => e.vars
  | none => []

def Stmt.vars : Stmt → List Var
  | .skip => []
  | .assign x e => x :: e.vars
  | .seq s t => s.vars ++ t.vars
  | .ite c s t => c.vars ++ s.vars ++ t.vars
  | .while c b => c.vars ++ b.vars
  | .ret none => []
  | .ret (some e) => e.vars
  | .call _ args => (args.map Arg.vars).flatten
  | .insert _ _ rows => (rows.map fun es => (es.map Expr.vars).flatten).flatten
  | .update _ sets c => (sets.map fun p => p.2.vars).flatten ++ Expr.varsOpt c
  | .delete _ c => Expr.varsOpt c

def Param.vars (p : Param) : List Var :=
  p.name :: (match p.default with | some e => e.vars | none => [])

/-- Every variable a routine names: parameters (and their defaults),
locals and the body's. -/
def Routine.vars (r : Routine) : List Var :=
  (r.params.map Param.vars).flatten ++ r.locals ++ r.body.vars

def SExpr.rename (f : Var → Var) : SExpr → SExpr
  | .var x => .var (f x)
  | .int n => .int n
  | .null => .null
  | .arith o a b => .arith o (a.rename f) (b.rename f)
  | .neg a => .neg (a.rename f)
  | .cmp o a b => .cmp o (a.rename f) (b.rename f)
  | .and a b => .and (a.rename f) (b.rename f)
  | .or a b => .or (a.rename f) (b.rename f)
  | .not a => .not (a.rename f)
  | .isNull a n => .isNull (a.rename f) n
  | .coalesce a b => .coalesce (a.rename f) (b.rename f)
  | .cond c a b => .cond (c.rename f) (a.rename f) (b.rename f)
  | .col c => .col c
  | .str s => .str s
  | .concat a b => .concat (a.rename f) (b.rename f)
  | .rtrim a => .rtrim (a.rename f)
  | .streq a b => .streq (a.rename f) (b.rename f)
  | .len a => .len (a.rename f)

mutual
  def Expr.rename (f : Var → Var) : Expr → Expr
    | .sc s => .sc (s.rename f)
    | .op g args => .op g (Expr.renameList f args)

  def Expr.renameList (f : Var → Var) : List Expr → List Expr
    | [] => []
    | e :: es => e.rename f :: Expr.renameList f es
end

def Arg.rename (f : Var → Var) : Arg → Arg
  | .val e => .val (e.rename f)
  | .ref x => .ref (f x)

def Stmt.rename (f : Var → Var) : Stmt → Stmt
  | .skip => .skip
  | .assign x e => .assign (f x) (e.rename f)
  | .seq s t => .seq (s.rename f) (t.rename f)
  | .ite c s t => .ite (c.rename f) (s.rename f) (t.rename f)
  | .while c b => .while (c.rename f) (b.rename f)
  | .ret e => .ret (e.map (Expr.rename f))
  | .call p args => .call p (args.map (Arg.rename f))
  | .insert t cs rows => .insert t cs (rows.map (·.map (Expr.rename f)))
  | .update t sets c => .update t (sets.map fun p => (p.1, p.2.rename f)) (c.map (Expr.rename f))
  | .delete t c => .delete t (c.map (Expr.rename f))

def Param.rename (f : Var → Var) (p : Param) : Param :=
  { p with name := f p.name, default := p.default.map (Expr.rename f) }

/-- Renames every variable; callee names stay. -/
def Routine.rename (f : Var → Var) (r : Routine) : Routine :=
  { r with
    params := r.params.map (Param.rename f)
    locals := r.locals.map f
    body := r.body.rename f }

/-! ## What a routine's expressions use

`opFree`: no uninterpreted operator, so the routine's meaning does not
depend on `ExprSem.op` (`Sqldiff.Core.Routine.obs_opFree`). `mayRaise` and
`hasEqNe` decide which residual assumptions a C2a confirmation carries
(`Sqldiff.Mirror.residual`). -/

def Expr.opFree : Expr → Bool
  | .sc _ => true
  | .op .. => false

def Arg.opFree : Arg → Bool
  | .val e => e.opFree
  | .ref _ => true

def Stmt.opFree : Stmt → Bool
  | .skip => true
  | .assign _ e => e.opFree
  | .seq s t => s.opFree && t.opFree
  | .ite c s t => c.opFree && s.opFree && t.opFree
  | .while c b => c.opFree && b.opFree
  | .ret none => true
  | .ret (some e) => e.opFree
  | .call _ args => args.all Arg.opFree
  | .insert _ _ rows => rows.all (·.all Expr.opFree)
  | .update _ sets c => sets.all (·.2.opFree) && c.all Expr.opFree
  | .delete _ c => c.all Expr.opFree

def Param.opFree (p : Param) : Bool :=
  match p.default with
  | some e => e.opFree
  | none => true

def Routine.opFree (r : Routine) : Bool :=
  r.params.all Param.opFree && r.body.opFree

/-- Every expression of a statement, in order. -/
def Stmt.exprs : Stmt → List Expr
  | .skip => []
  | .assign _ e => [e]
  | .seq s t => s.exprs ++ t.exprs
  | .ite c s t => c :: s.exprs ++ t.exprs
  | .while c b => c :: b.exprs
  | .ret none => []
  | .ret (some e) => [e]
  | .call _ args => args.filterMap fun | .val e => some e | .ref _ => none
  | .insert _ _ rows => rows.flatten
  | .update _ sets c => sets.map (·.2) ++ c.toList
  | .delete _ c => c.toList

/-- Every expression of a routine: parameter defaults and the body's. -/
def Routine.exprs (r : Routine) : List Expr :=
  r.params.filterMap Param.default ++ r.body.exprs

/-- The expression contains an operator that can raise an error:
arithmetic or negation (overflow, division by zero). Conservative: a
literal divisor or a constant operand does not count as safe. -/
def SExpr.mayRaise : SExpr → Bool
  | .var _ | .int _ | .null | .col _ | .str _ => false
  | .arith .. | .neg _ => true
  | .cmp _ a b | .and a b | .or a b | .coalesce a b | .concat a b | .streq a b =>
    a.mayRaise || b.mayRaise
  | .not a | .isNull a _ | .rtrim a | .len a => a.mayRaise
  | .cond c a b => c.mayRaise || a.mayRaise || b.mayRaise

/-- The expression compares with `=` or `<>`, the comparisons T-SQL's
`ANSI_NULLS OFF` changes. -/
def SExpr.hasEqNe : SExpr → Bool
  | .var _ | .int _ | .null | .col _ | .str _ => false
  | .cmp o a b => o == .eq || o == .ne || a.hasEqNe || b.hasEqNe
  | .streq _ _ => true
  | .arith _ a b | .and a b | .or a b | .coalesce a b | .concat a b => a.hasEqNe || b.hasEqNe
  | .neg a | .not a | .isNull a _ | .rtrim a | .len a => a.hasEqNe
  | .cond c a b => c.hasEqNe || a.hasEqNe || b.hasEqNe

/-- Some subterm (the expression itself included) satisfies `p`. -/
def SExpr.anySub (p : SExpr → Bool) : SExpr → Bool
  | s@(.var _) | s@(.int _) | s@.null | s@(.col _) | s@(.str _) => p s
  | s@(.arith _ a b) | s@(.cmp _ a b) | s@(.and a b) | s@(.or a b) | s@(.coalesce a b)
  | s@(.concat a b) | s@(.streq a b) => p s || a.anySub p || b.anySub p
  | s@(.neg a) | s@(.not a) | s@(.isNull a _) | s@(.rtrim a) | s@(.len a) => p s || a.anySub p
  | s@(.cond c a b) => p s || c.anySub p || a.anySub p || b.anySub p

/-- The expression concatenates strings (C2b: `ASM-CONCAT-NULL`). -/
def SExpr.hasConcat : SExpr → Bool := SExpr.anySub fun | .concat .. => true | _ => false

/-- The expression compares strings (C2b: `ASM-BINARY-COLLATION`). -/
def SExpr.hasStrEq : SExpr → Bool := SExpr.anySub fun | .streq .. => true | _ => false

/-- The expression counts characters (C2b: `ASM-TSQL-LEN-BMP`). -/
def SExpr.hasLen : SExpr → Bool := SExpr.anySub fun | .len _ => true | _ => false

/-- An uninterpreted operator counts as raising and as comparing: what it
does is unknown. -/
def Expr.test (p : SExpr → Bool) : Expr → Bool
  | .sc s => p s
  | .op .. => true

def Routine.mayRaise (r : Routine) : Bool := r.exprs.any (Expr.test SExpr.mayRaise)

def Routine.hasEqNe (r : Routine) : Bool := r.exprs.any (Expr.test SExpr.hasEqNe)

def Routine.hasConcat (r : Routine) : Bool := r.exprs.any (Expr.test SExpr.hasConcat)

def Routine.hasStrEq (r : Routine) : Bool := r.exprs.any (Expr.test SExpr.hasStrEq)

def Routine.hasLen (r : Routine) : Bool := r.exprs.any (Expr.test SExpr.hasLen)

/-! ## What a routine's data statements can do (C3a)

These decide whether a confirmation is in the slice
(`Sqldiff.Mirror.inC2a`) and which residual assumptions it carries
(`Sqldiff.Mirror.residual`). All are conservative: `true` means "might". -/

/-- The expression reads a column. -/
def SExpr.hasCol : SExpr → Bool
  | .var _ | .int _ | .null | .str _ => false
  | .col _ => true
  | .arith _ a b | .cmp _ a b | .and a b | .or a b | .coalesce a b | .concat a b
  | .streq a b => a.hasCol || b.hasCol
  | .neg a | .not a | .isNull a _ | .rtrim a | .len a => a.hasCol
  | .cond c a b => c.hasCol || a.hasCol || b.hasCol

/-- A part of the expression that reads no column can raise. PostgreSQL
plans a data statement of PL/pgSQL with the variables' values as
constants and may fold such a part while planning, so it can raise even
when no row is written (`Sqldiff.Mirror`, "Constant parts of a data
statement"). For an expression without columns this is `mayRaise`. -/
def SExpr.constRaise : SExpr → Bool
  | .var _ | .int _ | .null | .col _ | .str _ => false
  | .arith _ a b => if a.hasCol || b.hasCol then a.constRaise || b.constRaise else true
  | .neg a => if a.hasCol then a.constRaise else true
  | .cmp _ a b | .and a b | .or a b | .coalesce a b | .concat a b | .streq a b =>
    a.constRaise || b.constRaise
  | .not a | .isNull a _ | .rtrim a | .len a => a.constRaise
  | .cond c a b => c.constRaise || a.constRaise || b.constRaise

/-- The expression can raise; an uninterpreted operator counts. -/
def Expr.raise (e : Expr) : Bool := Expr.test SExpr.mayRaise e

/-- Column `c` of `t` is `NOT NULL` or a key column. -/
def TableDef.requires (t : TableDef) (c : Col) : Bool :=
  t.cols.any fun d => d.name == c && d.required

/-- The value is never NULL on a row of `t` whose required columns are
not NULL (a state that satisfies the catalog): an integer literal, a
required column, arithmetic on such values (which may raise, but not give
NULL), and `COALESCE`/`CASE` whose result is one of them. -/
def SExpr.nonNull (t : TableDef) : SExpr → Bool
  | .int _ => true
  | .col c => t.requires c
  | .arith _ a b => a.nonNull t && b.nonNull t
  | .neg a => a.nonNull t
  | .coalesce a b => a.nonNull t || b.nonNull t
  | .cond _ a b => a.nonNull t && b.nonNull t
  | _ => false

def Expr.nonNull (t : TableDef) : Expr → Bool
  | .sc s => s.nonNull t
  | .op .. => false

/-- The statement can violate a constraint (`NOT NULL`, primary key): an
`INSERT` into a table with a required column, an `UPDATE` that sets one
to a value that may be NULL (key columns are never set, `Sqldiff.Mirror`).
-/
def Stmt.violates : Stmt → Bool
  | .insert t _ _ => t.cols.any ColDef.required
  | .update t sets _ => sets.any fun p => t.requires p.1 && !p.2.nonNull t
  | _ => false

/-- The statement itself (not the statements inside it) can fail. -/
def Stmt.failsHere : Stmt → Bool
  | .assign _ e => e.raise
  | .ret (some e) => e.raise
  | .ite c .. => c.raise
  | .while c _ => c.raise
  | .call _ args => args.any fun | .val e => e.raise | .ref _ => false
  | .insert t cs rows => (Stmt.insert t cs rows).violates || rows.flatten.any Expr.raise
  | .update t sets c =>
    (Stmt.update t sets c).violates || sets.any (fun p => p.2.raise) || c.any Expr.raise
  | .delete _ c => c.any Expr.raise
  | _ => false

/-- Some statement of `s` can fail. -/
def Stmt.mayFail : Stmt → Bool
  | .seq s t => s.mayFail || t.mayFail
  | .ite c a b => c.raise || a.mayFail || b.mayFail
  | .while c b => c.raise || b.mayFail
  | s => s.failsHere

/-- `s` may write: a data statement, or a call (the callee may write). -/
def Stmt.hasWrite : Stmt → Bool
  | .insert .. | .update .. | .delete .. | .call .. => true
  | .seq s t | .ite _ s t => s.hasWrite || t.hasWrite
  | .while _ b => b.hasWrite
  | _ => false

/-- `s` has a data statement. -/
def Stmt.hasData : Stmt → Bool
  | .insert .. | .update .. | .delete .. => true
  | .seq s t | .ite _ s t => s.hasData || t.hasData
  | .while _ b => b.hasData
  | _ => false

/-- Some statement of `s` can fail after a write has happened; `w` says
whether one may have happened before `s`. A failing statement undoes its
own effect, so a data statement that fails counts only after an earlier
write. -/
def Stmt.errAfterWrite (w : Bool) : Stmt → Bool
  | .seq s t => s.errAfterWrite w || t.errAfterWrite (w || s.hasWrite)
  | .ite c a b => (w && c.raise) || a.errAfterWrite w || b.errAfterWrite w
  | .while c b =>
    let w' := w || b.hasWrite
    (w' && c.raise) || b.errAfterWrite w'
  | s => w && s.failsHere

/-- The data statements are in the C3a slice (`Sqldiff.Mirror`, "The C3a
slice: data statements"): a `WHERE` cannot raise; a statement cannot fail
both with an arithmetic error and with a constraint violation (which of
the two it reports then depends on the order rows are processed in,
which neither vendor fixes); no part of an `UPDATE`'s values that reads no
column can raise (`SExpr.constRaise`). -/
def Stmt.dmlSafe : Stmt → Bool
  | .insert t cs rows =>
    !((Stmt.insert t cs rows).violates && rows.flatten.any Expr.raise)
  | .update t sets c =>
    !(c.any Expr.raise) && !(sets.any fun p => Expr.test SExpr.constRaise p.2) &&
      !((Stmt.update t sets c).violates && sets.any fun p => p.2.raise)
  | .delete _ c => !(c.any Expr.raise)
  | .seq s t | .ite _ s t => s.dmlSafe && t.dmlSafe
  | .while _ b => b.dmlSafe
  | _ => true

/-- Some run of the routine might fail: an expression can raise, or a
data statement can violate a constraint. -/
def Routine.mayFail (r : Routine) : Bool :=
  r.params.any (fun p => p.default.any Expr.raise) || r.body.mayFail

def Routine.errAfterWrite (r : Routine) : Bool := r.body.errAfterWrite false

def Routine.hasData (r : Routine) : Bool := r.body.hasData

def Routine.dmlSafe (r : Routine) : Bool := r.body.dmlSafe

end Sqldiff.Core
