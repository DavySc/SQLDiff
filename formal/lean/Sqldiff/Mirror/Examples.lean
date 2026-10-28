/-
Checked examples for the C1 mirror check, on the generated abstract syntax
(`Sqldiff.Ast`): a T-SQL procedure and its PL/pgSQL mirror, confirmed;
what the converter emits today, and variants that must not be confirmed;
and runs of the core interpreter as an executable oracle. Every `#guard`
runs on every `lake build`.
-/
import Sqldiff.Mirror.Check

namespace Sqldiff.Mirror.Examples

open Sqldiff.Core (ExprSem WorldModel Obs CallEvent SExpr)
open Sqldiff.Gate (Answer)

def isConfirmed : Answer → Bool
  | .confirmed _ => true
  | .unconfirmed _ => false

def reasonOf : Answer → String
  | .confirmed _ => ""
  | .unconfirmed r => r

/-! ## Building blocks -/

def int : Ast.Ty := .Ty "int" []
def integer : Ast.Ty := .Ty "integer" []
/-- A T-SQL variable and a PL/pgSQL one. -/
def tv (x : String) : Ast.Expr := .Var x
def pv (x : String) : Ast.Expr := .Name [x]
def num (n : Nat) : Ast.Expr := .Num (toString n)
def fn (n : String) (args : List Ast.Expr) : Ast.Expr :=
  .CallE (.CallF [n] args false [] false false false)
def set (x v : Ast.Expr) : Ast.Node := .NStmt (.Assign x v)
def ret (e : Option Ast.Expr) : Ast.Node := .NStmt (.Return e)
def declT (x : String) (v : Option Ast.Expr) : Ast.Node := .NStmt (.Declare x (some int) v)
def declP (x : String) (v : Option Ast.Expr) : Ast.Stmt := .Declare x (some integer) v
def arg (e : Ast.Expr) (output : Bool) : Ast.Arg := .Arg none e output
def tsqlProc (name : List String) (ps : List Ast.Param) (body : List Ast.Node) : Ast.Routine :=
  .Routine .Procedure name ps none .Caller none (.Block [] body)
def pgProc (name : List String) (ps : List Ast.Param) (decls : List Ast.Stmt)
    (body : List Ast.Node) : Ast.Routine :=
  .Routine .Procedure name ps none .Caller none (.Block decls body)
def returnCode : Ast.Param := .Param "return_code" integer .InOut (some .Null)

/-! ## The pair

The converter's input (with `SET NOCOUNT ON` and a call added):

```sql
CREATE PROCEDURE dbo.p @a int, @b int = 2 OUTPUT AS
BEGIN
  SET NOCOUNT ON;
  DECLARE @x int = 1;
  IF @a > 1 SET @x = 2;
  ELSE IF @a < 0 BEGIN SET @x = 3; END
  ELSE SET @x = 4;
  WHILE @x < 10 SET @x = @x + 1;
  EXEC dbo.log_value @x, @b OUTPUT;
  RETURN @x;
END
```

and the mirror C1 confirms. It differs from what the converter emits
today in two lines, both about the return status (`Sqldiff.Mirror.statusVar`):
`return_code := 0` first, and `COALESCE(v_x, 0)` rather than `v_x`.

```sql
CREATE OR REPLACE PROCEDURE p(p_a integer, INOUT p_b integer DEFAULT 2,
    INOUT return_code integer DEFAULT NULL) LANGUAGE plpgsql AS $$
DECLARE v_x integer;
BEGIN
  return_code := 0;
  v_x := 1;
  IF p_a > 1 THEN v_x := 2; ELSIF p_a < 0 THEN v_x := 3; ELSE v_x := 4; END IF;
  WHILE v_x < 10 LOOP v_x := v_x + 1; END LOOP;
  CALL log_value(v_x, p_b);
  return_code := COALESCE(v_x, 0);
  RETURN;
END; $$;
```
-/

def tsqlParams : List Ast.Param :=
  [.Param "@a" int .In none, .Param "@b" int .InOut (some (num 2))]

/-- The T-SQL procedure; `output` says whether `@b` is passed `OUTPUT`. -/
def srcWith (output : Bool) : Ast.Routine :=
  tsqlProc ["dbo", "p"] tsqlParams [
    .Setting "NOCOUNT" true,
    declT "@x" (some (num 1)),
    .If (.Bin ">" (tv "@a") (num 1)) [set (tv "@x") (num 2)]
      [.If (.Bin "<" (tv "@a") (num 0)) [.NBlock (.Block [] [set (tv "@x") (num 3)])]
        [set (tv "@x") (num 4)]],
    .While (.Bin "<" (tv "@x") (num 10)) [set (tv "@x") (.Bin "+" (tv "@x") (num 1))],
    .Call ["dbo", "log_value"] [arg (tv "@x") false, arg (tv "@b") output] none,
    ret (some (tv "@x"))]

def srcT : Ast.Routine := srcWith true

/-- The PL/pgSQL body, with the two status lines and the `ELSIF` branches
as parameters. -/
def pgBody (first : List Ast.Node) (status : Ast.Expr) (b3 b4 : Nat := 3) : List Ast.Node :=
  first ++ [
    set (pv "v_x") (num 1),
    .If (.Bin ">" (pv "p_a") (num 1)) [set (pv "v_x") (num 2)]
      [.If (.Bin "<" (pv "p_a") (num 0)) [set (pv "v_x") (num b3)]
        [set (pv "v_x") (num (if b4 = 3 then 4 else b4))]],
    .While (.Bin "<" (pv "v_x") (num 10)) [set (pv "v_x") (.Bin "+" (pv "v_x") (num 1))],
    .Call ["log_value"] [arg (pv "v_x") false, arg (pv "p_b") false] none,
    set (pv "return_code") status,
    ret none]

def pgParams : List Ast.Param :=
  [.Param "p_a" integer .In none, .Param "p_b" integer .InOut (some (num 2)), returnCode]

def mirrorP : Ast.Routine :=
  pgProc ["p"] pgParams [declP "v_x" none]
    (pgBody [set (pv "return_code") (num 0)] (fn "COALESCE" [pv "v_x", num 0]))

/-- What the converter emits today. -/
def todayP : Ast.Routine :=
  pgProc ["p"] pgParams [declP "v_x" none] (pgBody [] (pv "v_x"))

def dep : Deployment := { rowCounts := false, routines := [("dbo.log_value", "log_value")] }
def Γ : Catalog := { routines := [("log_value", [.In, .InOut, .InOut])] }
def cert : Cert :=
  ⟨[("@a", "p_a"), ("@b", "p_b"), ("@x", "v_x"), (statusVar, "return_code")]⟩

/-! ## Confirmed -/

#guard isConfirmed (check dep Γ cert srcT mirrorP)
-- The certificate's entries may come in any order.
#guard isConfirmed (check dep Γ ⟨cert.vars.reverse⟩ srcT mirrorP)
-- PostgreSQL folds unquoted names: `V_X`, `P_A` and `Log_Value` are `v_x`, `p_a`, `log_value`.
#guard isConfirmed (check dep Γ cert srcT
  (pgProc ["p"] [.Param "P_A" integer .In none, .Param "p_b" integer .InOut (some (num 2)),
      returnCode] [declP "V_X" none]
    ([set (pv "return_code") (num 0), set (pv "V_x") (num 1)] ++
      (pgBody [] (fn "COALESCE" [pv "v_x", num 0])).drop 1 |>.map fun n =>
        match n with
        | .Call _ args r => .Call ["Log_Value"] args r
        | n => n)))

/-- `DECLARE` with a value inside a loop assigns on every iteration: the
mirror declares at the top and assigns in the loop. -/
def loopT : Ast.Routine :=
  tsqlProc ["q"] [.Param "@n" int .InOut none] [
    .While (.Bin "<" (tv "@n") (num 3)) [
      declT "@y" (some (num 0)),
      set (tv "@y") (.Bin "+" (tv "@y") (num 1)),
      set (tv "@n") (.Bin "+" (tv "@n") (tv "@y"))]]
def loopP (decl : Option Ast.Expr) (inLoop : List Ast.Node) : Ast.Routine :=
  pgProc ["q"] [.Param "n" integer .InOut none, returnCode] [declP "y" decl] [
    set (pv "return_code") (num 0),
    .While (.Bin "<" (pv "n") (num 3)) (inLoop ++ [
      set (pv "y") (.Bin "+" (pv "y") (num 1)),
      set (pv "n") (.Bin "+" (pv "n") (pv "y"))])]
def loopCert : Cert := ⟨[("@n", "n"), ("@y", "y"), (statusVar, "return_code")]⟩

#guard isConfirmed (check dep ∅ loopCert loopT (loopP none [set (pv "y") (num 0)]))
-- Hoisting the value into the declaration runs it once: not the mirror.
#guard reasonOf (check dep ∅ loopCert loopT (loopP (some (num 0)) [])) ==
  "the lowered routines differ beyond the renaming"

/-! ## Not confirmed -/

/- What the converter emits today: no `return_code := 0`, and a NULL `v_x`
would give the caller NULL where T-SQL gives 0. In this routine `v_x` is
never NULL and every path ends in `RETURN`, so the outputs happen to agree
(see the oracle below); C1 cannot see that, it needs values (C2). -/
#guard reasonOf (check dep Γ cert srcT todayP) ==
  "the lowered routines differ beyond the renaming"

-- The `ELSIF` branches swapped.
#guard reasonOf (check dep Γ cert srcT (pgProc ["p"] pgParams [declP "v_x" none]
    (pgBody [set (pv "return_code") (num 0)] (fn "COALESCE" [pv "v_x", num 0]) 4 3))) ==
  "the lowered routines differ beyond the renaming"

/- A wrong renaming: `@a` and `@x` exchanged. Consistent and injective, but
the renamed source is not the target. -/
#guard reasonOf (check dep Γ ⟨[("@a", "v_x"), ("@b", "p_b"), ("@x", "p_a"),
    (statusVar, "return_code")]⟩ srcT mirrorP) == "the lowered routines differ beyond the renaming"

-- A non-injective renaming: `@a` and `@x` both to `v_x`.
#guard reasonOf (check dep Γ ⟨[("@a", "v_x"), ("@b", "p_b"), ("@x", "v_x"),
    (statusVar, "return_code")]⟩ srcT mirrorP) ==
  "certificate: two T-SQL variables are given one PL/pgSQL name"

-- An inconsistent certificate, and one that leaves out the status.
#guard reasonOf (check dep Γ ⟨cert.vars ++ [("@x", "y")]⟩ srcT mirrorP) ==
  "certificate: a T-SQL variable is given two PL/pgSQL names"
#guard reasonOf (check dep Γ ⟨cert.vars.take 3⟩ srcT mirrorP) ==
  "certificate: a T-SQL variable has no PL/pgSQL name"

/- `EXEC` without `OUTPUT` does not copy `@b` back; the `CALL` to an
`INOUT` parameter does. -/
#guard reasonOf (check dep Γ cert (srcWith false) mirrorP) ==
  "the lowered routines differ beyond the renaming"

-- T-SQL `OUTPUT` is `INOUT`; an `OUT` parameter starts NULL.
#guard !isConfirmed (check dep Γ cert srcT
  (pgProc ["p"] [.Param "p_a" integer .In none, .Param "p_b" integer .Out none, returnCode]
    [declP "v_x" none] (pgBody [set (pv "return_code") (num 0)] (fn "COALESCE" [pv "v_x", num 0]))))

-- An observer that reads row counts: `SET NOCOUNT ON` is outside C1.
#guard reasonOf (check { dep with rowCounts := true } Γ cert srcT mirrorP) ==
  "the T-SQL routine is outside the C1 spec"

-- A callee the deployment does not map, and one the target catalog lacks.
#guard reasonOf (check { dep with routines := [] } Γ cert srcT mirrorP) ==
  "the T-SQL routine is outside the C1 spec"
#guard reasonOf (check dep ∅ cert srcT mirrorP) == "the PL/pgSQL routine is outside the C1 spec"

/- `@I` and `@i` are one variable or two depending on the collation:
outside the spec. -/
#guard reasonOf (check dep ∅ ⟨[("@I", "a"), ("@i", "b"), (statusVar, "return_code")]⟩
    (tsqlProc ["r"] [.Param "@I" int .InOut none] [declT "@i" none, set (tv "@i") (num 1)])
    (pgProc ["r"] [.Param "a" integer .InOut none, returnCode] [declP "b" none]
      [set (pv "return_code") (num 0), set (pv "b") (num 1)])) ==
  "the T-SQL routine is outside the C1 spec"

/- PostgreSQL folds `X` to `x`: `DECLARE X int; x int;` declares one name
twice, and `X := 1; out1 := x` reads the 1 just written. Without the fold
the core would see two variables, and this pair, where T-SQL has two
variables and returns NULL in `@out`, would be confirmed. -/
def twoT : Ast.Routine :=
  tsqlProc ["r"] [.Param "@out" int .InOut none]
    [declT "@x" none, declT "@y" none, set (tv "@x") (num 1), set (tv "@out") (tv "@y")]
def oneP : Ast.Routine :=
  pgProc ["r"] [.Param "out1" integer .InOut none, returnCode] [declP "X" none, declP "x" none]
    [set (pv "return_code") (num 0), set (pv "X") (num 1), set (pv "out1") (pv "x")]
#guard reasonOf (check dep ∅ ⟨[("@out", "out1"), ("@x", "X"), ("@y", "x"),
    (statusVar, "return_code")]⟩ twoT oneP) == "the PL/pgSQL routine is outside the C1 spec"
-- With `"X` quoted on both uses the names are distinct, and the pair is a mirror.
#guard isConfirmed (check dep ∅ ⟨[("@out", "out1"), ("@x", "\"X"), ("@y", "y"),
    (statusVar, "return_code")]⟩ twoT
  (pgProc ["r"] [.Param "out1" integer .InOut none, returnCode] [declP "\"X" none, declP "y" none]
    [set (pv "return_code") (num 0), set (pv "\"X") (num 1), set (pv "out1") (pv "y")]))

-- An undeclared PL/pgSQL name is an error in PostgreSQL, not a NULL.
#guard reasonOf (check dep ∅ ⟨[("@out", "out1"), ("@y", "y"), (statusVar, "return_code")]⟩
    (tsqlProc ["r"] [.Param "@out" int .InOut none] [declT "@y" none, set (tv "@out") (tv "@y")])
    (pgProc ["r"] [.Param "out1" integer .InOut none, returnCode] []
      [set (pv "return_code") (num 0), set (pv "out1") (pv "y")])) ==
  "the PL/pgSQL routine is outside the C1 spec"

-- `@@ROWCOUNT` is a system function, not a variable.
#guard reasonOf (check dep ∅ ⟨[("@x", "x"), (statusVar, "return_code")]⟩
    (tsqlProc ["r"] [.Param "@x" int .InOut none] [set (tv "@x") (tv "@@ROWCOUNT")])
    (pgProc ["r"] [.Param "x" integer .InOut none, returnCode] []
      [set (pv "return_code") (num 0), set (pv "x") (pv "row_count")])) ==
  "the T-SQL routine is outside the C1 spec"

/- `EXEC @rc = p @a`: the status is `p`'s last parameter, which C1 cannot
place without `p`'s arity, so this is outside C1. (Appending `@rc` after
the arguments would have confirmed `CALL p(a, rc)` passing `rc` into an
ordinary second parameter.) -/
#guard reasonOf (check { dep with routines := [("p2", "p2")] } { routines := [("p2", [.In, .InOut, .InOut])] }
    ⟨[("@a", "a"), ("@rc", "rc"), (statusVar, "return_code")]⟩
    (tsqlProc ["r"] [.Param "@a" int .In none]
      [declT "@rc" none, .Call ["p2"] [arg (tv "@a") false] (some (tv "@rc"))])
    (pgProc ["r"] [.Param "a" integer .In none, returnCode] [declP "rc" none]
      [set (pv "return_code") (num 0), .Call ["p2"] [arg (pv "a") false, arg (pv "rc") false] none])) ==
  "the T-SQL routine is outside the C1 spec"

-- A PostgreSQL procedure cannot `RETURN` a value.
#guard reasonOf (check dep ∅ ⟨[(statusVar, "return_code")]⟩
    (tsqlProc ["r"] [] [ret (some (num 1))])
    (pgProc ["r"] [returnCode] [] [set (pv "return_code") (num 0), ret (some (num 1))])) ==
  "the PL/pgSQL routine is outside the C1 spec"

-- A nested PL/pgSQL block with its own `DECLARE` re-initialises on entry.
#guard reasonOf (check dep ∅ loopCert loopT
    (pgProc ["q"] [.Param "n" integer .InOut none, returnCode] [] [
      set (pv "return_code") (num 0),
      .While (.Bin "<" (pv "n") (num 3)) [
        .NBlock (.Block [declP "y" (some (num 0))] [
          set (pv "y") (.Bin "+" (pv "y") (num 1)),
          set (pv "n") (.Bin "+" (pv "n") (pv "y"))])]])) ==
  "the PL/pgSQL routine is outside the C1 spec"

-- A query is outside C1.
#guard reasonOf (check dep ∅ ⟨[(statusVar, "return_code")]⟩
    (tsqlProc ["r"] [] [.NStmt (.Delete ["t"] none [] none)])
    (pgProc ["r"] [returnCode] [] [set (pv "return_code") (num 0)])) ==
  "the T-SQL routine is outside the C1 spec"

/-! ## A procedure without `RETURN e`

Its status is always 0, so the PL/pgSQL mirror needs no `return_code`
(the deployment convention in `Sqldiff.Mirror.statusName`). -/

def plainT : Ast.Routine :=
  tsqlProc ["r"] [.Param "@n" int .InOut none] [set (tv "@n") (.Bin "+" (tv "@n") (num 1))]
def plainP : Ast.Routine :=
  pgProc ["r"] [.Param "n" integer .InOut none] [] [set (pv "n") (.Bin "+" (pv "n") (num 1))]
def plainCert : Cert := ⟨[("@n", "n"), (statusVar, statusVar)]⟩

#guard isConfirmed (check dep ∅ plainCert plainT plainP)
-- A T-SQL `RETURN e` sets the status, which a mirror without
-- `return_code` cannot show.
#guard reasonOf (check dep ∅ plainCert
    (tsqlProc ["r"] [.Param "@n" int .InOut none]
      [set (tv "@n") (.Bin "+" (tv "@n") (num 1)), ret (some (num 7))]) plainP) ==
  "the lowered routines differ beyond the renaming"

/-! ## The core as an executable oracle

Values of type `Val Unit`: the routines below are in the slice, so no
uninterpreted operator is evaluated (`es` fails if one were). `log_value`
counts its calls in the world and adds its first argument to its
second. -/

abbrev V := Core.Val Unit
def I (n : Int) : V := .int n
def N : V := .null

def es : ExprSem Unit := ⟨fun _ _ => .err (.other "op")⟩

def addV : V → V → V
  | .int x, .int y => .int (x + y)
  | _, _ => .null

def wm : WorldModel Unit Nat where
  call p vs w db := match p, vs with
    | "log_value", [v, b] => (w + 1, db, [v, addV v b, .int 0])
    | _, vs => (w, db, vs)

def run (r : Option Core.Routine) (args : List V) (fuel : Nat) : Option (Obs Unit Nat) :=
  r.map fun r => r.obs es wm args 0 [] fuel

def T := lowerT dep Γ srcT
def P := lowerP Γ mirrorP

-- `@a = 5`: `@x` goes 2 to 10, `@b` becomes 7 + 10, the status is 10.
#guard run T [I 5, I 7] 100 ==
  some (.done [I 17, I 10] none 1 [] [⟨"log_value", [I 10, I 7]⟩])
#guard run P [I 5, I 7] 100 == run T [I 5, I 7] 100
-- `@b` omitted: its default 2.
#guard run T [I (-3)] 100 ==
  some (.done [I 12, I 10] none 1 [] [⟨"log_value", [I 10, I 2]⟩])
#guard run P [I (-3)] 100 == run T [I (-3)] 100
-- `@a` NULL: `@a > 1` and `@a < 0` are UNKNOWN, so the last `ELSE` runs.
#guard run T [N, I 7] 100 ==
  some (.done [I 17, I 10] none 1 [] [⟨"log_value", [I 10, I 7]⟩])
#guard run P [N, I 7] 100 == run T [N, I 7] 100
-- Too little fuel: both cut off at the same point.
#guard run T [I 5, I 7] 4 == run P [I 5, I 7] 4
#guard match run T [I 5, I 7] 4 with
  | some (.pending _ []) => true
  | _ => false

-- Today's output agrees here, because `v_x` is never NULL ...
#guard run (lowerP Γ todayP) [I 5, I 7] 100 == run T [I 5, I 7] 100

/- ... but not in general. `RETURN @x` with `@x` NULL returns status 0;
`return_code := v_x` gives NULL. And a procedure that ends without
`RETURN e` has status 0, while `return_code` keeps what the caller passed
(NULL by default). -/
def nullT : Ast.Routine := tsqlProc ["s"] [] [declT "@x" none, ret (some (tv "@x"))]
def nullP : Ast.Routine := pgProc ["s"] [returnCode] [declP "v_x" none]
  [set (pv "return_code") (pv "v_x"), ret none]
#guard run (lowerT dep Γ nullT) [] 10 == some (.done [I 0] none 0 [] [])
#guard run (lowerP ∅ nullP) [] 10 == some (.done [N] none 0 [] [])

def endT : Ast.Routine := tsqlProc ["s"] [] []
def endP : Ast.Routine := pgProc ["s"] [returnCode] [] []
#guard run (lowerT dep Γ endT) [] 10 == some (.done [I 0] none 0 [] [])
#guard run (lowerP ∅ endP) [] 10 == some (.done [N] none 0 [] [])

/-! ## C2a: the scalar slice

Which theorem a confirmation names: `check_sound_c2a` when every
expression of both routines is in the slice (and the T-SQL routine aborts
on errors or cannot raise one), `check_sound` otherwise. -/

def theoremOf : Answer → String
  | .confirmed t => t
  | .unconfirmed _ => ""

def isC2a (a : Answer) : Bool := theoremOf a == theoremC2a
def isC1 (a : Answer) : Bool := theoremOf a == theoremC1

def bigint : Ast.Ty := .Ty "BIGINT" []
def xactAbort : Ast.Node := .Setting "XACT_ABORT" true
def inP (x : String) (ty : Ast.Ty := integer) : Ast.Param := .Param x ty .In none
def ioP (x : String) (ty : Ast.Ty := integer) : Ast.Param := .Param x ty .InOut none
def inT (x : String) (ty : Ast.Ty := int) : Ast.Param := .Param x ty .In none
def ioT (x : String) (ty : Ast.Ty := int) : Ast.Param := .Param x ty .InOut none

/-- `@r = e` in T-SQL and `r := e'` in PL/pgSQL, with parameters `a`, `b`
in and `r` in-out, the T-SQL side optionally under `XACT_ABORT ON`; the
PL/pgSQL procedure has no `return_code` (its status is 0, as the T-SQL
procedure's, `Sqldiff.Mirror.statusName`). -/
def pairT (abort : Bool) (e : Ast.Expr) (ty : Ast.Ty := int) : Ast.Routine :=
  tsqlProc ["r"] [inT "@a" ty, inT "@b" ty, ioT "@r" ty]
    ((if abort then [xactAbort] else []) ++ [set (tv "@r") e])
def pairP (e : Ast.Expr) (ty : Ast.Ty := integer) : Ast.Routine :=
  pgProc ["r"] [inP "a" ty, inP "b" ty, ioP "r" ty] [] [set (pv "r") e]
def pairCert : Cert := ⟨[("@a", "a"), ("@b", "b"), ("@r", "r"), (statusVar, statusVar)]⟩
def checkPair (abort : Bool) (t p : Ast.Expr) : Answer :=
  check dep ∅ pairCert (pairT abort t) (pairP p)
def runPair (abort : Bool) (t p : Ast.Expr) (args : List V) :
    Option (Obs Unit Nat) × Option (Obs Unit Nat) :=
  (run (lowerT dep Γ (pairT abort t)) args 20, run (lowerP ∅ (pairP p)) args 20)

def ta : Ast.Expr := tv "@a"
def tb : Ast.Expr := tv "@b"
def pa : Ast.Expr := pv "a"
def pb : Ast.Expr := pv "b"

/-! ### Arithmetic: overflow and division by zero are error observations -/

-- `@a / @b` against `a / b`, under `XACT_ABORT ON`: in the slice.
#guard isC2a (checkPair true (.Bin "/" ta tb) (.Bin "/" pa pb))
-- Without `XACT_ABORT ON` T-SQL goes on after the failed statement, so the
-- confirmation stays on the C1 theorem.
#guard isC1 (checkPair false (.Bin "/" ta tb) (.Bin "/" pa pb))
-- Truncation toward zero, on both sides.
#guard runPair true (.Bin "/" ta tb) (.Bin "/" pa pb) [I (-7), I 2, N] ==
  (some (.done [I (-3), I 0] none 0 [] []), some (.done [I (-3), I 0] none 0 [] []))
-- Division by zero: an arithmetic error ends both routines.
#guard runPair true (.Bin "/" ta tb) (.Bin "/" pa pb) [I 1, I 0, I 5] ==
  (some (.failed .arithmetic []), some (.failed .arithmetic []))
-- `-2147483648 / -1` overflows.
#guard runPair true (.Bin "/" ta tb) (.Bin "/" pa pb) [I (-2147483648), I (-1), N] ==
  (some (.failed .arithmetic []), some (.failed .arithmetic []))
-- NULL in, NULL out.
#guard runPair true (.Bin "/" ta tb) (.Bin "/" pa pb) [N, I 0, I 5] ==
  (some (.done [N, I 0] none 0 [] []), some (.done [N, I 0] none 0 [] []))
-- `2147483647 + 1` overflows on both sides.
#guard isC2a (checkPair true (.Bin "+" ta tb) (.Bin "+" pa pb))
#guard runPair true (.Bin "+" ta tb) (.Bin "+" pa pb) [I 2147483647, I 1, N] ==
  (some (.failed .arithmetic []), some (.failed .arithmetic []))
-- `-(-2147483648)` overflows.
#guard runPair true (.Unary "-" ta) (.Unary "-" pa) [I (-2147483648), N, N] ==
  (some (.failed .arithmetic []), some (.failed .arithmetic []))
#guard runPair true (.Unary "-" ta) (.Unary "-" pa) [I 5, N, N] ==
  (some (.done [I (-5), I 0] none 0 [] []), some (.done [I (-5), I 0] none 0 [] []))
-- The error keeps the calls made before it.
def callThenDivT : Ast.Routine :=
  tsqlProc ["r"] [inT "@a", inT "@b", ioT "@r"]
    [xactAbort, .Call ["dbo", "log_value"] [arg ta false, arg (tv "@r") true] none,
      set (tv "@r") (.Bin "/" ta tb)]
def callThenDivP : Ast.Routine :=
  pgProc ["r"] [inP "a", inP "b", ioP "r"] []
    [.Call ["log_value"] [arg pa false, arg (pv "r") false] none, set (pv "r") (.Bin "/" pa pb)]
#guard isC2a (check dep Γ pairCert callThenDivT callThenDivP)
#guard run (lowerT dep Γ callThenDivT) [I 1, I 0, I 2] 20 ==
  some (.failed .arithmetic [⟨"log_value", [I 1, I 2]⟩])
#guard run (lowerP Γ callThenDivP) [I 1, I 0, I 2] 20 ==
  run (lowerT dep Γ callThenDivT) [I 1, I 0, I 2] 20
/- The division can fail after the call, which may have written: what the
caller sees of that after the failure differs between the dialects (C4),
so the answer names `ASM-STATE-AFTER-ERROR`, and the failed observation
above has no world. -/
#guard (residualOf dep Γ callThenDivT) ==
  some ["ASM-TSQL-ARITH-ERRORS", "ASM-STATE-AFTER-ERROR"]

-- The slice's meaning, operator by operator (`SExpr.eval`).
def σ2 (a b : V) : Core.Store Unit := fun x => if x = "a" then a else if x = "b" then b else .null
def ev (s : SExpr) (a b : V) : Core.Res Unit := s.eval (σ2 a b) Core.nullStore
#guard ev (.arith .div (.var "a") (.var "b")) (I 7) (I 2) == .ok (I 3)
#guard ev (.arith .div (.var "a") (.var "b")) (I (-7)) (I 2) == .ok (I (-3))
#guard ev (.arith .mod (.var "a") (.int 2)) (I (-7)) N == .ok (I (-1))
#guard ev (.arith .mod (.var "a") (.int (-2))) (I 7) N == .ok (I 1)
#guard ev (.arith .div (.var "a") (.var "b")) (I 1) (I 0) == .err .divByZero
#guard ev (.arith .add (.var "a") (.var "b")) (I 2147483647) (I 1) == .err .overflow
#guard ev (.arith .mul (.var "a") (.var "b")) (I 65536) (I 32768) == .err .overflow
#guard ev (.arith .sub (.var "a") (.var "b")) (I (-2147483648)) (I 1) == .err .overflow
#guard ev (.arith .div (.var "a") (.var "b")) (I (-2147483648)) (I (-1)) == .err .overflow
#guard ev (.neg (.var "a")) (I (-2147483648)) N == .err .overflow
-- A NULL operand gives NULL, but the other operand is still evaluated.
#guard ev (.arith .add (.var "a") (.var "b")) N (I 1) == .ok N
#guard ev (.arith .add (.var "a") (.arith .div (.var "b") (.int 0))) N (I 1) == .err .divByZero
-- Three-valued comparisons and connectives.
#guard ev (.cmp .eq (.var "a") (.var "b")) N N == .ok N
#guard ev (.cmp .ne (.var "a") (.var "b")) (I 1) N == .ok N
#guard ev (.cmp .lt (.var "a") (.var "b")) (I 1) (I 2) == .ok (.bool true)
#guard ev (.and (.cmp .eq (.var "a") (.int 1)) (.cmp .eq (.var "b") (.int 1))) (I 2) N ==
  .ok (.bool false)
#guard ev (.and (.cmp .eq (.var "a") (.int 1)) (.cmp .eq (.var "b") (.int 1))) (I 1) N == .ok N
#guard ev (.or (.cmp .eq (.var "a") (.int 1)) (.cmp .eq (.var "b") (.int 1))) (I 1) N ==
  .ok (.bool true)
#guard ev (.or (.cmp .eq (.var "a") (.int 1)) (.cmp .eq (.var "b") (.int 1))) (I 2) N == .ok N
#guard ev (.not (.cmp .eq (.var "a") (.int 1))) N N == .ok N
#guard ev (.isNull (.var "a") false) N N == .ok (.bool true)
#guard ev (.isNull (.var "a") true) N N == .ok (.bool false)
-- `COALESCE` and `CASE` evaluate only what they need.
#guard ev (.coalesce (.var "a") (.arith .div (.int 1) (.var "b"))) (I 3) (I 0) == .ok (I 3)
#guard ev (.coalesce (.var "a") (.arith .div (.int 1) (.var "b"))) N (I 0) == .err .divByZero
#guard ev (.cond (.cmp .eq (.var "b") (.int 0)) .null (.arith .div (.var "a") (.var "b")))
  (I 1) (I 0) == .ok N
-- An UNKNOWN `WHEN` goes on to the next branch.
#guard ev (.cond (.cmp .eq (.var "b") (.int 0)) (.int 1) (.int 2)) (I 1) N == .ok (I 2)

/-! ### Comparisons with NULL and three-valued `IF`

```sql
CREATE PROCEDURE dbo.r @a int, @b int, @r int OUTPUT AS
BEGIN
  IF @a = @b SET @r = 1;
  ELSE IF NOT (@a = @b) SET @r = 2;
  ELSE SET @r = 3;
END
```

With `@a` or `@b` NULL, `@a = @b` and `NOT (@a = @b)` are both UNKNOWN,
so `@r` is 3. Nothing can raise, so no `XACT_ABORT` is needed. -/

def ifT : Ast.Routine :=
  tsqlProc ["r"] [inT "@a", inT "@b", ioT "@r"] [
    .If (.Bin "=" ta tb) [set (tv "@r") (num 1)]
      [.If (.Unary "NOT" (.Bin "=" ta tb)) [set (tv "@r") (num 2)] [set (tv "@r") (num 3)]]]
def ifP : Ast.Routine :=
  pgProc ["r"] [inP "a", inP "b", ioP "r"] [] [
    .If (.Bin "=" pa pb) [set (pv "r") (num 1)]
      [.If (.Unary "NOT" (.Bin "=" pa pb)) [set (pv "r") (num 2)] [set (pv "r") (num 3)]]]

#guard isC2a (check dep ∅ pairCert ifT ifP)
#guard run (lowerT dep Γ ifT) [I 1, I 1, N] 20 == some (.done [I 1, I 0] none 0 [] [])
#guard run (lowerT dep Γ ifT) [I 1, I 2, N] 20 == some (.done [I 2, I 0] none 0 [] [])
#guard run (lowerT dep Γ ifT) [N, I 2, N] 20 == some (.done [I 3, I 0] none 0 [] [])
#guard run (lowerP ∅ ifP) [N, I 2, N] 20 == run (lowerT dep Γ ifT) [N, I 2, N] 20
#guard run (lowerP ∅ ifP) [N, N, N] 20 == some (.done [I 3, I 0] none 0 [] [])
-- Swapping the branches of an `IF` on a possibly UNKNOWN condition is not
-- `IF NOT`: refused.
#guard reasonOf (check dep ∅ pairCert ifT
  (pgProc ["r"] [inP "a", inP "b", ioP "r"] [] [
    .If (.Unary "NOT" (.Bin "=" pa pb)) [set (pv "r") (num 2)]
      [.If (.Bin "=" pa pb) [set (pv "r") (num 1)] [set (pv "r") (num 3)]]])) ==
  "the lowered routines differ beyond the renaming"
-- `WHILE` on UNKNOWN does not loop.
def whileT : Ast.Routine :=
  tsqlProc ["r"] [inT "@a", inT "@b", ioT "@r"] [
    set (tv "@r") (num 0),
    .While (.Bin "<" (tv "@r") ta) [set (tv "@r") (.Bin "+" (tv "@r") (num 1))]]
#guard run (lowerT dep Γ whileT) [I 3, N, N] 20 == some (.done [I 3, I 0] none 0 [] [])
#guard run (lowerT dep Γ whileT) [N, N, N] 20 == some (.done [I 0, I 0] none 0 [] [])

/-! ### `COALESCE` and `ISNULL` -/

-- T-SQL `ISNULL(@a, @b)` with `int`s is PostgreSQL `COALESCE(a, b)`.
#guard isC2a (checkPair false (fn "ISNULL" [ta, tb]) (fn "COALESCE" [pa, pb]))
#guard isC2a (checkPair false (fn "COALESCE" [ta, tb]) (fn "COALESCE" [pa, pb]))
#guard isC2a (checkPair false (fn "ISNULL" [ta, num 0]) (fn "COALESCE" [pa, num 0]))
#guard isC2a (checkPair false (fn "COALESCE" [ta, tb, num 0]) (fn "COALESCE" [pa, pb, num 0]))
#guard isC2a (checkPair false (fn "ISNULL" [ta, .Null]) (fn "COALESCE" [pa, .Null]))
#guard runPair false (fn "ISNULL" [ta, tb]) (fn "COALESCE" [pa, pb]) [N, I 4, N] ==
  (some (.done [I 4, I 0] none 0 [] []), some (.done [I 4, I 0] none 0 [] []))
#guard runPair false (fn "ISNULL" [ta, tb]) (fn "COALESCE" [pa, pb]) [I 1, I 4, N] ==
  (some (.done [I 1, I 0] none 0 [] []), some (.done [I 1, I 0] none 0 [] []))
-- `ISNULL(NULL, @b)`: the first argument must be an `int` expression.
#guard (typed (.tsql dep Γ) [("@b", int)] {} (fn "ISNULL" [.Null, tb])).isNone
-- `COALESCE(NULL, NULL)` has no `int` argument.
#guard (typed (.tsql dep Γ) [] {} (fn "COALESCE" [.Null, .Null])).isNone
-- `COALESCE(x, y, z)` is not `COALESCE(COALESCE(x, y), z)` as a term.
#guard reasonOf (checkPair false (fn "COALESCE" [ta, tb, num 0])
    (fn "COALESCE" [fn "COALESCE" [pa, pb], num 0])) ==
  "the lowered routines differ beyond the renaming"
/- A replacement that can raise: whether T-SQL evaluates it when `@a` is
not NULL is not documented, so `ISNULL` is outside the slice and does not
lower like `COALESCE`. -/
#guard reasonOf (checkPair true (fn "ISNULL" [ta, .Bin "+" tb (num 1)])
    (fn "COALESCE" [pa, .Bin "+" pb (num 1)])) ==
  "the lowered routines differ beyond the renaming"
/- With `bigint` (outside the slice) `ISNULL` and `COALESCE` stay two
operators: `ASM-ISNULL-TYPE` is not discharged there. -/
#guard reasonOf (check dep ∅ pairCert (pairT false (fn "ISNULL" [ta, tb]) bigint)
    (pairP (fn "COALESCE" [pa, pb]) bigint)) ==
  "the lowered routines differ beyond the renaming"
-- PostgreSQL has no `ISNULL` function: never the slice's `coalesce`.
#guard (typed (.pg ∅ []) [("a", integer), ("b", integer)] {} (fn "ISNULL" [pa, pb])).isNone

/-! ### `CASE` -/

def safeDiv (a b : Ast.Expr) : Ast.Expr :=
  .Case none [.When2 (.Bin "=" b (num 0)) .Null] (some (.Bin "/" a b))
#guard isC2a (checkPair true (safeDiv ta tb) (safeDiv pa pb))
#guard runPair true (safeDiv ta tb) (safeDiv pa pb) [I 7, I 0, I 9] ==
  (some (.done [N, I 0] none 0 [] []), some (.done [N, I 0] none 0 [] []))
#guard runPair true (safeDiv ta tb) (safeDiv pa pb) [I 7, I 2, I 9] ==
  (some (.done [I 3, I 0] none 0 [] []), some (.done [I 3, I 0] none 0 [] []))
-- Without `ELSE`, NULL.
#guard isC2a (checkPair false (.Case none [.When2 (.Bin ">" ta tb) ta] none)
  (.Case none [.When2 (.Bin ">" pa pb) pa] none))
#guard runPair false (.Case none [.When2 (.Bin ">" ta tb) ta] none)
  (.Case none [.When2 (.Bin ">" pa pb) pa] none) [I 1, I 2, I 9] ==
  (some (.done [N, I 0] none 0 [] []), some (.done [N, I 0] none 0 [] []))
/- A branch without variables that fails: PostgreSQL may fail while
planning even when the branch is not taken, so it is outside the slice;
the pair is still a C1 mirror. -/
def badCase (a : Ast.Expr) : Ast.Expr :=
  .Case none [.When2 (.Bin ">" a (num 0)) (num 1)] (some (.Bin "/" (num 1) (num 0)))
#guard isC1 (checkPair true (badCase ta) (badCase pa))
-- A constant branch that does not fail is fine.
#guard isC2a (checkPair true
  (.Case none [.When2 (.Bin ">" ta (num 0)) (num 1)] (some (.Bin "/" (num 6) (num 3))))
  (.Case none [.When2 (.Bin ">" pa (num 0)) (num 1)] (some (.Bin "/" (num 6) (num 3)))))
-- Branch order matters.
#guard reasonOf (checkPair false
    (.Case none [.When2 (.Bin ">" ta tb) ta, .When2 (.Bin "<" ta tb) tb] (some (num 0)))
    (.Case none [.When2 (.Bin "<" pa pb) pb, .When2 (.Bin ">" pa pb) pa] (some (num 0)))) ==
  "the lowered routines differ beyond the renaming"

/-! ### Refused: the lowerings differ -/

-- `@a / @b` (integer division) against `a::numeric / b`.
#guard reasonOf (checkPair true (.Bin "/" ta tb) (.Bin "/" (.Cast pa (.Ty "NUMERIC" [])) pb)) ==
  "the lowered routines differ beyond the renaming"
-- `+` against `||`.
#guard reasonOf (checkPair true (.Bin "+" ta tb) (.Bin "||" pa pb)) ==
  "the lowered routines differ beyond the renaming"
-- `int` against `bigint` arithmetic: the slice on one side only.
#guard reasonOf (check dep ∅ pairCert (pairT true (.Bin "+" ta tb))
    (pairP (.Bin "+" pa pb) bigint)) ==
  "the lowered routines differ beyond the renaming"
-- `@a % @b` is on the C1 path on both sides (a variable divisor), `@a % 2`
-- in the slice, `@a % -1` not.
#guard isC1 (checkPair true (.Bin "%" ta tb) (.Bin "%" pa pb))
#guard isC2a (checkPair true (.Bin "%" ta (num 2)) (.Bin "%" pa (num 2)))
#guard isC1 (checkPair true (.Bin "%" ta (.Num "-1")) (.Bin "%" pa (.Num "-1")))

/-! ### At the edge of the slice -/

-- Literals: `2147483648` is `numeric` in T-SQL and `bigint` in PostgreSQL.
#guard intLit? "2147483647" == some 2147483647
#guard intLit? "-2147483648" == some (-2147483648)
#guard intLit? "0" == some 0
#guard intLit? "2147483648" == none
#guard intLit? "-2147483649" == none
#guard intLit? "1_000" == none
#guard intLit? "007" == none
#guard intLit? "0x1f" == none
#guard intLit? "1e3" == none
#guard intLit? "--5" == none
#guard intLit? "" == none
#guard isC1 (checkPair false (.Num "2147483648") (.Num "2147483648"))
-- `AND` with an operand that can raise: evaluation order is unspecified.
#guard isC1 (check dep ∅ pairCert
  (tsqlProc ["r"] [inT "@a", inT "@b", ioT "@r"] [xactAbort,
    .If (.Bin "AND" (.Bin "<>" tb (num 0)) (.Bin ">" (.Bin "/" ta tb) (num 1)))
      [set (tv "@r") (num 1)] []])
  (pgProc ["r"] [inP "a", inP "b", ioP "r"] [] [
    .If (.Bin "AND" (.Bin "<>" pb (num 0)) (.Bin ">" (.Bin "/" pa pb) (num 1)))
      [set (pv "r") (num 1)] []]))
-- `SET XACT_ABORT ON` after a statement that can fail: not at the start.
#guard isC1 (check dep ∅ pairCert
  (tsqlProc ["r"] [inT "@a", inT "@b", ioT "@r"] [set (tv "@r") (.Bin "/" ta tb), xactAbort])
  (pairP (.Bin "/" pa pb)))
-- `SET XACT_ABORT OFF` is outside the spec.
#guard reasonOf (check dep ∅ pairCert
  (tsqlProc ["r"] [inT "@a", inT "@b", ioT "@r"] [.Setting "XACT_ABORT" false,
    set (tv "@r") ta]) (pairP pa)) == "the T-SQL routine is outside the C1 spec"
-- A `varchar(10)` parameter copied into a PostgreSQL `varchar(10)` parameter:
-- PostgreSQL parameters carry no length, so the copy is not known to fit
-- (C2b, `ExamplesC2b` has the `text` form, which is in the slice).
#guard isC1 (check dep ∅ pairCert (pairT false ta (.Ty "VARCHAR" ["10"]))
  (pairP pa (.Ty "VARCHAR" ["10"])))
-- `int(4)` is not the slice's type.
#guard !intType (.pg ∅ []) (.Ty "INT" ["4"])
-- `NULL` compared with `=`: outside the slice (session settings change it).
#guard isC1 (checkPair false (.Case none [.When2 (.Bin "=" ta .Null) (num 1)] (some (num 0)))
  (.Case none [.When2 (.Bin "=" pa .Null) (num 1)] (some (num 0))))
-- PostgreSQL `int4` and `INT`, T-SQL `INTEGER` and `INT`: the slice's integer.
#guard isC2a (check dep ∅ pairCert (pairT false (fn "ISNULL" [ta, tb]) (.Ty "INTEGER" []))
  (pairP (fn "COALESCE" [pa, pb]) (.Ty "INT4" [])))
#guard isC2a (check dep ∅ pairCert (pairT false (fn "ISNULL" [ta, tb]) (.Ty "INT" []))
  (pairP (fn "COALESCE" [pa, pb]) (.Ty "int" [])))

/-! ### Functions and the residual assumptions -/

def fnT (body : List Ast.Node) : Ast.Routine :=
  .Routine .Function ["f"] [inT "@a"] (some int) .Caller none (.Block [] body)
def fnP (body : List Ast.Node) : Ast.Routine :=
  .Routine .Function ["f"] [inP "a"] (some integer) .Caller none (.Block [] body)
def fnCert : Cert := ⟨[("@a", "a")]⟩

-- A T-SQL function cannot `SET XACT_ABORT ON`: in the slice only if
-- nothing can raise.
#guard isC2a (check dep ∅ fnCert (fnT [ret (some (fn "ISNULL" [ta, num 0]))])
  (fnP [ret (some (fn "COALESCE" [pa, num 0]))]))
#guard isC1 (check dep ∅ fnCert (fnT [ret (some (.Bin "+" ta (num 1)))])
  (fnP [ret (some (.Bin "+" pa (num 1)))]))
-- A function returning `bigint` is not in the slice.
#guard isC1 (check dep ∅ fnCert
  (.Routine .Function ["f"] [inT "@a"] (some bigint) .Caller none
    (.Block [] [ret (some (fn "ISNULL" [ta, num 0]))]))
  (.Routine .Function ["f"] [inP "a"] (some bigint) .Caller none
    (.Block [] [ret (some (fn "COALESCE" [pa, num 0]))])))

#guard (residualOf dep Γ (pairT true (.Bin "/" ta tb))) ==
  some ["ASM-TSQL-ARITH-ERRORS"]
#guard (residualOf dep Γ ifT) == some ["ASM-TSQL-ANSI-NULLS"]
#guard (residualOf dep Γ (pairT false (fn "ISNULL" [ta, tb]))) == some []

/-! ### The pair at the top

It is in the slice but for `XACT_ABORT`: `@x + 1` can overflow, and
T-SQL would go on after it. With `SET XACT_ABORT ON` first, it is a C2a
confirmation. -/

#guard isC1 (check dep Γ cert srcT mirrorP)
def srcAbort : Ast.Routine :=
  match srcT with
  | .Routine k n ps r s v (.Block ds body) => .Routine k n ps r s v (.Block ds (xactAbort :: body))
#guard isC2a (check dep Γ cert srcAbort mirrorP)
#guard (residualOf dep Γ srcAbort) == some ["ASM-TSQL-ARITH-ERRORS"]
-- The two status lines are still needed.
#guard !isConfirmed (check dep Γ cert srcAbort todayP)

end Sqldiff.Mirror.Examples
