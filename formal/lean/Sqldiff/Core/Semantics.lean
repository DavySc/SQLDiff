/-
The core's semantics: a fuel-based definitional interpreter, executable so
the core can serve as the oracle for engine differential tests (decision 9
in docs/verified-conversion.md), the observations a caller makes (decision
1 in docs/verification-roadmap.md), and refinement.

Read top to bottom: what an expression means, what the world does on a
call, the state, one step of the interpreter per statement, a routine's
run, what a caller observes, and `Refines`.
-/
import Sqldiff.Core.Syntax

namespace Sqldiff.Core

variable {V W : Type}

/-! ## Parameters of the semantics

Every theorem quantifies over these, so it holds for every meaning of the
operators and every behaviour of the routines a call reaches. -/

/-- What values are and what C1 expressions mean. `null` is the value of
`NULL` and of a variable never assigned (T-SQL `DECLARE` without `=`, a
PL/pgSQL declaration without `:=`, a PostgreSQL `OUT` parameter on entry).
`op f` is what operator `f` computes from its arguments' values.
`truthy` decides `IF` and `WHILE`; both dialects take the `ELSE` branch
and leave the loop when the condition is not TRUE, so NULL is not truthy
in any instance C2 will give. -/
structure ExprSem (V : Type) where
  null : V
  lit : Int → V
  op : String → List V → V
  truthy : V → Bool

/-- The routines a call reaches, as the world runs them: given the callee's
name, the argument values and the world state, the new world state and
one value per argument position (the parameter's value when the callee
ended; the caller keeps those in `ref` positions only). The world is the
environment of decision 1: chosen once, the same for both sides. -/
structure WorldModel (V W : Type) where
  call : String → List V → W → W × List V

/-! ## State -/

/-- Every variable name has a value; names never assigned are `null`. -/
abbrev Store (V : Type) := Var → V

def Store.set (σ : Store V) (x : Var) (v : V) : Store V :=
  fun y => if y = x then v else σ y

/-- A call as the world sees it: the callee and the argument values. -/
structure CallEvent (V : Type) where
  proc : String
  args : List V
deriving DecidableEq, Repr

structure State (V W : Type) where
  store : Store V
  world : W
  calls : List (CallEvent V)

/-- How a statement ends. `outOfFuel` keeps the state reached, so the
calls made before the fuel ran out stay observable. -/
inductive Outcome (V W : Type) where
  | normal (s : State V W)
  | returned (v : Option V) (s : State V W)
  | outOfFuel (s : State V W)

/-! ## Expressions and calls -/

mutual
  def Expr.eval (es : ExprSem V) (σ : Store V) : Expr → V
    | .var x => σ x
    | .lit n => es.lit n
    | .null => es.null
    | .op f args => es.op f (Expr.evalList es σ args)

  def Expr.evalList (es : ExprSem V) (σ : Store V) : List Expr → List V
    | [] => []
    | e :: es' => e.eval es σ :: Expr.evalList es σ es'
end

def Arg.eval (es : ExprSem V) (σ : Store V) : Arg → V
  | .val e => e.eval es σ
  | .ref x => σ x

/-- After a call: the value the world gives for position `i` is written
to the argument in position `i` when that argument is `ref x`. -/
def Store.setRefs (σ : Store V) : List Arg → List V → Store V
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
quantifies over it. A `return` ends the routine: it propagates out of
every enclosing `seq` and `while` without running anything else, so
`RETURN` inside `WHILE` needs no special case. -/

def exec (es : ExprSem V) (wm : WorldModel V W) : Nat → Stmt → State V W → Outcome V W
  | 0, _, st => .outOfFuel st
  | _ + 1, .skip, st => .normal st
  | _ + 1, .assign x e, st => .normal { st with store := st.store.set x (e.eval es st.store) }
  | n + 1, .seq s t, st =>
    match exec es wm n s st with
    | .normal st' => exec es wm n t st'
    | o => o
  | n + 1, .ite c s t, st =>
    if es.truthy (c.eval es st.store) then exec es wm n s st else exec es wm n t st
  | n + 1, .while c b, st =>
    if es.truthy (c.eval es st.store) then
      match exec es wm n b st with
      | .normal st' => exec es wm n (.while c b) st'
      | o => o
    else .normal st
  | _ + 1, .ret e, st => .returned (e.map (Expr.eval es st.store)) st
  | _ + 1, .call p args, st =>
    let vs := args.map (Arg.eval es st.store)
    let (w', rs) := wm.call p vs st.world
    .normal { store := st.store.setRefs args rs, world := w', calls := st.calls ++ [⟨p, vs⟩] }

/-! ## A routine's run and what a caller observes (decision 1) -/

/-- The store where every variable is NULL. -/
def nullStore (es : ExprSem V) : Store V := fun _ => es.null

/-- The value a parameter starts with: NULL for `out`; otherwise the
argument, or when the caller passed none, the default evaluated with every
variable NULL (T-SQL defaults are constants, PostgreSQL defaults cannot
read parameters), or NULL when there is no default (where the engine would
refuse the call: an error, outside C1). -/
def paramValue (es : ExprSem V) (p : Param) (arg : Option V) : V :=
  match p.mode, arg with
  | .out, _ => es.null
  | _, some v => v
  | _, none => match p.default with
    | some e => e.eval es (nullStore es)
    | none => es.null

/-- The store on entry: arguments bound to parameters by position; every
other variable NULL. Arguments beyond the last parameter are ignored. -/
def bindParams (es : ExprSem V) : List Param → List V → Store V → Store V
  | [], _, σ => σ
  | p :: ps, [], σ => bindParams es ps [] (σ.set p.name (paramValue es p none))
  | p :: ps, v :: vs, σ => bindParams es ps vs (σ.set p.name (paramValue es p (some v)))

def initStore (es : ExprSem V) (ps : List Param) (args : List V) : Store V :=
  bindParams es ps args (nullStore es)

/-- The values of the `out` and `inout` parameters, in parameter order:
what the caller reads back. By position, so no variable name is observed. -/
def outVals (σ : Store V) : List Param → List V
  | [] => []
  | p :: ps => if p.mode = .inp then outVals σ ps else σ p.name :: outVals σ ps

/-- An observation. `done` is a finished run: out parameters, the returned
value (`none` for `RETURN` without a value and for falling off the end),
the world afterwards and every call made. `pending` is a run cut off by
fuel: the world and the calls so far, which the world has already seen. -/
inductive Obs (V W : Type) where
  | done (outs : List V) (ret : Option V) (world : W) (calls : List (CallEvent V))
  | pending (world : W) (calls : List (CallEvent V))
deriving DecidableEq, Repr

def observe (r : Routine) : Outcome V W → Obs V W
  | .normal st => .done (outVals st.store r.params) none st.world st.calls
  | .returned v st => .done (outVals st.store r.params) v st.world st.calls
  | .outOfFuel st => .pending st.world st.calls

/-- The observation of running `r` on `args` in world `w` with fuel `n`. -/
def Routine.obs (es : ExprSem V) (wm : WorldModel V W) (r : Routine) (args : List V) (w : W)
    (n : Nat) : Obs V W :=
  observe r (exec es wm n r.body ⟨initStore es r.params args, w, []⟩)

/-- `r` does not terminate: every amount of fuel runs out. -/
def Diverges (es : ExprSem V) (wm : WorldModel V W) (r : Routine) (args : List V) (w : W) :
    Prop :=
  ∀ n, ∃ w' cs, r.obs es wm args w n = .pending w' cs

/-! ## Refinement (decision 1)

`a` is the reference (the source), `b` the candidate (the conversion).
The environment, here the arguments and the world, is chosen once and
shared: in C1 the value correspondence is the identity, since both sides
are lowered into the same core values, and observations are positional,
so the variable correspondence between the sides is not part of the
statement (it is how `check` proves it, `Sqldiff.Core.Rename`).

Two clauses, for every environment:

1. Whatever `b` shows with some fuel, `a` shows with some fuel. For a
   finished run that is the result; for a cut-off run it is the world and
   the calls so far, so every finite prefix of `b`'s interaction with the
   world is one `a` has, including when `b` runs forever.
2. If `b` runs forever, so does `a`: non-termination is an observation of
   its own, so a looping `b` does not refine a terminating `a`.

The core is deterministic, and once a run finishes more fuel gives the
same observation (`Sqldiff.Core.obs_mono`), so a finished result does not
depend on the fuel used to find it. The run is never empty, as decision 1
requires: each fuel gives an observation, and either some fuel finishes
or the run diverges. -/
def Refines (es : ExprSem V) (wm : WorldModel V W) (a b : Routine) : Prop :=
  ∀ (args : List V) (w : W),
    (∀ n, ∃ m, b.obs es wm args w n = a.obs es wm args w m) ∧
    (Diverges es wm b args w → Diverges es wm a args w)

end Sqldiff.Core
