/-
Checked examples for the C1 mirror check, on the generated abstract syntax
(`Sqldiff.Ast`): a T-SQL procedure and its PL/pgSQL mirror, confirmed;
what the converter emits today, and variants that must not be confirmed;
and runs of the core interpreter as an executable oracle. Every `#guard`
runs on every `lake build`.
-/
import Sqldiff.Mirror.Check

namespace Sqldiff.Mirror.Examples

open Sqldiff.Core (ExprSem WorldModel Obs CallEvent)
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
def Γ : Catalog := [("log_value", [.In, .InOut, .InOut])]
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

#guard isConfirmed (check dep [] loopCert loopT (loopP none [set (pv "y") (num 0)]))
-- Hoisting the value into the declaration runs it once: not the mirror.
#guard reasonOf (check dep [] loopCert loopT (loopP (some (num 0)) [])) ==
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
#guard reasonOf (check dep [] cert srcT mirrorP) == "the PL/pgSQL routine is outside the C1 spec"

/- `@I` and `@i` are one variable or two depending on the collation:
outside the spec. -/
#guard reasonOf (check dep [] ⟨[("@I", "a"), ("@i", "b"), (statusVar, "return_code")]⟩
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
#guard reasonOf (check dep [] ⟨[("@out", "out1"), ("@x", "X"), ("@y", "x"),
    (statusVar, "return_code")]⟩ twoT oneP) == "the PL/pgSQL routine is outside the C1 spec"
-- With `"X` quoted on both uses the names are distinct, and the pair is a mirror.
#guard isConfirmed (check dep [] ⟨[("@out", "out1"), ("@x", "\"X"), ("@y", "y"),
    (statusVar, "return_code")]⟩ twoT
  (pgProc ["r"] [.Param "out1" integer .InOut none, returnCode] [declP "\"X" none, declP "y" none]
    [set (pv "return_code") (num 0), set (pv "\"X") (num 1), set (pv "out1") (pv "y")]))

-- An undeclared PL/pgSQL name is an error in PostgreSQL, not a NULL.
#guard reasonOf (check dep [] ⟨[("@out", "out1"), ("@y", "y"), (statusVar, "return_code")]⟩
    (tsqlProc ["r"] [.Param "@out" int .InOut none] [declT "@y" none, set (tv "@out") (tv "@y")])
    (pgProc ["r"] [.Param "out1" integer .InOut none, returnCode] []
      [set (pv "return_code") (num 0), set (pv "out1") (pv "y")])) ==
  "the PL/pgSQL routine is outside the C1 spec"

-- `@@ROWCOUNT` is a system function, not a variable.
#guard reasonOf (check dep [] ⟨[("@x", "x"), (statusVar, "return_code")]⟩
    (tsqlProc ["r"] [.Param "@x" int .InOut none] [set (tv "@x") (tv "@@ROWCOUNT")])
    (pgProc ["r"] [.Param "x" integer .InOut none, returnCode] []
      [set (pv "return_code") (num 0), set (pv "x") (pv "row_count")])) ==
  "the T-SQL routine is outside the C1 spec"

/- `EXEC @rc = p @a`: the status is `p`'s last parameter, which C1 cannot
place without `p`'s arity, so this is outside C1. (Appending `@rc` after
the arguments would have confirmed `CALL p(a, rc)` passing `rc` into an
ordinary second parameter.) -/
#guard reasonOf (check { dep with routines := [("p2", "p2")] } [("p2", [.In, .InOut, .InOut])]
    ⟨[("@a", "a"), ("@rc", "rc"), (statusVar, "return_code")]⟩
    (tsqlProc ["r"] [.Param "@a" int .In none]
      [declT "@rc" none, .Call ["p2"] [arg (tv "@a") false] (some (tv "@rc"))])
    (pgProc ["r"] [.Param "a" integer .In none, returnCode] [declP "rc" none]
      [set (pv "return_code") (num 0), .Call ["p2"] [arg (pv "a") false, arg (pv "rc") false] none])) ==
  "the T-SQL routine is outside the C1 spec"

-- A PostgreSQL procedure cannot `RETURN` a value.
#guard reasonOf (check dep [] ⟨[(statusVar, "return_code")]⟩
    (tsqlProc ["r"] [] [ret (some (num 1))])
    (pgProc ["r"] [returnCode] [] [set (pv "return_code") (num 0), ret (some (num 1))])) ==
  "the PL/pgSQL routine is outside the C1 spec"

-- A nested PL/pgSQL block with its own `DECLARE` re-initialises on entry.
#guard reasonOf (check dep [] loopCert loopT
    (pgProc ["q"] [.Param "n" integer .InOut none, returnCode] [] [
      set (pv "return_code") (num 0),
      .While (.Bin "<" (pv "n") (num 3)) [
        .NBlock (.Block [declP "y" (some (num 0))] [
          set (pv "y") (.Bin "+" (pv "y") (num 1)),
          set (pv "n") (.Bin "+" (pv "n") (pv "y"))])]])) ==
  "the PL/pgSQL routine is outside the C1 spec"

-- A query is outside C1.
#guard reasonOf (check dep [] ⟨[(statusVar, "return_code")]⟩
    (tsqlProc ["r"] [] [.NStmt (.Delete ["t"] none [] none)])
    (pgProc ["r"] [returnCode] [] [set (pv "return_code") (num 0)])) ==
  "the T-SQL routine is outside the C1 spec"

/-! ## The core as an executable oracle

Values are `Option Int` (`none` is NULL); operators get their integer
meaning; `log_value` counts its calls in the world and adds its first
argument to its second. -/

def es : ExprSem (Option Int) where
  null := none
  lit n := some n
  op
    | "bin:>", [some a, some b] => some (if a > b then 1 else 0)
    | "bin:<", [some a, some b] => some (if a < b then 1 else 0)
    | "bin:+", [some a, some b] => some (a + b)
    | "fn:coalesce", [a, b] => a.orElse fun _ => b
    | _, _ => none
  truthy v := v == some 1

def wm : WorldModel (Option Int) Nat where
  call
    | "log_value", [v, b], w => (w + 1, [v, (fun x y => x + y) <$> v <*> b, some 0])
    | _, vs, w => (w, vs)

def run (r : Option Core.Routine) (args : List (Option Int)) (fuel : Nat) :
    Option (Obs (Option Int) Nat) :=
  r.map fun r => r.obs es wm args 0 fuel

def T := lowerT dep srcT
def P := lowerP Γ mirrorP

-- `@a = 5`: `@x` goes 2 to 10, `@b` becomes 7 + 10, the status is 10.
#guard run T [some 5, some 7] 100 ==
  some (.done [some 17, some 10] none 1 [⟨"log_value", [some 10, some 7]⟩])
#guard run P [some 5, some 7] 100 == run T [some 5, some 7] 100
-- `@b` omitted: its default 2.
#guard run T [some (-3)] 100 ==
  some (.done [some 12, some 10] none 1 [⟨"log_value", [some 10, some 2]⟩])
#guard run P [some (-3)] 100 == run T [some (-3)] 100
-- Too little fuel: both cut off at the same point.
#guard run T [some 5, some 7] 4 == run P [some 5, some 7] 4
#guard match run T [some 5, some 7] 4 with
  | some (.pending _ []) => true
  | _ => false

-- Today's output agrees here, because `v_x` is never NULL ...
#guard run (lowerP Γ todayP) [some 5, some 7] 100 == run T [some 5, some 7] 100

/- ... but not in general. `RETURN @x` with `@x` NULL returns status 0;
`return_code := v_x` gives NULL. And a procedure that ends without
`RETURN e` has status 0, while `return_code` keeps what the caller passed
(NULL by default). -/
def nullT : Ast.Routine := tsqlProc ["s"] [] [declT "@x" none, ret (some (tv "@x"))]
def nullP : Ast.Routine := pgProc ["s"] [returnCode] [declP "v_x" none]
  [set (pv "return_code") (pv "v_x"), ret none]
#guard run (lowerT dep nullT) [] 10 == some (.done [some 0] none 0 [])
#guard run (lowerP [] nullP) [] 10 == some (.done [none] none 0 [])

def endT : Ast.Routine := tsqlProc ["s"] [] []
def endP : Ast.Routine := pgProc ["s"] [returnCode] [] []
#guard run (lowerT dep endT) [] 10 == some (.done [some 0] none 0 [])
#guard run (lowerP [] endP) [] 10 == some (.done [none] none 0 [])

end Sqldiff.Mirror.Examples
