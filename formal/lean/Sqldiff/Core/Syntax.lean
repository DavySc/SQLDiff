/-
The core language, phase C1 ("Skeleton") of docs/verified-conversion.md.

Decision 9 there: one typed core with one semantics, and each dialect's
meaning *is* its lowering into this core (`Sqldiff.Mirror.lowerT`,
`Sqldiff.Mirror.lowerP`). C1 is the control skeleton of a routine:
parameters with modes and defaults, local declarations, assignment, `if`,
`while`, `return`, blocks and calls. There are no errors, no transactions
(C4) and no data (C2): an expression is a variable, an integer literal,
NULL, or an operator applied to expressions, and what an operator means
is a parameter of the semantics (`ExprSem.op`).
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

/-- C1 expressions. `op f args` applies the operator named `f` (`bin:+`,
`fn:coalesce`, ...) to its arguments. The core does not know what `f`
means: theorems hold for every meaning (`ExprSem.op`). **The same `f` on
both sides of a conversion is taken to mean the same function.** That is
true of the core's own vocabulary and not yet shown for the operators of
two dialects; giving operators their per-dialect meaning is C2. -/
inductive Expr where
  | var (x : Var)
  | lit (n : Int)
  | null
  | op (f : String) (args : List Expr)
deriving Repr

-- `deriving DecidableEq` does not handle the nested `List Expr`.
mutual
  def Expr.decEq : (a b : Expr) → Decidable (a = b)
    | .var x, .var y =>
      if h : x = y then isTrue (by rw [h]) else isFalse (by intro e; cases e; exact h rfl)
    | .lit m, .lit n =>
      if h : m = n then isTrue (by rw [h]) else isFalse (by intro e; cases e; exact h rfl)
    | .null, .null => isTrue rfl
    | .op f xs, .op g ys =>
      if hf : f = g then
        match Expr.decEqList xs ys with
        | isTrue h => isTrue (by rw [hf, h])
        | isFalse h => isFalse (by intro e; cases e; exact h rfl)
      else isFalse (by intro e; cases e; exact hf rfl)
    | .var _, .lit _ | .var _, .null | .var _, .op .. => isFalse (by intro e; cases e)
    | .lit _, .var _ | .lit _, .null | .lit _, .op .. => isFalse (by intro e; cases e)
    | .null, .var _ | .null, .lit _ | .null, .op .. => isFalse (by intro e; cases e)
    | .op .., .var _ | .op .., .lit _ | .op .., .null => isFalse (by intro e; cases e)

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

/-- C1 statements. A block is a sequence (`seq`); the empty block is
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
order; every variable starts as NULL (see `Sqldiff.Core.initStore`), so in
C1 the list has no run-time effect and is kept so that the check compares
the declarations too. The routine's own name is not here: a caller
observes nothing by it, and which name reaches which routine is the
deployment's business (`Sqldiff.Mirror.Deployment`). -/
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

mutual
  def Expr.vars : Expr → List Var
    | .var x => [x]
    | .lit _ => []
    | .null => []
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

mutual
  def Expr.rename (f : Var → Var) : Expr → Expr
    | .var x => .var (f x)
    | .lit n => .lit n
    | .null => .null
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

end Sqldiff.Core
