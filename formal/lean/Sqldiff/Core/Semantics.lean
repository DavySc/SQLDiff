/-
The core's semantics: a fuel-based definitional interpreter, executable so
the core can serve as the oracle for engine differential tests (decision 9
in docs/verified-conversion.md), the observations a caller makes (decision
1 in docs/verification-roadmap.md), and refinement.

Read top to bottom: values and errors, what a slice expression means, what
an uninterpreted operator and the world may do, the state, one step of the
interpreter per statement, a routine's run, what a caller observes, and
`Refines`.
-/
import Sqldiff.Core.Syntax

namespace Sqldiff.Core

variable {U W : Type}

/-! ## Values and errors (C2a) -/

/-- A value. `null` is SQL NULL, and also UNKNOWN, the third truth value:
a three-valued boolean is `bool true`, `bool false` or `null`. `int` is a
32-bit signed integer (T-SQL `int`, PostgreSQL `integer`); the slice's
operators keep it in range (`toInt32`). `opaque` is any value outside the
slice (strings, decimals, dates, ...): only uninterpreted operators and
the world make or read one, so theorems hold for every type `U` of them. -/
inductive Val (U : Type) where
  | null
  | bool (b : Bool)
  | int (i : Int)
  | opaque (u : U)
deriving DecidableEq, Repr

/-- Why an expression fails. `overflow` is T-SQL error 8115 and
PostgreSQL SQLSTATE 22003 ("integer out of range"); `divByZero` is T-SQL
8134 and SQLSTATE 22012. `other` is raised by an uninterpreted operator. -/
inductive ErrKind where
  | overflow
  | divByZero
  | other (code : String)
deriving DecidableEq, Repr

/-- What a caller can tell about an error across dialects (decision 1:
the error kind of the semantic profiles, `[error_kinds]`). Overflow and
division by zero are both `arithmetic`. That is also why the order in
which the operands of one expression are evaluated, which neither vendor
specifies, does not matter here: within the slice the only errors are
arithmetic ones, and whether some error happens does not depend on the
order. -/
inductive ErrClass where
  | arithmetic
  | other (code : String)
deriving DecidableEq, Repr

def ErrKind.cls : ErrKind → ErrClass
  | .overflow => .arithmetic
  | .divByZero => .arithmetic
  | .other c => .other c

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

/-- `IF`, `WHILE` and `CASE WHEN` take their branch only on TRUE: FALSE
and UNKNOWN (NULL) go to `ELSE` or leave the loop. -/
def isTrue : Val U → Bool
  | .bool true => true
  | _ => false

/-- Every variable name has a value; names never assigned are NULL. -/
abbrev Store (U : Type) := Var → Val U

def Store.set (σ : Store U) (x : Var) (v : Val U) : Store U :=
  fun y => if y = x then v else σ y

/-- The meaning of a slice expression in a store. It needs no parameter:
this is the part of the core that does not depend on operators meaning the
same "by name". `coalesce` and `cond` evaluate only the branch they
take. -/
def SExpr.eval (σ : Store U) : SExpr → Res U
  | .var x => .ok (σ x)
  | .int n => .ok (.int n)
  | .null => .ok .null
  | .arith o a b => bind2 (a.eval σ) (b.eval σ) (intOp2 (arithInt o))
  | .neg a => match a.eval σ with
    | .err k => .err k
    | .ok .null => .ok .null
    | .ok (.int x) => toInt32 (-x)
    | .ok _ => typeErr
  | .cmp o a b => bind2 (a.eval σ) (b.eval σ) (intOp2 fun x y => .ok (.bool (cmpInt o x y)))
  | .and a b => bind2 (a.eval σ) (b.eval σ) and3
  | .or a b => bind2 (a.eval σ) (b.eval σ) or3
  | .not a => match a.eval σ with
    | .err k => .err k
    | .ok v => not3 v
  | .isNull a isNot => match a.eval σ with
    | .err k => .err k
    | .ok .null => .ok (.bool !isNot)
    | .ok _ => .ok (.bool isNot)
  | .coalesce a b => match a.eval σ with
    | .err k => .err k
    | .ok .null => b.eval σ
    | .ok v => .ok v
  | .cond c a b => match c.eval σ with
    | .err k => .err k
    | .ok v => if isTrue v then a.eval σ else b.eval σ

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
name, the argument values and the world state, the new world state and
one value per argument position (the parameter's value when the callee
ended; the caller keeps those in `ref` positions only). The world is the
environment of decision 1: chosen once, the same for both sides. A call
does not fail (errors from callees are C4). -/
structure WorldModel (U W : Type) where
  call : String → List (Val U) → W → W × List (Val U)

/-! ## State -/

/-- A call as the world sees it: the callee and the argument values. -/
structure CallEvent (U : Type) where
  proc : String
  args : List (Val U)
deriving DecidableEq, Repr

structure State (U W : Type) where
  store : Store U
  world : W
  calls : List (CallEvent U)

/-- How a statement ends. `error` ends the routine: there are no handlers
yet (T-SQL `TRY`/`CATCH` and PL/pgSQL `EXCEPTION` are C4), and a T-SQL
routine is in the spec with errors only when it aborts on them
(`Sqldiff.Mirror.abortsT`). `outOfFuel` keeps the state reached, so the
calls made before the fuel ran out stay observable. -/
inductive Outcome (U W : Type) where
  | normal (s : State U W)
  | returned (v : Option (Val U)) (s : State U W)
  | error (k : ErrKind) (s : State U W)
  | outOfFuel (s : State U W)

/-! ## Expressions and calls -/

mutual
  def Expr.eval (es : ExprSem U) (σ : Store U) : Expr → Res U
    | .sc s => s.eval σ
    | .op f args => es.op f (Expr.evalList es σ args)

  def Expr.evalList (es : ExprSem U) (σ : Store U) : List Expr → List (Res U)
    | [] => []
    | e :: es' => e.eval es σ :: Expr.evalList es σ es'
end

def Arg.eval (es : ExprSem U) (σ : Store U) : Arg → Res U
  | .val e => e.eval es σ
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

/-! ## The interpreter

`exec es wm n s st` runs statement `s` from state `st` with fuel `n`. Fuel
bounds the depth of the run, not its length: a statement takes one unit
and runs each of its parts with what is left, and each iteration of a
`while` runs one level deeper than the one before. A run that terminates
does so for every large enough `n`; a run that does not terminate is out
of fuel for every `n`. Fuel is never an observation of its own: `Refines`
quantifies over it. A `return` or an error ends the routine: it
propagates out of every enclosing `seq` and `while` without running
anything else. A statement whose expression fails changes nothing: the
error keeps the state from before it. -/

def exec (es : ExprSem U) (wm : WorldModel U W) : Nat → Stmt → State U W → Outcome U W
  | 0, _, st => .outOfFuel st
  | _ + 1, .skip, st => .normal st
  | _ + 1, .assign x e, st => match e.eval es st.store with
    | .err k => .error k st
    | .ok v => .normal { st with store := st.store.set x v }
  | n + 1, .seq s t, st =>
    match exec es wm n s st with
    | .normal st' => exec es wm n t st'
    | o => o
  | n + 1, .ite c s t, st => match c.eval es st.store with
    | .err k => .error k st
    | .ok v => if isTrue v then exec es wm n s st else exec es wm n t st
  | n + 1, .while c b, st => match c.eval es st.store with
    | .err k => .error k st
    | .ok v =>
      if isTrue v then
        match exec es wm n b st with
        | .normal st' => exec es wm n (.while c b) st'
        | o => o
      else .normal st
  | _ + 1, .ret none, st => .returned none st
  | _ + 1, .ret (some e), st => match e.eval es st.store with
    | .err k => .error k st
    | .ok v => .returned (some v) st
  | _ + 1, .call p args, st => match Arg.evalAll es st.store args with
    | .error k => .error k st
    | .ok vs =>
      let (w', rs) := wm.call p vs st.world
      .normal { store := st.store.setRefs args rs, world := w', calls := st.calls ++ [⟨p, vs⟩] }

/-! ## A routine's run and what a caller observes (decision 1) -/

/-- The store where every variable is NULL. -/
def nullStore : Store U := fun _ => .null

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
    | some e => e.eval es nullStore
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

/-- An observation. `done` is a finished run: out parameters, the returned
value (`none` for `RETURN` without a value and for falling off the end),
the world afterwards and every call made. `failed` is a run that ended
with an error: its kind as decision 1 compares it across dialects, the
world and the calls made before it; out parameters and the returned value
do not reach a caller whose call failed. `pending` is a run cut off by
fuel: the world and the calls so far, which the world has already seen. -/
inductive Obs (U W : Type) where
  | done (outs : List (Val U)) (ret : Option (Val U)) (world : W) (calls : List (CallEvent U))
  | failed (k : ErrClass) (world : W) (calls : List (CallEvent U))
  | pending (world : W) (calls : List (CallEvent U))
deriving DecidableEq, Repr

def observe (r : Routine) : Outcome U W → Obs U W
  | .normal st => .done (outVals st.store r.params) none st.world st.calls
  | .returned v st => .done (outVals st.store r.params) v st.world st.calls
  | .error k st => .failed k.cls st.world st.calls
  | .outOfFuel st => .pending st.world st.calls

/-- The observation of running `r` on `args` in world `w` with fuel `n`. -/
def Routine.obs (es : ExprSem U) (wm : WorldModel U W) (r : Routine) (args : List (Val U)) (w : W)
    (n : Nat) : Obs U W :=
  match initStore es r.params args with
  | .error k => .failed k.cls w []
  | .ok σ => observe r (exec es wm n r.body ⟨σ, w, []⟩)

/-- `r` does not terminate: every amount of fuel runs out. -/
def Diverges (es : ExprSem U) (wm : WorldModel U W) (r : Routine) (args : List (Val U)) (w : W) :
    Prop :=
  ∀ n, ∃ w' cs, r.obs es wm args w n = .pending w' cs

/-! ## Refinement (decision 1)

`a` is the reference (the source), `b` the candidate (the conversion).
The environment, here the arguments and the world, is chosen once and
shared: the value correspondence is the identity, since both sides are
lowered into the same core values, and observations are positional, so
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
  ∀ (args : List (Val U)) (w : W),
    (∀ n, ∃ m, b.obs es wm args w n = a.obs es wm args w m) ∧
    (Diverges es wm b args w → Diverges es wm a args w)

end Sqldiff.Core
