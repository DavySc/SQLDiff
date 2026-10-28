/-
Checked examples for the C1 mirror check: a T-SQL procedure and its
PL/pgSQL mirror, confirmed; what the converter emits today, and variants
that must not be confirmed; and runs of the core interpreter as an
executable oracle. Every `#guard` runs on every `lake build`.
-/
import Sqldiff.Mirror.Check

namespace Sqldiff.Mirror.Examples

open Sqldiff.Core (ExprSem WorldModel Obs CallEvent Routine)
open Sqldiff.Gate (Answer)
open Ast

def isConfirmed : Answer → Bool
  | .confirmed _ => true
  | .unconfirmed _ => false

def reasonOf : Answer → String
  | .confirmed _ => ""
  | .unconfirmed r => r

/-! ## Building blocks -/

def int : Ty := ⟨"int", []⟩
def integer : Ty := ⟨"integer", []⟩
/-- A T-SQL variable and a PL/pgSQL one. -/
def tv (x : String) : Expr := .var x
def pv (x : String) : Expr := .name [x]
def num (n : Nat) : Expr := .num (toString n)
def set (x v : Expr) : Node := .stmt (.assign x v)
def ret (e : Option Expr) : Node := .stmt (.return_ e)
def tsqlProc (name : List String) (ps : List Param) (body : List Node) : Ast.Routine :=
  { kind := .procedure, name, params := ps, returns := none, security := .caller,
    volatility := none, body := .mk [] body }
def pgProc (name : List String) (ps : List Param) (decls : List Stmt) (body : List Node) :
    Ast.Routine :=
  { kind := .procedure, name, params := ps, returns := none, security := .caller,
    volatility := none, body := .mk decls body }
def returnCode : Param := ⟨"return_code", integer, .inOut, some .null⟩

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

def srcT : Ast.Routine :=
  tsqlProc ["dbo", "p"] [⟨"@a", int, .in_, none⟩, ⟨"@b", int, .inOut, some (num 2)⟩] [
    .setting "NOCOUNT" true,
    .stmt (.declare "@x" (some int) (some (num 1))),
    .if_ (.bin ">" (tv "@a") (num 1)) [set (tv "@x") (num 2)]
      [.if_ (.bin "<" (tv "@a") (num 0)) [.block (.mk [] [set (tv "@x") (num 3)])]
        [set (tv "@x") (num 4)]],
    .while (.bin "<" (tv "@x") (num 10)) [set (tv "@x") (.bin "+" (tv "@x") (num 1))],
    .call ["dbo", "log_value"] [⟨none, tv "@x", false⟩, ⟨none, tv "@b", true⟩] none,
    ret (some (tv "@x"))]

/-- The PL/pgSQL body, with the two status lines as parameters. -/
def pgBody (first : List Node) (status : Expr) : List Node :=
  first ++ [
    set (pv "v_x") (num 1),
    .if_ (.bin ">" (pv "p_a") (num 1)) [set (pv "v_x") (num 2)]
      [.if_ (.bin "<" (pv "p_a") (num 0)) [set (pv "v_x") (num 3)] [set (pv "v_x") (num 4)]],
    .while (.bin "<" (pv "v_x") (num 10)) [set (pv "v_x") (.bin "+" (pv "v_x") (num 1))],
    .call ["log_value"] [⟨none, pv "v_x", false⟩, ⟨none, pv "p_b", false⟩] none,
    set (pv "return_code") status,
    ret none]

def pgParams : List Param :=
  [⟨"p_a", integer, .in_, none⟩, ⟨"p_b", integer, .inOut, some (num 2)⟩, returnCode]

def mirrorP : Ast.Routine :=
  pgProc ["p"] pgParams [.declare "v_x" (some integer) none]
    (pgBody [set (pv "return_code") (num 0)] (.call ["COALESCE"] [pv "v_x", num 0]))

/-- What the converter emits today. -/
def todayP : Ast.Routine :=
  pgProc ["p"] pgParams [.declare "v_x" (some integer) none] (pgBody [] (pv "v_x"))

def dep : Deployment := { rowCounts := false, routines := [("dbo.log_value", "log_value")] }
def Γ : Catalog := [("log_value", [.in_, .inOut, .inOut])]
def cert : Cert :=
  ⟨[("@a", "p_a"), ("@b", "p_b"), ("@x", "v_x"), (statusVar, "return_code")]⟩

/-! ## Confirmed -/

#guard isConfirmed (check dep Γ cert srcT mirrorP)
-- The certificate's entries may come in any order.
#guard isConfirmed (check dep Γ ⟨cert.vars.reverse⟩ srcT mirrorP)

/-- `DECLARE` with a value inside a loop assigns on every iteration: the
mirror declares at the top and assigns in the loop. -/
def loopT : Ast.Routine :=
  tsqlProc ["q"] [⟨"@n", int, .inOut, none⟩] [
    .while (.bin "<" (tv "@n") (num 3)) [
      .stmt (.declare "@y" (some int) (some (num 0))),
      set (tv "@y") (.bin "+" (tv "@y") (num 1)),
      set (tv "@n") (.bin "+" (tv "@n") (tv "@y"))]]
def loopP (decl : Option Expr) (inLoop : List Node) : Ast.Routine :=
  pgProc ["q"] [⟨"n", integer, .inOut, none⟩, returnCode] [.declare "y" (some integer) decl] [
    set (pv "return_code") (num 0),
    .while (.bin "<" (pv "n") (num 3)) (inLoop ++ [
      set (pv "y") (.bin "+" (pv "y") (num 1)),
      set (pv "n") (.bin "+" (pv "n") (pv "y"))])]
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
#guard reasonOf (check dep Γ cert srcT (pgProc ["p"] pgParams [.declare "v_x" (some integer) none]
    [set (pv "return_code") (num 0),
     set (pv "v_x") (num 1),
     .if_ (.bin ">" (pv "p_a") (num 1)) [set (pv "v_x") (num 2)]
       [.if_ (.bin "<" (pv "p_a") (num 0)) [set (pv "v_x") (num 4)] [set (pv "v_x") (num 3)]],
     .while (.bin "<" (pv "v_x") (num 10)) [set (pv "v_x") (.bin "+" (pv "v_x") (num 1))],
     .call ["log_value"] [⟨none, pv "v_x", false⟩, ⟨none, pv "p_b", false⟩] none,
     set (pv "return_code") (.call ["COALESCE"] [pv "v_x", num 0]),
     ret none])) == "the lowered routines differ beyond the renaming"

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
#guard reasonOf (check dep Γ cert
    { srcT with body := .mk [] [
        .setting "NOCOUNT" true,
        .stmt (.declare "@x" (some int) (some (num 1))),
        .if_ (.bin ">" (tv "@a") (num 1)) [set (tv "@x") (num 2)]
          [.if_ (.bin "<" (tv "@a") (num 0)) [set (tv "@x") (num 3)] [set (tv "@x") (num 4)]],
        .while (.bin "<" (tv "@x") (num 10)) [set (tv "@x") (.bin "+" (tv "@x") (num 1))],
        .call ["dbo", "log_value"] [⟨none, tv "@x", false⟩, ⟨none, tv "@b", false⟩] none,
        ret (some (tv "@x"))] }
    mirrorP) == "the lowered routines differ beyond the renaming"

-- T-SQL `OUTPUT` is `INOUT`; an `OUT` parameter starts NULL.
#guard !isConfirmed (check dep Γ cert srcT
  { mirrorP with params := [⟨"p_a", integer, .in_, none⟩, ⟨"p_b", integer, .out, none⟩, returnCode] })

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
    (tsqlProc ["r"] [⟨"@I", int, .inOut, none⟩] [set (tv "@i") (num 1)])
    (pgProc ["r"] [⟨"a", integer, .inOut, none⟩, returnCode] []
      [set (pv "return_code") (num 0), set (pv "b") (num 1)])) ==
  "the T-SQL routine is outside the C1 spec"

-- A PostgreSQL procedure cannot `RETURN` a value.
#guard reasonOf (check dep [] ⟨[(statusVar, "return_code")]⟩
    (tsqlProc ["r"] [] [ret (some (num 1))])
    (pgProc ["r"] [returnCode] [] [set (pv "return_code") (num 0), ret (some (num 1))])) ==
  "the PL/pgSQL routine is outside the C1 spec"

-- A nested PL/pgSQL block with its own `DECLARE` re-initialises on entry.
#guard reasonOf (check dep [] loopCert loopT
    (pgProc ["q"] [⟨"n", integer, .inOut, none⟩, returnCode] [] [
      set (pv "return_code") (num 0),
      .while (.bin "<" (pv "n") (num 3)) [
        .block (.mk [.declare "y" (some integer) (some (num 0))] [
          set (pv "y") (.bin "+" (pv "y") (num 1)),
          set (pv "n") (.bin "+" (pv "n") (pv "y"))])]])) ==
  "the PL/pgSQL routine is outside the C1 spec"

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
def nullT : Ast.Routine :=
  tsqlProc ["s"] [] [.stmt (.declare "@x" (some int) none), ret (some (tv "@x"))]
def nullP : Ast.Routine := pgProc ["s"] [returnCode] [.declare "v_x" (some integer) none]
  [set (pv "return_code") (pv "v_x"), ret none]
#guard run (lowerT dep nullT) [] 10 == some (.done [some 0] none 0 [])
#guard run (lowerP [] nullP) [] 10 == some (.done [none] none 0 [])

def endT : Ast.Routine := tsqlProc ["s"] [] []
def endP : Ast.Routine := pgProc ["s"] [returnCode] [] []
#guard run (lowerT dep endT) [] 10 == some (.done [some 0] none 0 [])
#guard run (lowerP [] endP) [] 10 == some (.done [none] none 0 [])

end Sqldiff.Mirror.Examples
