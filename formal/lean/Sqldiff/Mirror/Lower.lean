/-
The lowering of T-SQL and PL/pgSQL routines (`Sqldiff.Mirror.Ast`) into
the core. Decision 9 (docs/verified-conversion.md): this file *is* the
specification of the C1 constructs in both dialects; there is no other
T-SQL or PL/pgSQL semantics to prove it against. It is partial: `none`
means "outside the C1 spec", and a routine outside it is never confirmed.

C1 has no errors: every rule describes a run that raises none. Where the
engine would raise one (a missing argument without a default, `OUTPUT` on
a parameter that is not one), the rule says what happens otherwise, and
errors are C4. No data either: names are variables, never columns.

Read top to bottom: the trusted inputs, expressions, statements, calls,
blocks, and the two routines.
-/
import Sqldiff.Core.Syntax
import Sqldiff.Mirror.Ast
import Sqldiff.Mirror.Names

namespace Sqldiff.Mirror

open Sqldiff.Core (Var Stmt Arg seqList)

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

/-! ## Expressions

Operators keep their name, lower-cased, with a prefix for the syntactic
form: `bin:+`, `un:-`, `fn:coalesce`. The core leaves them uninterpreted,
so a confirmation is relative to **the same operator name meaning the
same function in both dialects**, which C2 replaces with per-dialect
meanings and mirror lemmas. T-SQL `@a + @b` and PostgreSQL `a + b` agree
in C1; whether they agree on strings, NULL and overflow is C2. -/

mutual
  def lowerExpr (d : Dialect) : Ast.Expr → Option Core.Expr
    | .var v => match d with
      | .tsql _ => some (.var v)
      | .pg _ => none
    | .name [x] => match d with
      | .pg _ => some (.var x)
      | .tsql _ => none -- a column: data, C3
    | .name _ => none
    | .num s => s.toInt?.map .lit -- integers only; decimals are C2
    | .str _ => none -- collation and padding: C2
    | .null => some .null
    | .call n args => do
      let args' ← lowerExprs d args
      pure (.op ("fn:" ++ (dotted n).toLower) args')
    | .bin o a b => do
      let a' ← lowerExpr d a
      let b' ← lowerExpr d b
      pure (.op ("bin:" ++ o.toLower) [a', b'])
    | .unary o a => do
      let a' ← lowerExpr d a
      pure (.op ("un:" ++ o.toLower) [a'])

  def lowerExprs (d : Dialect) : List Ast.Expr → Option (List Core.Expr)
    | [] => some []
    | e :: es => do
      let e' ← lowerExpr d e
      let es' ← lowerExprs d es
      pure (e' :: es')
end

/-- The variable an assignment or an `OUTPUT` argument names. -/
def target : Dialect → Ast.Expr → Option Var
  | .tsql _, .var v => some v
  | .pg _, .name [x] => some x
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
`status := 0`, and `RETURN e` is `status := coalesce(e, 0); return`. So a
caller observes the status as the last out value. Its entry value is
never read, so its mode and default have no effect; they are `inout` and
`NULL` to match the converter's `INOUT return_code integer DEFAULT NULL`.
The PL/pgSQL mirror must therefore set `return_code := 0` first and write
`return_code := COALESCE(e, 0)`: a bare `return_code := e` differs when
`e` is NULL, and without the first assignment a procedure that ends
without `RETURN e` leaves the caller's value (NULL by default), not 0. -/

def statusVar : Var := "%status"

def statusParam : Core.Param := ⟨statusVar, .inout, some .null⟩

/-! ## Statements -/

/-- One statement, as a list of core statements (a block contributes its
statements in place, so the result does not depend on how `BEGIN ... END`
nests).

* T-SQL `DECLARE @x t` does nothing at run time: every variable is NULL
  from the start of the routine, and a `DECLARE` inside a loop does not
  reset it. `DECLARE @x t = e` assigns, where it stands, each time it
  runs. So the PL/pgSQL mirror declares `x` at the top without a default
  and assigns `x := e` at the same position.
* PL/pgSQL declarations belong to the `DECLARE` section (`lowerP`).
* `RETURN e` in a function returns `e`; in a T-SQL procedure it sets the
  status (above); a PostgreSQL procedure cannot return a value. -/
def lowerStmt (d : Dialect) (k : Ast.RoutineKind) : Ast.Stmt → Option (List Stmt)
  | .assign t v => do
    let x ← target d t
    let v' ← lowerExpr d v
    pure [.assign x v']
  | .return_ none => some [.ret none]
  | .return_ (some e) => do
    let e' ← lowerExpr d e
    match d, k with
    | _, .function => pure [.ret (some e')]
    | .tsql _, .procedure => pure [.assign statusVar (.op "fn:coalesce" [e', .lit 0]), .ret none]
    | .pg _, .procedure => none
  | .declare x _ dflt => match d, dflt with
    | .tsql _, none => some []
    | .tsql _, some e => do
      let e' ← lowerExpr d e
      pure [.assign x e']
    | .pg _, _ => none

/-! ## Calls -/

/-- T-SQL `EXEC [@rc =] p a, @x OUTPUT`: positional arguments only; an
`OUTPUT` argument is a variable that receives the parameter's final
value; without `OUTPUT` nothing is copied back, even to an `OUTPUT`
parameter. `@rc =` receives the callee's status, which is the callee's
last parameter in the core (above), so it is one more `OUTPUT` argument.
The callee is renamed through the deployment. -/
def lowerTsqlArg (d : Dialect) (a : Ast.Arg) : Option Arg :=
  match a.name, a.output with
  | some _, _ => none -- named arguments: C1 binds by position
  | none, true => (target d a.value).map .ref
  | none, false => (lowerExpr d a.value).map .val

/-- PostgreSQL `CALL p(args)`: positional arguments only; an argument in
an `OUT` or `INOUT` position (the catalog says which) must be a variable,
and receives the parameter's final value; trailing parameters without an
argument take their defaults. -/
def lowerPgArg (d : Dialect) : Ast.Mode × Ast.Arg → Option Arg
  | (_, ⟨some _, _, _⟩) => none
  | (_, ⟨none, _, true⟩) => none -- `OUTPUT` is T-SQL syntax
  | (.in_, ⟨none, e, false⟩) => (lowerExpr d e).map .val
  | (_, ⟨none, e, false⟩) => (target d e).map .ref

def lowerCall (d : Dialect) (n : List String) (args : List Ast.Arg) (ret : Option Ast.Expr) :
    Option Stmt :=
  match d with
  | .tsql dep => do
    let callee ← lookup dep.routines (dotted n)
    let args' ← args.mapM (lowerTsqlArg d)
    let rc ← match ret with
      | none => some []
      | some e => (target d e).map fun x => [Arg.ref x]
    pure (.call callee (args' ++ rc))
  | .pg Γ => do
    let modes ← lookup Γ (dotted n)
    if args.length ≤ modes.length && ret.isNone then
      let args' ← (modes.zip args).mapM (lowerPgArg d)
      pure (.call (dotted n) args')
    else none

/-- `SET NOCOUNT ON|OFF` only changes rows-affected counts, of this
routine and of what it calls, so it is nothing when the observer does not
read them and outside C1 when it does. Other settings (`XACT_ABORT`, ...)
change error behaviour: C4. -/
def lowerSetting (d : Dialect) (name : String) : Option (List Stmt) :=
  match d with
  | .tsql dep => if name = "NOCOUNT" && !dep.rowCounts then some [] else none
  | .pg _ => none

/-! ## Blocks -/

mutual
  def lowerNode (d : Dialect) (k : Ast.RoutineKind) : Ast.Node → Option (List Stmt)
    | .stmt s => lowerStmt d k s
    | .block b => lowerInner d k b
    | .if_ c t e => do
      let c' ← lowerExpr d c
      let t' ← lowerNodes d k t
      let e' ← lowerNodes d k e
      pure [.ite c' (seqList t') (seqList e')]
    | .while c b => do
      let c' ← lowerExpr d c
      let b' ← lowerNodes d k b
      pure [.while c' (seqList b')]
    | .call n args ret => do
      let s ← lowerCall d n args ret
      pure [s]
    | .setting n _ => lowerSetting d n

  def lowerNodes (d : Dialect) (k : Ast.RoutineKind) : List Ast.Node → Option (List Stmt)
    | [] => some []
    | n :: ns => do
      let s ← lowerNode d k n
      let ss ← lowerNodes d k ns
      pure (s ++ ss)

  /-- A block inside the body. With declarations it is outside C1: a
  PL/pgSQL nested `DECLARE` re-initialises on every entry and can shadow.
  Without, it is its statements (T-SQL `BEGIN ... END` has none). -/
  def lowerInner (d : Dialect) (k : Ast.RoutineKind) : Ast.Block → Option (List Stmt)
    | .mk [] body => lowerNodes d k body
    | .mk (_ :: _) _ => none
end

/-- The names a list of statements declares, in order. -/
def declNames (ss : List Ast.Stmt) : List String :=
  ss.filterMap fun
    | .declare x _ _ => some x
    | _ => none

mutual
  /-- The names T-SQL `DECLARE`s anywhere in a node, in order. -/
  def declsNode : Ast.Node → List String
    | .stmt (.declare x _ _) => [x]
    | .block b => declsBlock b
    | .if_ _ t e => declsNodes t ++ declsNodes e
    | .while _ b => declsNodes b
    | .stmt _ | .call .. | .setting .. => []

  def declsNodes : List Ast.Node → List String
    | [] => []
    | n :: ns => declsNode n ++ declsNodes ns

  def declsBlock : Ast.Block → List String
    | .mk ds body => declNames ds ++ declsNodes body
end

/-! ## Routines -/

def lowerMode : Ast.Mode → Core.Mode
  | .in_ => .inp
  | .out => .out
  | .inOut => .inout

def lowerParam (d : Dialect) (p : Ast.Param) : Option Core.Param := do
  let dflt ← match p.default with
    | none => some none
    | some e => (lowerExpr d e).map some
  pure ⟨p.name, lowerMode p.mode, dflt⟩

/-- A T-SQL routine. Outside the spec, besides the statements above:

* a name declared twice, or a local with a parameter's name (SQL Server
  refuses both, error 134);
* two different names equal up to case, or a non-ASCII name: whether
  `@a` and `@A` are one variable depends on the instance's collation, so
  the lowering, which keeps names as written, is faithful only when no
  two names of the routine could be confused. -/
def lowerT (dep : Deployment) (a : Ast.Routine) : Option Core.Routine := do
  let d := Dialect.tsql dep
  let ps ← a.params.mapM (lowerParam d)
  let body ← lowerInner d a.kind a.body
  let (ps, body) : List Core.Param × List Stmt := match a.kind with
    | .procedure => (ps ++ [statusParam], Stmt.assign statusVar (.lit 0) :: body)
    | .function => (ps, body)
  let r : Core.Routine :=
    { params := ps, locals := declsBlock a.body, body := seqList body }
  if distinct (r.params.map (·.name) ++ r.locals) && caseSafe r.vars then some r else none

/-- A PL/pgSQL declaration `x t [:= e]`: the name, and the assignment the
block runs on entry, in declaration order, when there is a default. -/
def lowerDecl (d : Dialect) : Ast.Stmt → Option (Var × List Stmt)
  | .declare x _ none => some (x, [])
  | .declare x _ (some e) => (lowerExpr d e).map fun e' => (x, [.assign x e'])
  | _ => none

/-- A PL/pgSQL routine: one block, its `DECLARE` section, its statements.
Names are as PostgreSQL resolves them (unquoted folded to lower case by
the parser). Outside the spec, besides the statements above: a name
declared twice, or a local with a parameter's name (PostgreSQL lets the
local shadow the parameter; the core has one variable per name). -/
def lowerP (Γ : Catalog) (b : Ast.Routine) : Option Core.Routine := do
  let d := Dialect.pg Γ
  let ps ← b.params.mapM (lowerParam d)
  match b.body with
  | .mk decls stmts =>
    let ds ← decls.mapM (lowerDecl d)
    let body ← lowerNodes d b.kind stmts
    let r : Core.Routine :=
      { params := ps, locals := ds.map (·.1)
        body := seqList ((ds.map (·.2)).flatten ++ body) }
    if distinct (r.params.map (·.name) ++ r.locals) then some r else none

end Sqldiff.Mirror
