/-
Checked examples for the refusal diagnostics (`Sqldiff/Mirror/Diagnose.lean`,
checker 0.13.0): for routines the lowering refuses, the reason codes
`whyT` and `whyP` give, at least one per family (`routine`, `stmt`,
`node`, `expr`, `store`), and that a routine the lowering accepts gets
none. Every `#guard` runs on every `lake build`. The routines reuse the
builders of the other Examples files.
-/
import Sqldiff.Mirror.ExamplesC3b
import Sqldiff.Mirror.ExamplesC4

namespace Sqldiff.Mirror.ExamplesDiag

open Sqldiff.Mirror.Examples
open Sqldiff.Mirror.ExamplesC3a (acct Γd depd tc upd ins del nocount bumpT bumpP bumpCert)
open Sqldiff.Mirror.ExamplesC3b (sel asg als item selectT forBlock field keyT keyP existsT
  countStarE lookT lookP depq Γq)
open Sqldiff.Mirror.ExamplesC4 (tryT blockP throwT raiseP txn rollbackIfOpen insAcct bumpAllT
  rethrowT reraiseP)

/-- The T-SQL side of a pair, refused: its codes. -/
def wT (a : Ast.Routine) (d : Deployment := depd) (g : Catalog := Γd) : List String := whyT d g a
def wP (b : Ast.Routine) (g : Catalog := Γd) : List String := whyP g b

/-- A T-SQL procedure under `XACT_ABORT ON`, `NOCOUNT ON`, with `@id` in
and `@q` in-out. -/
def pT (body : List Ast.Node) : Ast.Routine :=
  tsqlProc ["dbo", "p"] [inT "@id", ioT "@q"] ([xactAbort, nocount] ++ body)
def pP (body : List Ast.Node) (decls : List Ast.Stmt := []) : Ast.Routine :=
  pgProc ["p"] [inP "p_id", ioP "p_q"] decls body
def vc (n : Nat) : Ast.Ty := .Ty "VARCHAR" [toString n]
def decimal : Ast.Ty := .Ty "DECIMAL" ["18", "2"]

/-! ## The oracle decides: a routine that lowers has no codes -/

#guard (lowerT dep Γ srcT).isSome && (lowerP Γ mirrorP).isSome
#guard why dep Γ srcT mirrorP == []
#guard why depd Γd bumpT bumpP == []
-- The check's reasons, and `why` names the T-SQL routine first.
#guard reasonOf (check depd Γd bumpCert (pT [.NStmt (.Merge ["dbo", "Acct"] none [] (.Table ["s"] none [])
    (num 1) [])]) bumpP) == reasonOutsideT
#guard reasonOf (check depd Γd bumpCert bumpT (pP [ret (some (num 1))])) == reasonOutsideP
#guard why depd Γd (pT [.NStmt (.Merge ["dbo", "Acct"] none [] (.Table ["s"] none []) (num 1) [])])
    (pP [ret (some (num 1))]) == ["stmt:merge"]
-- Every code is well-formed.
#guard (wT (pT [.Setting "ANSI NULLS (x)" false, .NStmt (.Assign (tv "@q") (.Cast (tv "@id") decimal))])).all
  codeOk

/-! ## routine -/

-- A `decimal(18,2)` parameter with a default no value of the slice fits.
#guard wT (tsqlProc ["dbo", "p"] [.Param "@d" decimal .In (some (num 1))] []) == ["routine:param:decimal_18_2"]
-- A default that T-SQL truncates: `@s varchar(3) = 'abcdef'`.
#guard wT (tsqlProc ["dbo", "p"] [.Param "@s" (vc 3) .In (some (.Str "abcdef"))] []) == ["routine:param:default"]
-- A T-SQL body block with declarations (the parser never writes one; the lowering refuses it).
#guard wT (.Routine .Procedure ["dbo", "p"] [] none .Caller none (.Block [declP "x" none] [])) ==
  ["routine:block-decls"]
-- A variable never declared, and one declared twice.
#guard wT (tsqlProc ["dbo", "p"] [] [set (tv "@y") (num 1)]) == ["routine:scope:undeclared"]
#guard wT (tsqlProc ["dbo", "p"] [] [declT "@y" none, declT "@y" none]) == ["routine:scope:duplicate"]
#guard wP (pgProc ["p"] [inP "a"] [declP "a" none] []) == ["routine:scope:duplicate"]
-- `@a` and `@A`: one variable or two depends on the collation.
#guard wT (tsqlProc ["dbo", "p"] [inT "@a", inT "@A"] []) == ["routine:names:case"]
#guard wT (tsqlProc ["dbo", "p"] [inT "@ä"] []) == ["routine:names:non-ascii"]
#guard wP (pgProc ["p"] [inP "\"Ä"] [] []) == ["routine:names:non-ascii"]
-- A function with a query: a procedure-only shape.
#guard wT (.Routine .Function ["dbo", "f"] [inT "@id"] (some int) .Caller none (.Block []
    [.If (existsT ["dbo", "Acct"] (some keyT)) [ret (some (num 1))] [], ret (some (num 0))])) ==
  ["routine:function"]

/-! ## stmt -/

-- A result set: `SELECT Qty FROM dbo.Acct WHERE Id = @id`.
#guard wT (pT [selectT [item (tc "Qty")] ["dbo", "Acct"] (some keyT)]) == ["stmt:select:result-set"]
-- A join, an alias, `TOP`, `ORDER BY`.
#guard wT (pT [.NStmt (.SSelect (.Select [] false [] false none [asg "@q" (tc "Qty")] [] false
    [.Join "inner" (.Table ["dbo", "Acct"] none []) (.Table ["dbo", "Pair"] none []) none] none [] none
    [] none none [] none))]) == ["stmt:select:from:join"]
#guard wT (pT [.NStmt (.SSelect (.Select [] false [] false (some (num 1)) [asg "@q" (tc "Qty")] []
    false [.Table ["dbo", "Acct"] (some "a") []] none [] none [.OrderItem (tc "Id") false none] none
    none [] none))]) == ["stmt:select:top", "stmt:select:from:alias", "stmt:select:order"]
-- `SELECT @q = SUM(Qty)`, `SELECT @q = COUNT(*)` with a `GROUP BY`.
#guard wT (pT [selectT [asg "@q" (fn "SUM" [tc "Qty"])] ["dbo", "Acct"] none]) == ["stmt:select:agg:sum"]
#guard wT (pT [.NStmt (.SSelect (.Select [] false [] false none [asg "@q" countStarE] [] false
    [.Table ["dbo", "Acct"] none []] none [tc "Id"] none [] none none [] none))]) == ["stmt:select:group"]
-- A `WHERE` that fixes no key; a running total; a pick into a string.
#guard wT (pT [selectT [asg "@q" (tc "Qty")] ["dbo", "Acct"] (some (.Bin "=" (tc "Qty") (tv "@id")))]) ==
  ["stmt:select:where:not-key"]
#guard wT (pT [selectT [asg "@q" (.Bin "+" (tv "@q") (tc "Qty"))] ["dbo", "Acct"] (some keyT)]) ==
  ["stmt:select:value"]
#guard wT (tsqlProc ["dbo", "p"] [inT "@id", ioT "@s" (vc 10)]
    [selectT [asg "@s" (tc "Qty")] ["dbo", "Acct"] (some keyT)]) ==
  ["store:pick:varchar_10", "stmt:select:target:varchar_10"]
-- An unknown table, and the PL/pgSQL `SELECT col INTO x`.
#guard wT (pT [selectT [asg "@q" (tc "Qty")] ["dbo", "Nope"] (some keyT)]) == ["stmt:select:table:unknown"]
#guard wP (pP [.NStmt (.SSelect (sel [item (pv "qty")] ["acct"] (some keyP) [pv "p_q"]))]) ==
  ["stmt:select:into"]
-- `INSERT ... SELECT`, `UPDATE ... FROM`, `DELETE` with an alias, `MERGE`.
#guard wT (pT [.NStmt (.Insert ["dbo", "Acct"] ["Id"] (.SrcQuery (sel [item (tc "Id")] ["dbo", "Pair"] none))
    [] [] none)]) == ["stmt:insert:select"]
#guard wT (pT [.NStmt (.Update ["dbo", "Acct"] none [.Set ["Qty"] (num 1)] [.Table ["dbo", "Pair"] none []]
    none)]) == ["stmt:update:from"]
#guard wT (pT [.NStmt (.Update ["dbo", "Acct"] none [.Set ["Id"] (num 1)] [] none)]) == ["stmt:update:key"]
#guard wT (pT [.NStmt (.Delete ["dbo", "Acct"] (some "a") [] none)]) == ["stmt:delete:alias"]
#guard wT (pT [.NStmt (.Merge ["dbo", "Acct"] none [] (.Table ["s"] none []) (num 1) [])]) == ["stmt:merge"]
-- A value the column cannot take: a string into `Qty int`; an unknown column.
#guard wT (pT [ins ["dbo", "Acct"] ["Id", "Qty"] [[tv "@id", .Str "x"]]]) == ["stmt:insert:value"]
#guard wT (pT [ins ["dbo", "Acct"] ["Id", "Qty"] [[tv "@id", .Cast (tv "@id") decimal]]]) ==
  ["stmt:insert:value", "expr:cast"]
#guard wT (pT [ins ["dbo", "Acct"] ["Id", "Nope"] [[tv "@id", num 1]]]) == ["stmt:insert:cols"]
-- A data statement in a function; one where row counts are observed.
#guard wT (.Routine .Function ["dbo", "f"] [inT "@id"] (some int) .Caller none (.Block []
    [del ["dbo", "Acct"] (some keyT), ret (some (num 0))])) == ["stmt:dml:function"]
#guard wT (tsqlProc ["dbo", "p"] [inT "@id"] [xactAbort, del ["dbo", "Acct"] (some keyT)])
    (d := { depd with rowCounts := true }) == ["stmt:dml:rowcounts"]
-- A PL/pgSQL procedure returning a value; a `DECLARE` in a PL/pgSQL body; a message.
#guard wP (pP [ret (some (num 1))]) == ["stmt:return:pg-proc"]
#guard wP (pP [.NStmt (declP "x" none)]) == ["stmt:declare:pg"]
#guard wT (pT [.NStmt (.Message (.Str "hi"))]) == ["stmt:message"]
#guard wT (pT [set (.Var "@@q") (num 1)]) == ["stmt:assign:target"]

/-! ## node -/

-- Settings other than the three the rules know; `XACT_ABORT OFF`.
#guard wT (pT [.Setting "ANSI_NULLS" false]) == ["node:setting:ansi_nulls"]
#guard wT (pT [.Setting "XACT_ABORT" false]) == ["node:setting:xact_abort"]
-- A callee the deployment does not map; a named argument; a call in a handled body.
#guard wT (pT [.Call ["dbo", "nope"] [] none]) == ["node:call:name"]
#guard wT (pT [.Call ["dbo", "log_value"] [.Arg (some "@x") (tv "@id") false] none]) (d := dep) (g := Γ) ==
  ["node:call:args"]
#guard wT (pT [.Call ["dbo", "log_value"] [] (some (tv "@q"))]) (d := dep) (g := Γ) == ["node:call:ret"]
#guard wT (pT [tryT [.Call ["dbo", "log_value"] [arg (tv "@id") false] none] [rethrowT]]) (d := dep) (g := Γ) ==
  ["node:call:handled"]
-- A `TRY` with two handlers (PL/pgSQL `WHEN division_by_zero`); a `THROW` outside a handler.
#guard wP (pP [blockP [set (pv "p_q") (num 1)] [reraiseP] ["division_by_zero"]]) == ["node:try:handlers"]
#guard wT (pT [rethrowT]) == ["node:throw:place"]
#guard wP (pP [reraiseP]) == ["node:throw:place"]
#guard wT (pT [.Throw [num 1, .Str "m", num 1]]) == ["node:throw:args"]
-- `RAISE NOTICE`; a format with `%`; another `ERRCODE`.
#guard wP (pP [raiseP "m" "P0002"]) == ["node:raise:opts"]
#guard wP (pP [.Raise (some "NOTICE") (some "m") [] []]) == ["node:raise:level"]
#guard wP (pP [.Raise none (some "m %") [pv "p_q"] []]) == ["node:raise:format"]
-- Transactions: without `XACT_ABORT ON`; a `COMMIT` without its `BEGIN`;
-- `ROLLBACK` outside the idiom; a savepoint; a name; the idiom inside an `IF`.
#guard wT (tsqlProc ["dbo", "p"] [inT "@id"] [txn "BEGIN", insAcct (tv "@id") (num 1), txn "COMMIT"]) ==
  ["node:txn:no-xact-abort"]
#guard wT (pT [txn "COMMIT"]) == ["node:txn:commit"]
#guard wT (pT [txn "BEGIN", txn "BEGIN", txn "COMMIT"]) == ["node:txn:nested"]
#guard wT (pT [txn "BEGIN", insAcct (tv "@id") (num 1)]) == ["node:txn:bracket"]
#guard wT (pT [txn "BEGIN", ret none, txn "COMMIT"]) == ["node:txn:bracket"]
#guard wT (pT [txn "BEGIN", txn "ROLLBACK"]) == ["node:txn:bracket", "node:txn:rollback"]
#guard wT (pT [txn "BEGIN", .Txn "SAVE" (some "sp"), txn "COMMIT"]) == ["node:txn:bracket", "node:txn:named"]
#guard wT (pT [.If (.Bin ">" (tv "@id") (num 0))
    [tryT [txn "BEGIN", insAcct (tv "@id") (num 1), txn "COMMIT"] [rollbackIfOpen, rethrowT]] []]) ==
  ["node:try:idiom", "node:txn:begin", "node:txn:commit"]
-- PL/pgSQL `COMMIT` in a `SECURITY DEFINER` procedure, and `ROLLBACK`.
#guard wP (.Routine .Procedure ["p"] [inP "p_id"] none .Owner none (.Block [] [txn "COMMIT"])) ==
  ["node:txn:commit"]
#guard wP (pP [txn "ROLLBACK"]) == ["node:txn:rollback"]
-- A `FOR` loop outside the converter's block; a block with other declarations.
#guard wP (pP [.ForQuery "r" (sel [als (pv "qty") "c"] ["acct"] (some keyP)) [field "p_q" "r" "c"]]) ==
  ["node:for-query"]
#guard wP (pP [.NBlock (.Block [declP "x" none] [set (pv "x") (num 1)])]) == ["node:block-decls"]
-- Inside `IF`, `WHILE` and a `TRY`, the inner node is named, not the parent.
#guard wT (pT [.If (.Bin ">" (tv "@id") (num 0)) [.While (.Bin ">" (tv "@id") (num 0))
    [tryT [.NBlock (.Block [] [.Setting "ARITHABORT" true])] [rethrowT]]] []]) == ["node:setting:arithabort"]

/-! ## expr -/

-- `@@ROWCOUNT`, `ERROR_NUMBER()`, a subquery in an expression, `EXISTS` in
-- a larger condition, `IN (SELECT ...)`, a qualified name, `*`, a window.
#guard wT (pT [set (tv "@q") (.Var "@@ROWCOUNT")]) == ["expr:var:global:rowcount"]
#guard wT (pT [set (tv "@q") (fn "ERROR_NUMBER" [])]) == ["expr:fn:error:error_number"]
#guard wP (pP [set (pv "p_q") (pv "SQLSTATE")]) == ["expr:fn:error:sqlstate"]
#guard wT (pT [set (tv "@q") (.Bin "+" (num 1) (.Subquery (sel [item (tc "Qty")] ["dbo", "Acct"] (some keyT))))]) ==
  ["expr:subquery"]
#guard wT (pT [.While (existsT ["dbo", "Acct"] (some keyT)) [set (tv "@q") (num 1)]]) == ["expr:exists"]
#guard wT (pT [.If (.Bin "AND" (existsT ["dbo", "Acct"] (some keyT)) (.Bin ">" (tv "@q") (num 0)))
    [set (tv "@q") (num 1)] []]) == ["expr:exists"]
#guard wT (pT [set (tv "@q") (.InQuery (tv "@id") (sel [item (tc "Id")] ["dbo", "Acct"] none) false)]) ==
  ["expr:in-query"]
#guard wT (pT [set (tv "@q") (.Name ["a", "Qty"])]) == ["expr:name:qualified"]
#guard wT (pT [set (tv "@q") (.Name ["Qty"])]) == ["expr:name:unresolved"]
#guard wT (pT [set (tv "@q") (.CallE (.CallF ["ROW_NUMBER"] [] false [] false true false))]) == ["expr:fn:over"]
#guard wT (pT [set (tv "@q") (.CallE (.CallF ["COUNT"] [tc "Qty"] true [] false false false))]) ==
  ["expr:fn:distinct"]
#guard wP (pP [set (pv "p_q") (.Var "@x")]) == ["expr:pg-var"]
-- The failing leaf inside a larger expression is named.
#guard wT (pT [set (tv "@q") (.Case none [.When2 (.IsNull (tv "@id") false) (.Bin "*" (num 2) (.Var "@@ERROR"))]
    (some (num 0)))]) == ["expr:var:global:error"]

/-! ## store -/

-- `SET @s = UPPER(@s)` with `@s varchar(20)`: a value outside the slice into a bounded string.
#guard wT (tsqlProc ["dbo", "p"] [ioT "@s" (vc 20)] [set (tv "@s") (fn "UPPER" [tv "@s"])]) ==
  ["store:assign:varchar_20", "expr:fn:upper"]
#guard wT (tsqlProc ["dbo", "p"] [ioT "@s" (vc 20)] [.NStmt (.Declare "@t" (some (vc 3)) (some (.Str "abcdef")))]) ==
  ["store:declare:varchar_3"]
#guard wT (.Routine .Function ["dbo", "f"] [inT "@s" (vc 5)] (some (vc 5)) .Caller none (.Block []
    [ret (some (.Bin "+" (tv "@s") (.Str "x")))])) == ["store:return:varchar_5"]
#guard wT (tsqlProc ["dbo", "p"] [ioT "@s" (vc 20)] [.Call ["dbo", "log_value"] [arg (tv "@s") true] none])
    (d := dep) (g := { Γ with routineTys := [("log_value", [vc 20, integer, integer])] }) ==
  ["store:call"]
-- (C2c re-review) Into an unbounded `text` parameter a T-SQL string
-- argument is outside the spec: the T-SQL callee's length is not known.
#guard wT (tsqlProc ["dbo", "p"] [ioT "@s" (vc 20)] [.Call ["dbo", "log_value"] [arg (tv "@s") true] none])
    (d := dep) (g := { Γ with routineTys := [("log_value", [.Ty "TEXT" [], integer, integer])] }) ==
  ["store:call", "node:call:type"]
-- (C2c) The same variable into an `integer` parameter: the types differ.
#guard wT (tsqlProc ["dbo", "p"] [ioT "@s" (vc 20)] [.Call ["dbo", "log_value"] [arg (tv "@s") true] none])
    (d := dep) (g := Γ) == ["store:call", "node:call:type"]
#guard wT (tsqlProc ["dbo", "p"] [inT "@id", ioT "@s" (vc 3)]
    [selectT [asg "@s" (.Str "abcd")] ["dbo", "Acct"] (some keyT)]) == ["store:pick:varchar_3"]
#guard wP (pgProc ["p"] [inP "p_id"] [.Declare "v_s" (some (vc 3)) none]
    [forBlock [als (.Str "abcd") "c1"] ["acct"] (some keyP) [field "v_s" "sqldiff_r" "c1"]]) ==
  ["store:for:varchar_3"]
-- The declared type is named as `tyName` spells it, sanitised.
#guard wP (pgProc ["p"] [inP "p_id"] [.Declare "v_s" (some (vc 20)) none]
    [set (pv "v_s") (fn "upper" [pv "v_s"])]) == ["store:assign:varchar_20", "expr:fn:upper"]
-- A local of a type PostgreSQL does not have: the variable itself is outside
-- the slice, named by its type (C2c, `type:<type>`).
#guard wP (pgProc ["p"] [inP "p_id"] [.Declare "v_s" (some (.Ty "NVARCHAR" ["MAX"])) none]
    [set (pv "v_s") (fn "upper" [pv "v_s"])]) == ["store:assign:nvarchar_max", "type:nvarchar_max"]
#guard wT (tsqlProc ["dbo", "p"] [ioT "@s" (vc 20)] [.NStmt (.Declare "@t" none (some (tv "@s")))]) ==
  ["store:declare:untyped"]

/-! ## Deduplication and the cap -/

#guard wT (pT [.Setting "ANSI_NULLS" false, .Setting "ANSI_NULLS" false, .Setting "ansi_nulls" true]) ==
  ["node:setting:ansi_nulls"]
#guard (wT (pT ((List.range 30).map fun i => .Setting s!"OPT{i}" true))).length == whyCap
#guard sanitise "decimal(18,2)" == "decimal_18_2"
#guard sanitise "int()" == "int"
#guard sanitise "  Ä@@x  " == "x"
#guard sanitise "()" == "_"
#guard codeOk "stmt:select:agg:sum" && !codeOk "stmt" && !codeOk "Stmt:x" && !codeOk "a::b"

end Sqldiff.Mirror.ExamplesDiag
