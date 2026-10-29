/-
The core's semantics: a fuel-based definitional interpreter, executable so
the core can serve as the oracle for engine differential tests (decision 9
in docs/verified-conversion.md), the observations a caller makes (decision
1 in docs/verification-roadmap.md), and refinement.

Read top to bottom: values and errors, what a slice expression means, the
database (C3a), what an uninterpreted operator and the world may do, the
state, the data statements, one step of the interpreter per statement, a
routine's run, what a caller observes, and `Refines`.
-/
import Sqldiff.Core.Syntax

namespace Sqldiff.Core

variable {U W : Type}

/-! ## Values and errors (C2a, C3a) -/

/-- A value. `null` is SQL NULL, and also UNKNOWN, the third truth value:
a three-valued boolean is `bool true`, `bool false` or `null`. `int` is a
32-bit signed integer (T-SQL `int`, PostgreSQL `integer`); the slice's
operators keep it in range (`toInt32`). `str` is a character string (C2b):
a sequence of Unicode characters, the same value whichever T-SQL
(`varchar`, `nvarchar`) or PostgreSQL (`varchar`, `text`) type holds it;
the empty string is not NULL. Which strings each type can hold, and what
happens to one that does not fit, is the lowering's business
(`Sqldiff.Mirror`, "The C2b slice: strings"). `opaque` is any value
outside the slice (decimals, dates, ...): only uninterpreted operators
and the world make or read one, so theorems hold for every type `U` of
them. -/
inductive Val (U : Type) where
  | null
  | bool (b : Bool)
  | int (i : Int)
  | opaque (u : U)
  | str (s : String)
deriving DecidableEq, Repr

-- `ErrKind`, why a statement fails, is in `Sqldiff.Core.Syntax`: C4's
-- `raise` statement names one.

/-- What a caller can tell about an error across dialects (decision 1:
the error kind of the semantic profiles, `[error_kinds]`). Overflow and
division by zero are both `arithmetic`; both constraint violations are
`constraint`. That is also why the order in which the operands of one
expression are evaluated, which neither vendor specifies, does not matter
here: within one expression of the slice the only errors are arithmetic
ones, and whether some error happens does not depend on the order. For
a data statement, the slice asks that it cannot fail with both kinds
(`Sqldiff.Core.Stmt.dmlSafe`). `user` (C4) is an error the routine raises
itself (`THROW`, `RAISE EXCEPTION`). -/
inductive ErrClass where
  | arithmetic
  | constraint
  | other (code : String)
  | user
deriving DecidableEq, Repr

def ErrKind.cls : ErrKind → ErrClass
  | .overflow => .arithmetic
  | .divByZero => .arithmetic
  | .constraint => .constraint
  | .other c => .other c
  | .user => .user

/-- The result of evaluating an expression: a value (possibly NULL) or an
error. -/
inductive Res (U : Type) where
  | ok (v : Val U)
  | err (k : ErrKind)
deriving DecidableEq, Repr

/-! ## The slice's meaning

The vendor citations for each rule are on the lowering
(`Sqldiff.Mirror.typed`), which is the spec; this is the one function
both lowerings map to. -/

def intMin : Int := -2147483648
def intMax : Int := 2147483647

/-- A mathematical result as a 32-bit integer, or overflow. -/
def toInt32 (i : Int) : Res U :=
  if intMin ≤ i ∧ i ≤ intMax then .ok (.int i) else .err .overflow

/-- Integer arithmetic. Division truncates toward zero (`Int.tdiv`) and
the remainder takes the sign of the dividend (`Int.tmod`); a zero divisor
raises, and so does a result outside the range (`intMin / -1`). -/
def arithInt : AOp → Int → Int → Res U
  | .add, a, b => toInt32 (a + b)
  | .sub, a, b => toInt32 (a - b)
  | .mul, a, b => toInt32 (a * b)
  | .div, a, b => if b = 0 then .err .divByZero else toInt32 (a.tdiv b)
  | .mod, a, b => if b = 0 then .err .divByZero else toInt32 (a.tmod b)

def cmpInt : COp → Int → Int → Bool
  | .eq, a, b => a == b
  | .ne, a, b => a != b
  | .lt, a, b => a < b
  | .gt, a, b => a > b
  | .le, a, b => a ≤ b
  | .ge, a, b => a ≥ b

/-- An operand of the wrong kind. A well-typed lowering never produces
one; the world can (a call writes back anything), and both sides of a
check see the same. -/
def typeErr : Res U := .err (.other "type")

/-- Both operands, left to right; the first error wins. Both are
evaluated even when the first is NULL. -/
def bind2 (r s : Res U) (f : Val U → Val U → Res U) : Res U :=
  match r with
  | .err k => .err k
  | .ok v => match s with
    | .err k => .err k
    | .ok w => f v w

/-- A strict integer operator: NULL if either operand is NULL. -/
def intOp2 (f : Int → Int → Res U) : Val U → Val U → Res U
  | .null, _ => .ok .null
  | _, .null => .ok .null
  | .int x, .int y => f x y
  | _, _ => typeErr

/-- A strict string operator: NULL if either operand is NULL. -/
def strOp2 (f : String → String → Res U) : Val U → Val U → Res U
  | .null, _ => .ok .null
  | _, .null => .ok .null
  | .str x, .str y => f x y
  | _, _ => typeErr

/-- The string without its trailing spaces: U+0020 only, not tabs or other
blanks (T-SQL `RTRIM`, PostgreSQL `rtrim` with one argument). -/
def rtrimSp (s : String) : String :=
  String.ofList (s.toList.reverse.dropWhile (· == ' ')).reverse

/-- A truth value: `some none` is UNKNOWN. -/
def truth : Val U → Option (Option Bool)
  | .null => some none
  | .bool b => some (some b)
  | _ => none

/-- Three-valued `AND`: FALSE if either side is FALSE, else UNKNOWN if
either is UNKNOWN, else TRUE. -/
def and3 (v w : Val U) : Res U :=
  match truth v, truth w with
  | some (some false), some _ => .ok (.bool false)
  | some _, some (some false) => .ok (.bool false)
  | some (some true), some (some true) => .ok (.bool true)
  | some _, some _ => .ok .null
  | _, _ => typeErr

/-- Three-valued `OR`: TRUE if either side is TRUE, else UNKNOWN if
either is UNKNOWN, else FALSE. -/
def or3 (v w : Val U) : Res U :=
  match truth v, truth w with
  | some (some true), some _ => .ok (.bool true)
  | some _, some (some true) => .ok (.bool true)
  | some (some false), some (some false) => .ok (.bool false)
  | some _, some _ => .ok .null
  | _, _ => typeErr

def not3 : Val U → Res U
  | .null => .ok .null
  | .bool b => .ok (.bool !b)
  | _ => typeErr

/-- `IF`, `WHILE`, `CASE WHEN` and `WHERE` take their branch or keep their
row only on TRUE: FALSE and UNKNOWN (NULL) go to `ELSE`, leave the loop or
skip the row. -/
def isTrue : Val U → Bool
  | .bool true => true
  | _ => false

/-- Every variable name has a value; names never assigned are NULL. The
same shape serves for the columns of the row a data statement is at
(`rowStore`). -/
abbrev Store (U : Type) := Var → Val U

def Store.set (σ : Store U) (x : Var) (v : Val U) : Store U :=
  fun y => if y = x then v else σ y

/-- The store where every variable is NULL; as a row, no columns. -/
def nullStore : Store U := fun _ => .null

/-- The meaning of a slice expression with variables `σ` and, inside a
data statement, the current row's columns `ρ` (`nullStore` elsewhere). It
needs no parameter: this is the part of the core that does not depend on
operators meaning the same "by name". `coalesce` and `cond` evaluate only
the branch they take. `len` counts characters (Unicode scalar values) and
cannot overflow within the slice: the lowerings bound every string they
admit (4000 characters in T-SQL; a PostgreSQL value is at most 1 GB). -/
def SExpr.eval (σ ρ : Store U) : SExpr → Res U
  | .var x => .ok (σ x)
  | .col c => .ok (ρ c)
  | .int n => .ok (.int n)
  | .null => .ok .null
  | .arith o a b => bind2 (a.eval σ ρ) (b.eval σ ρ) (intOp2 (arithInt o))
  | .neg a => match a.eval σ ρ with
    | .err k => .err k
    | .ok .null => .ok .null
    | .ok (.int x) => toInt32 (-x)
    | .ok _ => typeErr
  | .cmp o a b => bind2 (a.eval σ ρ) (b.eval σ ρ) (intOp2 fun x y => .ok (.bool (cmpInt o x y)))
  | .and a b => bind2 (a.eval σ ρ) (b.eval σ ρ) and3
  | .or a b => bind2 (a.eval σ ρ) (b.eval σ ρ) or3
  | .not a => match a.eval σ ρ with
    | .err k => .err k
    | .ok v => not3 v
  | .isNull a isNot => match a.eval σ ρ with
    | .err k => .err k
    | .ok .null => .ok (.bool !isNot)
    | .ok _ => .ok (.bool isNot)
  | .coalesce a b => match a.eval σ ρ with
    | .err k => .err k
    | .ok .null => b.eval σ ρ
    | .ok v => .ok v
  | .cond c a b => match c.eval σ ρ with
    | .err k => .err k
    | .ok v => if isTrue v then a.eval σ ρ else b.eval σ ρ
  | .str s => .ok (.str s)
  | .concat a b => bind2 (a.eval σ ρ) (b.eval σ ρ) (strOp2 fun x y => .ok (.str (x ++ y)))
  | .rtrim a => match a.eval σ ρ with
    | .err k => .err k
    | .ok .null => .ok .null
    | .ok (.str s) => .ok (.str (rtrimSp s))
    | .ok _ => typeErr
  | .streq a b => bind2 (a.eval σ ρ) (b.eval σ ρ) (strOp2 fun x y => .ok (.bool (x == y)))
  | .len a => match a.eval σ ρ with
    | .err k => .err k
    | .ok .null => .ok .null
    | .ok (.str s) => .ok (.int s.length)
    | .ok _ => typeErr

/-! ## The database (C3a)

A table is a list of rows; a row lists one value per column of the
table's definition (`TableDef.cols`), in that order. The database names
each table's rows.

**Lists, observed as bags.** The engines keep a table as a bag: neither
promises an order of rows. The core keeps a list because its statements
then have one result and its equality is decidable, and every statement
treats the list as a bag would: `insert` appends, `update` maps each row
on its own, `delete` filters; which rows are written, and whether a
statement fails, never depends on the order. The one thing that would,
which error a statement reports when two rows fail with different kinds,
is kept out of the slice (`Stmt.dmlSafe`). So two runs from one start
give equal lists exactly when a permutation-blind reading gives equal
bags, and a confirmation's "equal lists" claims no more than "equal
bags" about the engines. -/

/-- A row: one value per column of its table, in the table's order. -/
abbrev Row (U : Type) := List (Val U)

/-- The database: each table's name and rows. A table it does not list
has no rows. -/
abbrev Db (U : Type) := List (String × List (Row U))

/-- The rows of table `t` (the first entry wins). -/
def Db.get (db : Db U) (t : String) : List (Row U) :=
  match db.find? (·.1 == t) with
  | some p => p.2
  | none => []

/-- Table `t` with rows `rs`, in place when the database lists it, else
added at the end. -/
def Db.set (db : Db U) (t : String) (rs : List (Row U)) : Db U :=
  if db.any (·.1 == t) then db.map fun p => if p.1 == t then (t, rs) else p
  else db ++ [(t, rs)]

/-! ## Parameters of the semantics

Every theorem quantifies over these, so it holds for every meaning of the
uninterpreted operators and every behaviour of the routines a call
reaches. -/

/-- What an uninterpreted operator computes from the results of its
arguments, errors included: it may be strict or lazy, propagate an
argument's error or not, and raise its own. -/
structure ExprSem (U : Type) where
  op : String → List (Res U) → Res U

/-- The routines a call reaches, as the world runs them: given the callee's
name, the argument values, the world state and the database, the new world
state and database, and one value per argument position (the parameter's
value when the callee ended; the caller keeps those in `ref` positions
only). The world is the environment of decision 1: chosen once, the same
for both sides. A callee may read and write the database. A call does not
fail: a callee's own errors are not modelled, which is why C4 keeps calls
out of handled bodies (`Sqldiff.Mirror`, "The C4 rules"). -/
structure WorldModel (U W : Type) where
  call : String → List (Val U) → W → Db U → W × Db U × List (Val U)

/-! ## State -/

/-- A call as the world sees it: the callee and the argument values. -/
structure CallEvent (U : Type) where
  proc : String
  args : List (Val U)
deriving DecidableEq, Repr

structure State (U W : Type) where
  store : Store U
  world : W
  db : Db U
  calls : List (CallEvent U)

/-- How a statement ends. `error` ends the routine unless a `tryCatch`
handles it (C4); a T-SQL routine is in the spec with errors outside a
handler only when it aborts on them (`Sqldiff.Mirror.abortsT`). The state of an `error` is the state before
the failing statement: a statement that fails has no effect (statement
atomicity, `Sqldiff.Core.exec_error_atomic`). `outOfFuel` keeps the state
reached, so the calls made before the fuel ran out stay observable. -/
inductive Outcome (U W : Type) where
  | normal (s : State U W)
  | returned (v : Option (Val U)) (s : State U W)
  | error (k : ErrKind) (s : State U W)
  | outOfFuel (s : State U W)

/-! ## Expressions and calls -/

mutual
  def Expr.eval (es : ExprSem U) (σ ρ : Store U) : Expr → Res U
    | .sc s => s.eval σ ρ
    | .op f args => es.op f (Expr.evalList es σ ρ args)

  def Expr.evalList (es : ExprSem U) (σ ρ : Store U) : List Expr → List (Res U)
    | [] => []
    | e :: es' => e.eval es σ ρ :: Expr.evalList es σ ρ es'
end

/-- An argument of a call; no columns are in scope. -/
def Arg.eval (es : ExprSem U) (σ : Store U) : Arg → Res U
  | .val e => e.eval es σ nullStore
  | .ref x => .ok (σ x)

/-- The arguments of a call, left to right; the first error wins and no
call is made. -/
def Arg.evalAll (es : ExprSem U) (σ : Store U) : List Arg → Except ErrKind (List (Val U))
  | [] => .ok []
  | a :: as => match a.eval es σ with
    | .err k => .error k
    | .ok v => match Arg.evalAll es σ as with
      | .error k => .error k
      | .ok vs => .ok (v :: vs)

/-- After a call: the value the world gives for position `i` is written
to the argument in position `i` when that argument is `ref x`. -/
def Store.setRefs (σ : Store U) : List Arg → List (Val U) → Store U
  | .ref x :: as, v :: vs => Store.setRefs (σ.set x v) as vs
  | .val _ :: as, _ :: vs => Store.setRefs σ as vs
  | _, _ => σ

/-! ## The data statements (C3a)

Each takes `ev`, how an expression evaluates given the current row's
columns (the variables are fixed for the whole statement: a statement
does not assign variables), and gives the table's new rows or the first
error. The vendor rules are on the lowering (`Sqldiff.Mirror`, "The C3a
slice: data statements"). Rows are processed in list order and the first
error wins; within the slice, which row fails first is not observable
(`ErrClass`, `Stmt.dmlSafe`). -/

/-- The values of `es`, left to right; the first error wins. -/
def evalVals (ev : Store U → Expr → Res U) (ρ : Store U) :
    List Expr → Except ErrKind (List (Val U))
  | [] => .ok []
  | e :: es => match ev ρ e with
    | .err k => .error k
    | .ok v => match evalVals ev ρ es with
      | .error k => .error k
      | .ok vs => .ok (v :: vs)

/-- A `WHERE` condition: the row is kept only when it is TRUE; no
condition keeps every row. -/
def keeps (ev : Store U → Expr → Res U) (ρ : Store U) : Option Expr → Except ErrKind Bool
  | none => .ok true
  | some p => match ev ρ p with
    | .err k => .error k
    | .ok v => .ok (isTrue v)

/-- A row's columns as a store: column `c` is the row's value in `c`'s
position (the first column of that name), NULL for a name that is not a
column. -/
def rowStore : List ColDef → Row U → Store U
  | d :: ds, v :: vs => fun c => if c = d.name then v else rowStore ds vs c
  | _, _ => nullStore

/-- The value given to column `c` by a list of columns and their values
(the first entry wins). -/
def pick (cs : List Col) (vs : List (Val U)) (c : Col) : Option (Val U) :=
  match cs, vs with
  | c' :: cs, v :: vs => if c' = c then some v else pick cs vs c
  | _, _ => none

def isNullV : Val U → Bool
  | .null => true
  | _ => false

/-- No required column (`NOT NULL` or key) of the row is NULL. -/
def rowOk (t : List ColDef) (r : Row U) : Bool :=
  (t.zip r).all fun p => !(p.1.required && isNullV p.2)

def keyVal : Val U → Option Int
  | .int i => some i
  | _ => none

/-- The row's primary key, `none` when a key value is not an integer. -/
def keyOf (t : List ColDef) (r : Row U) : Option (List Int) :=
  ((t.zip r).filter (·.1.key)).mapM fun p => keyVal p.2

/-- `INSERT INTO t (cs) VALUES ...`: each row's values, left to right, with
no columns in scope; the new row has them in the listed columns and NULL
in the others; a NULL in a required column, or a key that a row already
in the table (or inserted before it by this statement) has, is a
constraint violation. `acc` is the table so far; new rows go at the end. -/
def insertRows (ev : Store U → Expr → Res U) (t : TableDef) (cs : List Col) :
    List (List Expr) → List (Row U) → Except ErrKind (List (Row U))
  | [], acc => .ok acc
  | es :: ess, acc => match evalVals ev nullStore es with
    | .error k => .error k
    | .ok vs =>
      let r := t.cols.map fun d => (pick cs vs d.name).getD .null
      if !rowOk t.cols r then .error .constraint
      else if !t.hasKey then insertRows ev t cs ess (acc ++ [r])
      else match keyOf t.cols r with
        | none => .error (.other "type")
        | some k =>
          if acc.any (fun r' => keyOf t.cols r' == some k) then .error .constraint
          else insertRows ev t cs ess (acc ++ [r])

/-- One row of `UPDATE t SET c1 = e1, ...`: every `ei` on the row's old
values, then each `ci` takes its value; a NULL in a required column is a
constraint violation. Key columns are never set (`Sqldiff.Mirror`), so no
key check is needed. -/
def updRow (ev : Store U → Expr → Res U) (t : TableDef) (sets : List (Col × Expr)) (r : Row U) :
    Except ErrKind (Row U) :=
  match evalVals ev (rowStore t.cols r) (sets.map (·.2)) with
  | .error k => .error k
  | .ok vs =>
    let r' := (t.cols.zip r).map fun p => (pick (sets.map (·.1)) vs p.1.name).getD p.2
    if rowOk t.cols r' then .ok r' else .error .constraint

/-- `UPDATE t SET ... [WHERE c]`, row by row. -/
def updateRows (ev : Store U → Expr → Res U) (t : TableDef) (sets : List (Col × Expr))
    (c : Option Expr) : List (Row U) → Except ErrKind (List (Row U))
  | [] => .ok []
  | r :: rs => match keeps ev (rowStore t.cols r) c with
    | .error k => .error k
    | .ok b =>
      match (if b then updRow ev t sets r else .ok r) with
      | .error k => .error k
      | .ok r' => match updateRows ev t sets c rs with
        | .error k => .error k
        | .ok rs' => .ok (r' :: rs')

/-- `DELETE FROM t [WHERE c]`, row by row. -/
def deleteRows (ev : Store U → Expr → Res U) (t : TableDef) (c : Option Expr) :
    List (Row U) → Except ErrKind (List (Row U))
  | [] => .ok []
  | r :: rs => match keeps ev (rowStore t.cols r) c with
    | .error k => .error k
    | .ok b => match deleteRows ev t c rs with
      | .error k => .error k
      | .ok rs' => .ok (if b then rs' else r :: rs')

/-! ## The interpreter

`exec es wm n s st` runs statement `s` from state `st` with fuel `n`. Fuel
bounds the depth of the run, not its length: a statement takes one unit
and runs each of its parts with what is left, and each iteration of a
`while` runs one level deeper than the one before. A run that terminates
does so for every large enough `n`; a run that does not terminate is out
of fuel for every `n`. Fuel is never an observation of its own: `Refines`
quantifies over it. A `return` or an error ends the routine: it
propagates out of every enclosing `seq` and `while` without running
anything else, until a `tryCatch` body (C4), whose handler then runs.
A statement whose expression fails changes nothing: the error keeps the
state from before it; so does a data statement that fails on any row
(statement atomicity). -/

def exec (es : ExprSem U) (wm : WorldModel U W) : Nat → Stmt → State U W → Outcome U W
  | 0, _, st => .outOfFuel st
  | _ + 1, .skip, st => .normal st
  | _ + 1, .assign x e, st => match e.eval es st.store nullStore with
    | .err k => .error k st
    | .ok v => .normal { st with store := st.store.set x v }
  | n + 1, .seq s t, st =>
    match exec es wm n s st with
    | .normal st' => exec es wm n t st'
    | o => o
  | n + 1, .ite c s t, st => match c.eval es st.store nullStore with
    | .err k => .error k st
    | .ok v => if isTrue v then exec es wm n s st else exec es wm n t st
  | n + 1, .while c b, st => match c.eval es st.store nullStore with
    | .err k => .error k st
    | .ok v =>
      if isTrue v then
        match exec es wm n b st with
        | .normal st' => exec es wm n (.while c b) st'
        | o => o
      else .normal st
  | _ + 1, .ret none, st => .returned none st
  | _ + 1, .ret (some e), st => match e.eval es st.store nullStore with
    | .err k => .error k st
    | .ok v => .returned (some v) st
  | _ + 1, .call p args, st => match Arg.evalAll es st.store args with
    | .error k => .error k st
    | .ok vs =>
      let (w', db', rs) := wm.call p vs st.world st.db
      .normal { store := st.store.setRefs args rs, world := w', db := db',
                calls := st.calls ++ [⟨p, vs⟩] }
  | _ + 1, .insert t cs rows, st =>
    match insertRows (fun ρ e => e.eval es st.store ρ) t cs rows (st.db.get t.name) with
    | .error k => .error k st
    | .ok rs => .normal { st with db := st.db.set t.name rs }
  | _ + 1, .update t sets c, st =>
    match updateRows (fun ρ e => e.eval es st.store ρ) t sets c (st.db.get t.name) with
    | .error k => .error k st
    | .ok rs => .normal { st with db := st.db.set t.name rs }
  | _ + 1, .delete t c, st =>
    match deleteRows (fun ρ e => e.eval es st.store ρ) t c (st.db.get t.name) with
    | .error k => .error k st
    | .ok rs => .normal { st with db := st.db.set t.name rs }
  | n + 1, .tryCatch u b h, st =>
    match exec es wm n b st with
    | .error k st' => exec es wm n (h.bindErr k) (if u then { st' with db := st.db } else st')
    | o => o
  | _ + 1, .raise k, st => .error k st
  | _ + 1, .reraise, st => .error (.other "reraise") st
  | _ + 1, .commit, st => .normal st

/-! ## A routine's run and what a caller observes (decision 1) -/

/-- The value a parameter starts with: NULL for `out`; otherwise the
argument, or when the caller passed none, the default evaluated with every
variable NULL (T-SQL defaults are constants, PostgreSQL defaults cannot
read parameters), or NULL when there is no default (where the engine would
refuse the call: an error, outside the spec). A default can fail. -/
def paramValue (es : ExprSem U) (p : Param) (arg : Option (Val U)) : Res U :=
  match p.mode, arg with
  | .out, _ => .ok .null
  | _, some v => .ok v
  | _, none => match p.default with
    | some e => e.eval es nullStore nullStore
    | none => .ok .null

/-- The store on entry: arguments bound to parameters by position; every
other variable NULL. Arguments beyond the last parameter are ignored. The
first default that fails fails the call. -/
def bindParams (es : ExprSem U) : List Param → List (Val U) → Store U → Except ErrKind (Store U)
  | [], _, σ => .ok σ
  | p :: ps, [], σ => match paramValue es p none with
    | .err k => .error k
    | .ok v => bindParams es ps [] (σ.set p.name v)
  | p :: ps, v :: vs, σ => match paramValue es p (some v) with
    | .err k => .error k
    | .ok v' => bindParams es ps vs (σ.set p.name v')

def initStore (es : ExprSem U) (ps : List Param) (args : List (Val U)) :
    Except ErrKind (Store U) :=
  bindParams es ps args nullStore

/-- The values of the `out` and `inout` parameters, in parameter order:
what the caller reads back. By position, so no variable name is observed. -/
def outVals (σ : Store U) : List Param → List (Val U)
  | [] => []
  | p :: ps => if p.mode = .inp then outVals σ ps else σ p.name :: outVals σ ps

/-- An observation.

* `done` is a finished run: out parameters, the returned value (`none`
  for `RETURN` without a value and for falling off the end), the world and
  the database afterwards (decision 1: tables as bags, see "The database"
  above), and every call made.
* `failed` is a run that ended with an error: its kind as decision 1
  compares it across dialects, and the calls made before it. Neither the
  world nor the database after a failed call is observed: what a caller
  sees of them depends on its transaction and on each dialect's rollback
  (T-SQL under `XACT_ABORT ON` without an open transaction keeps the
  statements before the failing one, PostgreSQL rolls the whole call back).
  A confirmation of a routine that can fail after a write says so
  (`Sqldiff.Mirror.residual`, `ASM-STATE-AFTER-ERROR`). C4 keeps this
  observation: a *handled* error does not end the call, and the database
  the handler starts from, and so the one a finished call shows, is
  modelled exactly (`Stmt.tryCatch`, `undo`); only the state after an
  error that leaves the routine stays unobserved.
  Out parameters and the returned value do not reach a caller whose call
  failed.
* `pending` is a run cut off by fuel: the world and the calls so far,
  which the world has already seen; the database of a run that has not
  ended is not observed (one session, decision 1). -/
inductive Obs (U W : Type) where
  | done (outs : List (Val U)) (ret : Option (Val U)) (world : W) (db : Db U)
      (calls : List (CallEvent U))
  | failed (k : ErrClass) (calls : List (CallEvent U))
  | pending (world : W) (calls : List (CallEvent U))
deriving DecidableEq, Repr

def observe (r : Routine) : Outcome U W → Obs U W
  | .normal st => .done (outVals st.store r.params) none st.world st.db st.calls
  | .returned v st => .done (outVals st.store r.params) v st.world st.db st.calls
  | .error k st => .failed k.cls st.calls
  | .outOfFuel st => .pending st.world st.calls

/-- The observation of running `r` on `args` in world `w` and database
`db` with fuel `n`. -/
def Routine.obs (es : ExprSem U) (wm : WorldModel U W) (r : Routine) (args : List (Val U)) (w : W)
    (db : Db U) (n : Nat) : Obs U W :=
  match initStore es r.params args with
  | .error k => .failed k.cls []
  | .ok σ => observe r (exec es wm n r.body ⟨σ, w, db, []⟩)

/-- `r` does not terminate: every amount of fuel runs out. -/
def Diverges (es : ExprSem U) (wm : WorldModel U W) (r : Routine) (args : List (Val U)) (w : W)
    (db : Db U) : Prop :=
  ∀ n, ∃ w' cs, r.obs es wm args w db n = .pending w' cs

/-! ## Refinement (decision 1)

`a` is the reference (the source), `b` the candidate (the conversion).
The environment, here the arguments, the world and the database, is
chosen once and shared: the value correspondence is the identity, since
both sides are lowered into the same core values; the state
correspondence is the identity too, since both sides name the target
catalog's tables and columns (the lowering maps T-SQL names to them,
`Sqldiff.Mirror.Deployment.tables`); and observations are positional, so
the variable correspondence between the sides is not part of the
statement (it is how `check` proves it, `Sqldiff.Core.Rename`).

Two clauses, for every environment:

1. Whatever `b` shows with some fuel, `a` shows with some fuel. For a
   finished run that is the result or the error; for a cut-off run it is
   the world and the calls so far, so every finite prefix of `b`'s
   interaction with the world is one `a` has, including when `b` runs
   forever.
2. If `b` runs forever, so does `a`: non-termination is an observation of
   its own, so a looping `b` does not refine a terminating `a`.

The core is deterministic, and once a run finishes more fuel gives the
same observation (`Sqldiff.Core.obs_mono`), so a finished result does not
depend on the fuel used to find it. The run is never empty, as decision 1
requires: each fuel gives an observation, and either some fuel finishes
or the run diverges. -/
def Refines (es : ExprSem U) (wm : WorldModel U W) (a b : Routine) : Prop :=
  ∀ (args : List (Val U)) (w : W) (db : Db U),
    (∀ n, ∃ m, b.obs es wm args w db n = a.obs es wm args w db m) ∧
    (Diverges es wm b args w db → Diverges es wm a args w db)

end Sqldiff.Core
