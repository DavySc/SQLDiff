/-
Routines as abstract syntax: the C1 subset of the Rust types in
`crates/sqldiff-syntax/src/routine.rs` (`Routine`, `Param`, `Block`,
`Node`, `Arg`) and `crates/sqldiff-syntax/src/abstract_syntax.rs`
(`Stmt`, `Expr`, `Type`), with the same constructors and field order, so
that the codegen planned for C0 can replace this file mechanically. One
type serves both dialects, as in Rust; the dialect is a parameter of the
lowering (`Sqldiff.Mirror.Lower`).

Left out until a later phase needs them: the `Stmt` constructors for
queries and DML (C3) and `Message`; the `Expr` constructors other than
those below (C2/C3); the `Call` fields other than name and arguments
(`distinct`, `order`, `within_group`, `over`, `star`).
-/

namespace Sqldiff.Mirror.Ast

inductive RoutineKind where
  | procedure
  | function
deriving DecidableEq, Repr

/-- How a parameter passes its value. T-SQL has no `out`: its `OUTPUT`
parameters are `inOut`. -/
inductive Mode where
  | in_
  | out
  | inOut
deriving DecidableEq, Repr

/-- A type as written: name and arguments (`numeric(10,2)`). Types are C2;
C1 reads none. -/
structure Ty where
  name : String
  args : List String
deriving DecidableEq, Repr

inductive Expr where
  /-- Column or unresolved name: qualified parts as written. In PL/pgSQL a
  variable is a one-part name. -/
  | name (parts : List String)
  /-- T-SQL `@x` / `@@x`. -/
  | var (v : String)
  | num (s : String)
  /-- String literal value. -/
  | str (s : String)
  | null
  /-- A function call `name(args)` (`Call` in Rust, C1 fields only). -/
  | call (name : List String) (args : List Expr)
  | bin (op : String) (a b : Expr)
  | unary (op : String) (a : Expr)
deriving Repr

structure Param where
  /-- As written; T-SQL names keep their `@`. -/
  name : String
  ty : Ty
  mode : Mode
  default : Option Expr
deriving Repr

/-- Whose privileges the body runs with. -/
inductive Security where
  | caller
  | owner
deriving DecidableEq, Repr

inductive Stmt where
  /-- `SET v = e`, `v := e`, `SET @v = e`. -/
  | assign (target value : Expr)
  | return_ (e : Option Expr)
  | declare (name : String) (ty : Option Ty) (default : Option Expr)
deriving Repr

structure Arg where
  /-- `@p = value` (T-SQL), `p => value` (PostgreSQL). -/
  name : Option String
  value : Expr
  /-- T-SQL `OUTPUT`: the argument receives the parameter's final value. -/
  output : Bool
deriving Repr

mutual
  inductive Node where
    | stmt (s : Stmt)
    | block (b : Block)
    /-- `IF c THEN ... ELSE ...`; `ELSE IF` and `ELSIF` are an `if_` alone
    in `els`. -/
    | if_ (cond : Expr) (then_ els : List Node)
    | while (cond : Expr) (body : List Node)
    /-- `EXEC [@rc =] proc args` (T-SQL), `CALL proc(args)` (PostgreSQL). -/
    | call (name : List String) (args : List Arg) (ret : Option Expr)
    /-- A T-SQL session option: `SET NOCOUNT ON`. Upper case. -/
    | setting (name : String) (on : Bool)
  deriving Repr

  /-- A block: PL/pgSQL `[DECLARE ...] BEGIN ... END`, T-SQL `BEGIN ...
  END`. T-SQL declarations are statements of the body. -/
  inductive Block where
    | mk (decls : List Stmt) (body : List Node)
  deriving Repr
end

structure Routine where
  kind : RoutineKind
  name : List String
  params : List Param
  returns : Option Ty
  security : Security
  /-- PostgreSQL `STABLE` / `IMMUTABLE` (`VOLATILE` is the default, `none`). -/
  volatility : Option String
  body : Block
deriving Repr

end Sqldiff.Mirror.Ast
