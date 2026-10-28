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
of the semantics (`ExprSem.op`). There are no transactions (C4) and no
data (C3).
-/

namespace Sqldiff.Core

/-- A variable name, as the lowering spells it. -/
abbrev Var := String

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
`Sqldiff.Core.WorldModel`); they are never inlined. -/
inductive Stmt where
  | skip
  | assign (x : Var) (e : Expr)
  | seq (s t : Stmt)
  | ite (c : Expr) (s t : Stmt)
  | while (c : Expr) (body : Stmt)
  | ret (e : Option Expr)
  | call (proc : String) (args : List Arg)
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

def Stmt.vars : Stmt → List Var
  | .skip => []
  | .assign x e => x :: e.vars
  | .seq s t => s.vars ++ t.vars
  | .ite c s t => c.vars ++ s.vars ++ t.vars
  | .while c b => c.vars ++ b.vars
  | .ret none => []
  | .ret (some e) => e.vars
  | .call _ args => (args.map Arg.vars).flatten

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

/-- Every expression of a routine: parameter defaults and the body's. -/
def Routine.exprs (r : Routine) : List Expr :=
  r.params.filterMap Param.default ++ r.body.exprs

/-- The expression contains an operator that can raise an error:
arithmetic or negation (overflow, division by zero). Conservative: a
literal divisor or a constant operand does not count as safe. -/
def SExpr.mayRaise : SExpr → Bool
  | .var _ | .int _ | .null => false
  | .arith .. | .neg _ => true
  | .cmp _ a b | .and a b | .or a b | .coalesce a b => a.mayRaise || b.mayRaise
  | .not a | .isNull a _ => a.mayRaise
  | .cond c a b => c.mayRaise || a.mayRaise || b.mayRaise

/-- The expression compares with `=` or `<>`, the comparisons T-SQL's
`ANSI_NULLS OFF` changes. -/
def SExpr.hasEqNe : SExpr → Bool
  | .var _ | .int _ | .null => false
  | .cmp o a b => o == .eq || o == .ne || a.hasEqNe || b.hasEqNe
  | .arith _ a b | .and a b | .or a b | .coalesce a b => a.hasEqNe || b.hasEqNe
  | .neg a | .not a | .isNull a _ => a.hasEqNe
  | .cond c a b => c.hasEqNe || a.hasEqNe || b.hasEqNe

/-- An uninterpreted operator counts as raising and as comparing: what it
does is unknown. -/
def Expr.test (p : SExpr → Bool) : Expr → Bool
  | .sc s => p s
  | .op .. => true

def Routine.mayRaise (r : Routine) : Bool := r.exprs.any (Expr.test SExpr.mayRaise)

def Routine.hasEqNe (r : Routine) : Bool := r.exprs.any (Expr.test SExpr.hasEqNe)

end Sqldiff.Core
