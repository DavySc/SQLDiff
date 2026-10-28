/-
The lowering of T-SQL and PL/pgSQL routines, as the Rust parser reads them
(`Sqldiff.Ast`, generated), into the core. Decision 9
(docs/verified-conversion.md): this file *is* the specification of the C1
constructs in both dialects; there is no other T-SQL or PL/pgSQL semantics
to prove it against. One abstract syntax serves both dialects and the
dialect is a parameter. The lowering is partial: `none` means "outside the
C1 spec" (queries, DML, errors, other settings, ...), and a routine outside
it is never confirmed.

C1 has no errors: every rule describes a run that raises none. Where the
engine would raise one (a missing argument without a default, `OUTPUT` on
a parameter that is not one), the rule says what happens otherwise, and
errors are C4. No data either: names are variables, never columns.

Read top to bottom: the trusted inputs, expressions, statements, calls,
blocks, and the two routines.
-/
import Sqldiff.Ast
import Sqldiff.Core.Syntax
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

/-- A PostgreSQL name as PostgreSQL resolves it. The Rust parser keeps
names as written, except that a quoted name whose case matters arrives
with a leading `"` (`"Foo"` as `"Foo`; a quoted all-lower-case name
arrives without the quote, `sqldiff_syntax::abstract_syntax::quoted_name`).
PostgreSQL folds unquoted names to lower case, so `X` and `x` are one
variable: a name with the leading `"` is kept, every other name is
lower-cased. PostgreSQL folds only ASCII letters, and so does
`String.toLower`; non-ASCII names are outside C1 anyway (`asciiNames`),
since an unquoted `Ä` and a quoted `"Ä"` are one name to PostgreSQL but
not to this rule. Catalog keys are names folded by this rule. -/
def pgName (n : String) : String :=
  if n.startsWith "\"" then n else n.toLower

/-! ## Expressions

A variable read is a core variable, renamed by the certificate. Integer
literals and NULL are core values. Every other expression is an operator
of the core, named by its syntactic form and its non-expression fields
(`bin:+`, `un:-`, `fn:coalesce`, `isnull:false`, `cast:int()`,
`str:abc`, ...), applied to its sub-expressions; distinct forms get
distinct names, so two expressions lower to the same term exactly when
they are the same expression up to the renaming of variables.

The core leaves operators uninterpreted, so a confirmation is relative to
**the same operator name meaning the same function in both dialects**,
which C2 replaces with per-dialect meanings and mirror lemmas. T-SQL
`@a + @b` and PostgreSQL `a + b` agree in C1; whether they agree on
strings, NULL and overflow is C2. Expressions that read data (subqueries,
`EXISTS`, `IN (SELECT ...)`, `*`, aggregates and window calls) are C3. -/

def flag (b : Bool) : String := if b then "1" else "0"

def tyName : Ast.Ty → String
  | .Ty n args => n.toLower ++ "(" ++ ",".intercalate args ++ ")"

mutual
  def lowerExpr (d : Dialect) : Ast.Expr → Option Core.Expr
    | .Var v => match d with
      | .tsql _ => if v.startsWith "@@" then none else some (.var v) -- `@@ERROR` etc.: C4
      | .pg _ => none
    | .Name [x] => match d with
      | .pg _ => some (.var (pgName x))
      | .tsql _ => none -- a column: data, C3
    | .Name _ => none
    | .Num s => some (match s.toInt? with
      | some n => .lit n
      | none => .op ("num:" ++ s) [])
    | .Str s => some (.op ("str:" ++ s) [])
    | .Null => some .null
    | .Star _ => none
    | .CallE (.CallF n args distinct order withinGroup over star) =>
      if distinct || !order.isEmpty || withinGroup || over || star then none
      else (lowerExprs d args).map (.op ("fn:" ++ (dotted n).toLower))
    | .Bin o a b => do
      let a' ← lowerExpr d a
      let b' ← lowerExpr d b
      pure (.op ("bin:" ++ o.toLower) [a', b'])
    | .Unary o a => (lowerExpr d a).map fun a' => .op ("un:" ++ o.toLower) [a']
    | .IsNull x neg => (lowerExpr d x).map fun x' => .op ("isnull:" ++ flag neg) [x']
    | .InList x xs neg => do
      let x' ← lowerExpr d x
      let xs' ← lowerExprs d xs
      pure (.op ("in:" ++ flag neg) (x' :: xs'))
    | .InQuery .. => none
    | .Between x lo hi neg => do
      let x' ← lowerExpr d x
      let lo' ← lowerExpr d lo
      let hi' ← lowerExpr d hi
      pure (.op ("between:" ++ flag neg) [x', lo', hi'])
    | .Like x p neg ci => do
      let x' ← lowerExpr d x
      let p' ← lowerExpr d p
      pure (.op ("like:" ++ flag neg ++ flag ci) [x', p'])
    | .Cast x ty => (lowerExpr d x).map fun x' => .op ("cast:" ++ tyName ty) [x']
    | .Case operand whens els => do
      -- `case:OE`: whether there is an operand and an `ELSE`; the `WHEN`
      -- pairs fill the arguments between them.
      let o' ← lowerOpt d operand
      let ws ← lowerWhens d whens
      let e' ← lowerOpt d els
      pure (.op ("case:" ++ flag o'.isSome ++ flag e'.isSome) (o'.toList ++ ws ++ e'.toList))
    | .Exists _ | .Subquery _ => none
    | .Tuple xs => (lowerExprs d xs).map (.op "tuple")
    | .Special n => some (.op ("special:" ++ n.toLower) [])
    | .Extract f x => (lowerExpr d x).map fun x' => .op ("extract:" ++ f.toLower) [x']

  def lowerExprs (d : Dialect) : List Ast.Expr → Option (List Core.Expr)
    | [] => some []
    | e :: es => do
      let e' ← lowerExpr d e
      let es' ← lowerExprs d es
      pure (e' :: es')

  /-- An optional part: `some none` when absent, `none` when unsupported. -/
  def lowerOpt (d : Dialect) : Option Ast.Expr → Option (Option Core.Expr)
    | none => some none
    | some e => (lowerExpr d e).map some

  def lowerWhens (d : Dialect) : List Ast.When2 → Option (List Core.Expr)
    | [] => some []
    | .When2 c r :: ws => do
      let c' ← lowerExpr d c
      let r' ← lowerExpr d r
      let ws' ← lowerWhens d ws
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
  status (above); a PostgreSQL procedure cannot return a value.
* Queries, DML and messages are outside C1. -/
def lowerStmt (d : Dialect) (k : Ast.RoutineKind) : Ast.Stmt → Option (List Stmt)
  | .Assign t v => do
    let x ← target d t
    let v' ← lowerExpr d v
    pure [.assign x v']
  | .Return none => some [.ret none]
  | .Return (some e) => do
    let e' ← lowerExpr d e
    match d, k with
    | _, .Function => pure [.ret (some e')]
    | .tsql _, .Procedure => pure [.assign statusVar (.op "fn:coalesce" [e', .lit 0]), .ret none]
    | .pg _, .Procedure => none
  | .Declare x _ dflt => match d, dflt with
    | .tsql _, none => some []
    | .tsql _, some e => do
      let e' ← lowerExpr d e
      pure [.assign x e']
    | .pg _, _ => none
  | .SSelect .. | .Insert .. | .Update .. | .Delete .. | .Merge .. | .Message .. => none

/-! ## Calls -/

/-- T-SQL `EXEC p a, @x OUTPUT`: positional arguments only; an `OUTPUT`
argument is a variable that receives the parameter's final value; without
`OUTPUT` nothing is copied back, even to an `OUTPUT` parameter. The callee
is renamed through the deployment. `EXEC @rc = p` is outside C1: the
status is the callee's *last* parameter in the core (above), so placing
`@rc` needs the callee's arity, which C1 does not take as input. -/
def lowerTsqlArg (d : Dialect) : Ast.Arg → Option Arg
  | .Arg (some _) _ _ => none -- named arguments: C1 binds by position
  | .Arg none e true => (target d e).map .ref
  | .Arg none e false => (lowerExpr d e).map .val

/-- PostgreSQL `CALL p(args)`: positional arguments only; an argument in
an `OUT` or `INOUT` position (the catalog says which) must be a variable,
and receives the parameter's final value; trailing parameters without an
argument take their defaults. -/
def lowerPgArg (d : Dialect) : Ast.Mode × Ast.Arg → Option Arg
  | (_, .Arg (some _) _ _) => none
  | (_, .Arg none _ true) => none -- `OUTPUT` is T-SQL syntax
  | (.In, .Arg none e false) => (lowerExpr d e).map .val
  | (_, .Arg none e false) => (target d e).map .ref

def lowerCall (d : Dialect) (n : List String) (args : List Ast.Arg) (ret : Option Ast.Expr) :
    Option Stmt :=
  match d with
  | .tsql dep => do
    if ret.isSome then none else
    let callee ← lookup dep.routines (dotted n)
    let args' ← args.mapM (lowerTsqlArg d)
    pure (.call callee args')
  | .pg Γ => do
    let callee := dotted (n.map pgName)
    let modes ← lookup Γ callee
    if args.length ≤ modes.length && ret.isNone then
      let args' ← (modes.zip args).mapM (lowerPgArg d)
      pure (.call callee args')
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
    | .NStmt s => lowerStmt d k s
    | .NBlock b => lowerInner d k b
    | .If c t e => do
      let c' ← lowerExpr d c
      let t' ← lowerNodes d k t
      let e' ← lowerNodes d k e
      pure [.ite c' (seqList t') (seqList e')]
    | .While c b => do
      let c' ← lowerExpr d c
      let b' ← lowerNodes d k b
      pure [.while c' (seqList b')]
    | .Call n args ret => (lowerCall d n args ret).map fun s => [s]
    | .Setting n _ => lowerSetting d n

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
    | .Block [] body => lowerNodes d k body
    | .Block (_ :: _) _ => none
end

/-- The names a list of statements declares, in order. -/
def declNames (ss : List Ast.Stmt) : List String :=
  ss.filterMap fun
    | .Declare x _ _ => some x
    | _ => none

mutual
  /-- The names T-SQL `DECLARE`s anywhere in a node, in order. -/
  def declsNode : Ast.Node → List String
    | .NStmt (.Declare x _ _) => [x]
    | .NBlock b => declsBlock b
    | .If _ t e => declsNodes t ++ declsNodes e
    | .While _ b => declsNodes b
    | .NStmt _ | .Call .. | .Setting .. => []

  def declsNodes : List Ast.Node → List String
    | [] => []
    | n :: ns => declsNode n ++ declsNodes ns

  def declsBlock : Ast.Block → List String
    | .Block ds body => declNames ds ++ declsNodes body
end

/-! ## Routines -/

def lowerMode : Ast.Mode → Core.Mode
  | .In => .inp
  | .Out => .out
  | .InOut => .inout

def lowerParam (d : Dialect) : Ast.Param → Option Core.Param
  | .Param name _ mode dflt =>
    let name := match d with
      | .tsql _ => name
      | .pg _ => pgName name
    -- A default that reads a variable is refused by both engines.
    match lowerOpt d dflt with
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

/-- A T-SQL routine. Outside the spec, besides the statements above:

* a name declared twice, or a local with a parameter's name (SQL Server
  refuses both, error 134), or a variable never declared (`wellScoped`);
* two different names equal up to case, or a non-ASCII name: whether
  `@a` and `@A` are one variable depends on the instance's collation, so
  the lowering, which keeps names as written, is faithful only when no
  two names of the routine could be confused. -/
def lowerT (dep : Deployment) : Ast.Routine → Option Core.Routine
  | .Routine kind _ params _ _ _ body => do
    let d := Dialect.tsql dep
    let ps ← params.mapM (lowerParam d)
    let stmts ← lowerInner d kind body
    let (ps, stmts) : List Core.Param × List Stmt := match kind with
      | .Procedure => (ps ++ [statusParam], Stmt.assign statusVar (.lit 0) :: stmts)
      | .Function => (ps, stmts)
    let r : Core.Routine := { params := ps, locals := declsBlock body, body := seqList stmts }
    if wellScoped r && caseSafe r.vars then some r else none

/-- A PL/pgSQL declaration `x t [:= e]`: the name, and the assignment the
block runs on entry, in declaration order, when there is a default. -/
def lowerDecl (d : Dialect) : Ast.Stmt → Option (Var × List Stmt)
  | .Declare x _ none => some (pgName x, [])
  | .Declare x _ (some e) => (lowerExpr d e).map fun e' => (pgName x, [.assign (pgName x) e'])
  | _ => none

/-- A PL/pgSQL routine: one block, its `DECLARE` section, its statements.
Every name is folded by `pgName`. Outside the spec, besides the
statements above: a non-ASCII name; a name declared twice, or a local
with a parameter's name (PostgreSQL lets the local shadow the parameter;
the core has one variable per name); a variable never declared
(`wellScoped`). -/
def lowerP (Γ : Catalog) : Ast.Routine → Option Core.Routine
  | .Routine kind _ params _ _ _ (.Block decls stmts) => do
    let d := Dialect.pg Γ
    let ps ← params.mapM (lowerParam d)
    let ds ← decls.mapM (lowerDecl d)
    let body ← lowerNodes d kind stmts
    let r : Core.Routine :=
      { params := ps, locals := ds.map (·.1), body := seqList ((ds.map (·.2)).flatten ++ body) }
    if wellScoped r && asciiNames r.vars then some r else none

/-- The routine kind and the privileges it runs with: the check asks both
sides to agree on them. -/
def header : Ast.Routine → Bool × Bool
  | .Routine kind _ _ _ sec _ _ =>
    (match kind with | .Procedure => true | .Function => false,
     match sec with | .Caller => true | .Owner => false)

end Sqldiff.Mirror
