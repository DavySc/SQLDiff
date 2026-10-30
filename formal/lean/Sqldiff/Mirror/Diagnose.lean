/-
Why a routine is outside the spec: untrusted reason codes for a `lowerT`
or `lowerP` that answers `none` (checker 0.13.0).

The lowering (`Sqldiff/Mirror/Lower.lean`) is partial and says only
"outside the spec", so nobody can tell which constructs keep a corpus out.
`whyT` and `whyP` name them: stable codes `family:detail` (ASCII, lower
case, `[a-z0-9:_/.-]`; a user or type name in a detail is sanitised to
that set by `sanitise`), deduplicated, in the order the constructs stand,
at most `whyCap`. The families: `routine` (the routine as a whole, its
parameters, its names), `stmt` (a statement, with the shape of a `SELECT`,
`INSERT`, `UPDATE`, `DELETE`), `node` (a control node, a call, a setting,
a handler, a raised error, a transaction statement), `expr` (an
expression form), `store` (a string store that could change its value,
"String stores, on every path"). The table of codes is in
`formal/lean/README.md`, "Refusal diagnostics".

**Nothing here is trusted.** The lowering is the oracle: every function
below is called only on a node the lowering refuses (the callers test
`lowerParam`, `lowerNode`, `lowerStmt`, `lowerExpr`, `typed`, `storesStmt`,
`txnBracket`, ... first), recurses into the children the lowering refuses
(a failing inner node is named; the parent is named only when every child
lowers), and classifies by the shape of the abstract syntax. A code that
is wrong or missing changes no answer: the answer is `unconfirmed` either
way (`Sqldiff.Mirror.check` is untouched, and so are the theorems). Where
no code of the table fits, the lower-cased constructor name is the detail
(`stmt:sselect`, `node:if`, `expr:cast`), so nothing is silent; a routine
the lowering refuses without any stage naming a reason gets
`routine:unclassified`.
-/
import Sqldiff.Mirror.Lower

namespace Sqldiff.Mirror

open Sqldiff.Core (Var Stmt seqList)

/-- The largest number of codes an answer carries. -/
def whyCap : Nat := 24

/-- The last-resort code. -/
def unclassified : String := "routine:unclassified"

/-! ## Codes -/

/-- Collapse runs of `_`. -/
def squeezeUnderscores : List Char → List Char
  | [] => []
  | c :: rest => match squeezeUnderscores rest with
    | '_' :: r => if c == '_' then '_' :: r else c :: '_' :: r
    | r => c :: r

/-- A detail from a user or type name: ASCII letters lower-cased, digits,
`_`, `.` and `-` kept, every other character (`(`, `)`, `,`, a space, a
quote, anything non-ASCII) replaced by `_`, runs of `_` collapsed, and
leading and trailing `_` dropped; `_` when nothing is left. So `tyName`'s
`decimal(18,2)` is `decimal_18_2`, `int()` is `int`, `nvarchar(max)` is
`nvarchar_max`, and `@@ROWCOUNT` without its `@@` is `rowcount`. -/
def sanitise (s : String) : String :=
  let keep (c : Char) : Bool :=
    c.toNat < 128 && (c.isAlphanum || c == '_' || c == '.' || c == '-')
  let cs := s.toList.map fun c => let c := c.toLower; if keep c then c else '_'
  let cs := squeezeUnderscores cs
  let cs := ((cs.dropWhile (· == '_')).reverse.dropWhile (· == '_')).reverse
  if cs.isEmpty then "_" else String.ofList cs

/-- A code is well-formed: `family:detail` over the allowed characters. -/
def codeOk (c : String) : Bool :=
  !c.isEmpty && c.toList.all (fun ch =>
      ch.toNat < 128 && (ch.isLower || ch.isDigit || ch == ':' || ch == '_' || ch == '/' ||
        ch == '.' || ch == '-')) &&
    (c.splitOn ":").length ≥ 2 && (c.splitOn ":").all (!·.isEmpty)

/-- A declared type as a detail; `untyped` for none. -/
def tyDetail : Option Ast.Ty → String
  | some t => sanitise (tyName t)
  | none => "untyped"

/-- The declared type of a variable as a detail. -/
def varDetail (env : TyEnv) (x : Var) : String := tyDetail (lookup env x)

/-- The first occurrence of each code, in order. -/
def dedup : List String → List String
  | [] => []
  | x :: xs => x :: (dedup xs).filter (· != x)

/-- `xs`, or the fallback when there is nothing. -/
def orElse (xs : List String) (fallback : String) : List String :=
  if xs.isEmpty then [fallback] else xs

/-- The answer's list: deduplicated, never empty, at most `whyCap`. -/
def finish (xs : List String) : List String :=
  (orElse (dedup (xs.filter codeOk)) unclassified).take whyCap

/-! ## Expressions

`whyExpr d sc ok e` names why `e` fails the oracle `ok` (`lowerExpr`, or
`typed` where the lowering needs a value of the slice): a form the rules
refuse on its own (`ownReason`), else the failing sub-expressions, else
the form itself. -/

/-- The direct sub-expressions. -/
def subExprs : Ast.Expr → List Ast.Expr
  | .CallE (.CallF _ args _ order _ _ _) => args ++ order.map fun | .OrderItem e _ _ => e
  | .Bin _ a b => [a, b]
  | .Unary _ a => [a]
  | .IsNull x _ => [x]
  | .InList x xs _ => x :: xs
  | .InQuery x _ _ => [x]
  | .Between x lo hi _ => [x, lo, hi]
  | .Like x p _ _ => [x, p]
  | .Cast x _ => [x]
  | .Case o ws e => o.toList ++ (ws.flatMap fun | .When2 c r => [c, r]) ++ e.toList
  | .Tuple xs => xs
  | .Extract _ x => [x]
  | _ => []

/-- The constructor's own code, when nothing more precise applies. -/
def exprFallback : Ast.Expr → String
  | .Name _ => "expr:name"
  | .Var _ => "expr:var"
  | .Num _ => "expr:num"
  | .Str _ => "expr:str"
  | .Null => "expr:null"
  | .Star _ => "expr:star"
  -- A function outside the slice: its name.
  | .CallE (.CallF n _ _ _ _ _ _) => "expr:fn:" ++ sanitise (dotted n)
  | .Bin .. => "expr:bin"
  | .Unary .. => "expr:unary"
  | .IsNull .. => "expr:isnull"
  | .InList .. => "expr:inlist"
  | .InQuery .. => "expr:in-query"
  | .Between .. => "expr:between"
  | .Like .. => "expr:like"
  | .Cast .. => "expr:cast"
  | .Case .. => "expr:case"
  | .Exists _ => "expr:exists"
  | .Subquery _ => "expr:subquery"
  | .Tuple _ => "expr:tuple"
  | .Special _ => "expr:special"
  | .Extract .. => "expr:extract"

/-- (C2c) A variable read where the lowering needs a slice value, declared
with a type the slice has no values for (`datetime2(7)`, `datetime`,
`time`, `decimal(18,2)`, ...): `type:<type>`. -/
def typeCode (d : Dialect) (env : TyEnv) (x : Var) : Option String :=
  match lookup env x with
  | some ty => if (valTypeOf d ty).isNone then some ("type:" ++ tyDetail (some ty)) else none
  | none => none

/-- A form the rules refuse whatever its sub-expressions. -/
def ownReason (d : Dialect) (env : TyEnv) (sc : Scope) : Ast.Expr → Option String
  | .Var v => match d with
    | .tsql _ _ =>
      if v.startsWith "@@" then some ("expr:var:global:" ++ sanitise (String.ofList (v.toList.drop 2)))
      else typeCode d env v
    | .pg _ _ => some "expr:pg-var"
  | .Name [x] => match d with
    | .pg _ _ =>
      let n := pgName x
      if n == "sqlstate" || n == "sqlerrm" then some ("expr:fn:error:" ++ n)
      else match resolveName d sc x with
        | none => some "expr:name:unresolved"
        | some (.inr v) => typeCode d env v
        | some (.inl _) => none
    | .tsql _ _ => if (resolveName d sc x).isNone then some "expr:name:unresolved" else none
  | .Name _ => some "expr:name:qualified"
  | .Star _ => some "expr:star"
  | .InQuery .. => some "expr:in-query"
  | .Exists _ => some "expr:exists"
  | .Subquery _ => some "expr:subquery"
  -- C2c: DATEADD and DATEDIFF with a datepart outside the slice.
  | .CallE (.CallF [n] [part, _, _] false [] false false false) =>
    if (d matches .tsql _ _) && n.toUpper == "DATEADD" && (datePart? part).isNone then
      some "expr:dateadd:part"
    else if (d matches .tsql _ _) && n.toUpper == "DATEDIFF" && datePart? part != some .day then
      some "expr:datediff:part"
    else none
  | .CallE (.CallF n _ distinct order withinGroup over star) =>
    if (d matches .tsql _ _) && errorFns.contains (dotted n).toUpper then
      some ("expr:fn:error:" ++ sanitise (dotted n))
    else if distinct then some "expr:fn:distinct"
    else if !order.isEmpty then some "expr:fn:order"
    else if withinGroup then some "expr:fn:within-group"
    else if over then some "expr:fn:over"
    else if star then some "expr:fn:star"
    else none
  | _ => none

partial def whyExpr (d : Dialect) (env : TyEnv) (sc : Scope) (ok : Ast.Expr → Bool)
    (e : Ast.Expr) : List String :=
  match ownReason d env sc e with
  | some r => [r]
  | none =>
    -- C2c: DATEADD and DATEDIFF need their date (and number) arguments in
    -- the slice; the datepart is a name no scope resolves.
    let typedOk (k : Ast.Expr) : Bool := (typed d env sc k).isSome
    let inner := match d, e with
      | .tsql _ _, .CallE (.CallF [n] [_, a, b] false [] false false false) =>
        if n.toUpper == "DATEADD" || n.toUpper == "DATEDIFF" then
          ([a, b].filter fun k => !typedOk k).flatMap (whyExpr d env sc typedOk)
        else ((subExprs e).filter fun k => !ok k).flatMap (whyExpr d env sc ok)
      -- The converter's `make_interval` form: the date and the one number.
      | .pg _ _, .Bin "+" dt (.CallE (.CallF [f] [y, mo, _, dd, _, _, _] false [] false false false)) =>
        if f.toLower == "make_interval" then
          ([dt, y, mo, dd].filter fun k => !typedOk k).flatMap (whyExpr d env sc typedOk)
        else ((subExprs e).filter fun k => !ok k).flatMap (whyExpr d env sc ok)
      | _, _ => ((subExprs e).filter fun k => !ok k).flatMap (whyExpr d env sc ok)
    orElse inner (exprFallback e)

/-- Why `lowerExpr` refuses `e`. -/
def whyLower (d : Dialect) (env : TyEnv) (sc : Scope) (e : Ast.Expr) : List String :=
  whyExpr d env sc (fun k => (lowerExpr d env sc k).isSome) e

/-- Why `typed` refuses `e` (a value the rules need in the slice). -/
def whyTyped (d : Dialect) (env : TyEnv) (sc : Scope) (e : Ast.Expr) : List String :=
  whyExpr d env sc (fun k => (typed d env sc k).isSome) e

/-- (C2c) Why a value in a position of type `want` is refused: a string
literal where a date or timestamp is read but not in the ISO form, or
with more fractional digits than kept (`expr:date-literal`); a literal
in a bit position other than `0`/`1` (T-SQL) or `TRUE`/`FALSE`
(`expr:bit-literal`); else why `typed` refuses it. -/
def whyValue (d : Dialect) (env : TyEnv) (sc : Scope) (want : Option STy) (e : Ast.Expr) :
    List String :=
  match want, e with
  | some .date, .Str _ | some (.ts _), .Str _ | some .date, .Cast (.Str _) _
  | some (.ts _), .Cast (.Str _) _ => ["expr:date-literal"]
  | some .bit, .Num _ | some .bit, .Str _ | some .bit, .Special _ => ["expr:bit-literal"]
  | _, _ => if (typed d env sc e).isSome then [] else whyTyped d env sc e

/-- The codes of `e` when `lowerExpr` refuses it, nothing otherwise. -/
def ifLowerFails (d : Dialect) (env : TyEnv) (sc : Scope) (e : Ast.Expr) : List String :=
  if (lowerExpr d env sc e).isSome then [] else whyLower d env sc e

/-! ## Queries -/

/-- The shape of a `SELECT` that `selParts` refuses. -/
def whyFromOne : Ast.From → List String
  | .Table _ alias hints =>
    (if alias.isSome then ["stmt:select:from:alias"] else []) ++
      (if hints.isEmpty then [] else ["stmt:select:from:hints"])
  | .Sub .. => ["stmt:select:from:sub"]
  | .Values .. => ["stmt:select:from:values"]
  | .Func .. => ["stmt:select:from:func"]
  | .DataChange .. => ["stmt:select:from:data-change"]
  | .Join .. => ["stmt:select:from:join"]

def whyFrom : List Ast.From → List String
  | [] => ["stmt:select:from:none"]
  | [f] => whyFromOne f
  | fs => "stmt:select:from:multi" :: fs.flatMap whyFromOne

def whySelShape : Ast.Select → List String
  | .Select ctes forJson setOps distinct top _ _ _ froms _ group having order offset limit locking
      values =>
    (if ctes.isEmpty then [] else ["stmt:select:cte"]) ++
      (if forJson then ["stmt:select:for-json"] else []) ++
      (if setOps.isEmpty then [] else ["stmt:select:setop"]) ++
      (if distinct then ["stmt:select:distinct"] else []) ++
      (if top.isSome then ["stmt:select:top"] else []) ++
      (if values.isSome then ["stmt:select:from:values"] else whyFrom froms) ++
      (if group.isEmpty then [] else ["stmt:select:group"]) ++
      (if having.isSome then ["stmt:select:having"] else []) ++
      (if order.isEmpty then [] else ["stmt:select:order"]) ++
      (if offset.isSome || limit.isSome then ["stmt:select:offset"] else []) ++
      (if locking.isEmpty then [] else ["stmt:select:locking"])

/-- Aggregate functions, whose call outside the C3b `COUNT(*)` shape is
`stmt:select:agg:<fn>`. -/
def aggNames : List String :=
  ["COUNT", "COUNT_BIG", "SUM", "MIN", "MAX", "AVG", "STRING_AGG", "STDEV", "STDEVP", "VAR", "VARP",
   "CHECKSUM_AGG", "GROUPING", "APPROX_COUNT_DISTINCT", "ARRAY_AGG", "BOOL_AND", "BOOL_OR", "EVERY",
   "JSON_AGG", "JSONB_AGG", "BIT_AND", "BIT_OR"]

def aggOf : Ast.Expr → Option String
  | .CallE (.CallF [n] _ _ _ _ _ star) =>
    if star || aggNames.contains n.toUpper then some (sanitise n) else none
  | _ => none

/-- Why `tableOf` refuses the table a statement names: not in the
deployment's map or the catalog, or in the catalog but not a plain table
(`CatTable.wellFormed`). `pre` is the statement's family. -/
def whyTable (d : Dialect) (pre : String) (tgt : List String) : List String :=
  let (Γ, key) : Catalog × Option String := match d with
    | .tsql dep Γ => (Γ, lookup dep.tables (dotted tgt))
    | .pg Γ _ => (Γ, some (dotted (tgt.map pgName)))
  match key.bind fun n => Γ.tables.find? (·.name == n) with
  | some t => if t.wellFormed then [] else [pre ++ ":table:not-plain"]
  | none => [pre ++ ":table:unknown"]

/-- Why `queryOn` refuses a query: its table, or its `WHERE`. -/
def whyQueryOn (d : Dialect) (env : TyEnv) (pre : String) (tgt : List String)
    (wh : Option Ast.Expr) : List String :=
  match tableOf d tgt with
  | none => whyTable d pre tgt
  | some t => match wh with
    | some w => ifLowerFails d env (scopeOf d t tgt) w
    | none => []

/-- Why `pickValue` refuses a select item: an aggregate, a value outside
the slice (by its form), or one that reads a variable, can raise or is a
condition. -/
def whyPick (d : Dialect) (env : TyEnv) (sc : Scope) (e : Ast.Expr) : List String :=
  match aggOf e with
  | some f => ["stmt:select:agg:" ++ f]
  | none => if (typed d env sc e).isNone then whyTyped d env sc e else ["stmt:select:value"]

/-- Why `rowPick` refuses the targets and the condition: a target of the
wrong type, a `WHERE` that fixes no key. -/
def whyRowPick (d : Dialect) (env : TyEnv) (t : CatTable) (c : Option Core.Expr)
    (sets : List (Var × Core.Expr × STy)) : List String :=
  (sets.filterMap fun (x, _, ty) =>
    if pickTarget d env x ty then none else some ("stmt:select:target:" ++ varDetail env x)) ++
    (if keyFixed t c then [] else ["stmt:select:where:not-key"])

/-- Why `countPick` refuses its target. -/
def whyCount (d : Dialect) (env : TyEnv) (x : Var) : List String :=
  if intVarW d env x == some .i32 then [] else ["stmt:select:target:" ++ varDetail env x]

/-- Why `lowerSelectT` refuses T-SQL `SELECT ...`. -/
def whySelectT (d : Dialect) (env : TyEnv) (sel : Ast.Select) : List String :=
  match selParts sel with
  | none => whySelShape sel
  | some (items, into, strict, tgt, wh) =>
    if !into.isEmpty || strict then ["stmt:select:into"] else
    match queryOn d env tgt wh with
    | none => whyQueryOn d env "stmt:select" tgt wh
    | some (t, sc, c) =>
      let row : List String :=
        let bad := items.flatMap fun
          | .SelItem e none (some v) =>
            (if (target d (.Var v)).isNone then ["stmt:assign:target"] else []) ++
              (if (pickValue d env sc e).isNone then whyPick d env sc e else [])
          | .SelItem _ _ none => ["stmt:select:result-set"]
          | .SelItem _ (some _) (some _) => ["stmt:select:item"]
        if !bad.isEmpty then bad else
        match items.mapM (fun
            | .SelItem e none (some v) => do pure (← target d (.Var v), ← pickValue d env sc e)
            | _ => none) with
        | some sets => whyRowPick d env t c sets
        | none => []
      match items with
      | [.SelItem e none (some v)] =>
        if countStar e then
          match target d (.Var v) with
          | some x => whyCount d env x
          | none => ["stmt:assign:target"]
        else row
      | _ => row

/-- Why `lowerSelectP` refuses PL/pgSQL `SELECT ...`: no `INTO` (a result
set), `STRICT`, or an `INTO` other than `COUNT(*)` into one variable
(`SELECT col INTO x` is NULL on no row). -/
def whySelectP (d : Dialect) (env : TyEnv) (sel : Ast.Select) : List String :=
  match selParts sel with
  | none => whySelShape sel
  | some (items, into, strict, tgt, wh) =>
    if into.isEmpty then ["stmt:select:result-set"]
    else if strict then ["stmt:select:into"]
    else match items, into with
      | [.SelItem e none none], [x] =>
        if countStar e then
          match target d x with
          | none => ["stmt:assign:target"]
          | some x => match queryOn d env tgt wh with
            | none => whyQueryOn d env "stmt:select" tgt wh
            | some _ => whyCount d env x
        else ["stmt:select:into"]
      | _, _ => ["stmt:select:into"]

/-- Why `lowerScalar` refuses `x := (SELECT ...)`. -/
def whyScalar (d : Dialect) (env : TyEnv) (x : Var) (sel : Ast.Select) : List String :=
  match selParts sel with
  | none => whySelShape sel
  | some (items, into, strict, tgt, wh) =>
    if !into.isEmpty || strict then ["stmt:select:into"] else
    match queryOn d env tgt wh with
    | none => whyQueryOn d env "stmt:select" tgt wh
    | some (t, sc, c) => match items with
      | [.SelItem e none none] =>
        if countStar e then whyCount d env x
        else match pickValue d env sc e with
          | none => whyPick d env sc e
          | some v => whyRowPick d env t c [(x, v)]
      | [_] => ["stmt:select:item"]
      | _ => ["stmt:select:items"]

/-- Why `lowerExists` refuses `EXISTS (SELECT ...)`. -/
def whyExists (d : Dialect) (env : TyEnv) (sel : Ast.Select) : List String :=
  match selParts sel with
  | none => whySelShape sel
  | some (items, into, strict, tgt, wh) =>
    if !into.isEmpty || strict then ["stmt:select:into"]
    else if !existsItems items then ["stmt:select:items"]
    else whyQueryOn d env "stmt:select" tgt wh

/-- Why `lowerForBlock` refuses the converter's `FOR` block. -/
def whyFor (d : Dialect) (env : TyEnv) (k : Ast.RoutineKind) (r : String) (ty : Ast.Ty)
    (r' : String) (sel : Ast.Select) (body : List Ast.Node) : List String :=
  match d with
  | .tsql _ _ => ["node:for-query"]
  | .pg Γ vars =>
    let r := pgName r
    if !(k matches .Procedure) then ["routine:function"]
    else if !recordType ty || pgName r' != r then ["node:for-query"]
    else if vars.contains r then ["routine:scope:duplicate"]
    else
    let d' := Dialect.pg Γ (vars ++ [r])
    match selParts sel with
    | none => whySelShape sel
    | some (items, into, strict, tgt, wh) =>
      if !into.isEmpty || strict then ["stmt:select:into"] else
      match queryOn d' env tgt wh with
      | none => whyQueryOn d' env "stmt:select" tgt wh
      | some (t, sc, c) =>
        let bad := items.flatMap fun
          | .SelItem e (some _) none =>
            if (pickValue d' env sc e).isNone then whyPick d' env sc e else []
          | _ => ["stmt:select:item"]
        if !bad.isEmpty then bad else
        match items.mapM (fun
            | .SelItem e (some a) none => do pure (pgName a, ← pickValue d' env sc e)
            | _ => none), body.mapM (fieldAssign r) with
        | some cols, some asg =>
          if asg.length != cols.length || !distinct (cols.map (·.1)) ||
              !(asg.zip cols).all (fun (p, q) => p.2 == q.1) || (asg.map (·.1)).contains r then
            ["node:for-query"]
          else whyRowPick d' env t c ((asg.zip cols).map fun (p, q) => (p.1, q.2))
        | _, _ => ["node:for-query"]

/-! ## Statements -/

/-- Why `colValue` refuses a value written to column `c`. -/
def whyColValue (d : Dialect) (env : TyEnv) (sc : Scope) (code : String) (c : CatCol)
    (e : Ast.Expr) : List String :=
  let want := kindTy d c.kind
  match typedTo d env sc (some want) e with
  | none => code :: whyValue d env sc (some want) e
  | some v => if (colFits d c v).isSome then [] else [code]

/-- Why `dataOk` refuses a data statement here. -/
def whyDataOk (d : Dialect) (k : Ast.RoutineKind) : List String :=
  (if k matches .Procedure then [] else ["stmt:dml:function"]) ++
    match d with
    | .tsql dep _ => if dep.rowCounts then ["stmt:dml:rowcounts"] else []
    | .pg _ _ => []

def stmtFallback : Ast.Stmt → String
  | .SSelect _ => "stmt:sselect"
  | .Insert .. => "stmt:insert"
  | .Update .. => "stmt:update"
  | .Delete .. => "stmt:delete"
  | .Merge .. => "stmt:merge"
  | .Assign .. => "stmt:assign"
  | .Return _ => "stmt:return"
  | .Declare .. => "stmt:declare"
  | .Message _ => "stmt:message"

/-- Why `lowerStmt` refuses a statement. -/
def whyStmt (d : Dialect) (env : TyEnv) (k : Ast.RoutineKind) (s : Ast.Stmt) : List String :=
  let codes : List String := match s with
    | .Assign t (.Subquery q) =>
      (if k matches .Procedure then [] else ["routine:function"]) ++
        match target d t with
        | none => ["stmt:assign:target"]
        | some x => if (lowerScalar d env x q).isSome then [] else whyScalar d env x q
    | .SSelect q =>
      (if k matches .Procedure then [] else ["routine:function"]) ++
        match d with
        | .tsql _ _ => if (lowerSelectT d env q).isSome then [] else whySelectT d env q
        | .pg _ _ => if (lowerSelectP d env q).isSome then [] else whySelectP d env q
    | .Assign t v =>
      match target d t with
      | none => "stmt:assign:target" :: ifLowerFails d env {} v
      | some x =>
        if (lowerValue d env {} (wantOf d env x) v).isSome then [] else whyLower d env {} v
    | .Return none => []
    | .Return (some e) =>
      ifLowerFails d env {} e ++
        match d, k with
        | .pg _ _, .Procedure => ["stmt:return:pg-proc"]
        | _, _ => []
    | .Declare _ ty dflt => match d, dflt with
      | .pg _ _, _ => ["stmt:declare:pg"]
      | .tsql _ _, some e =>
        if (lowerValue d env {} (ty.bind (valTypeOf d)) e).isSome then [] else whyLower d env {} e
      | .tsql _ _, none => []
    | .Insert tgt cols src returning retInto conflict =>
      (match src with
        | .SrcQuery _ => ["stmt:insert:select"]
        | .SrcDefault => ["stmt:insert:default"]
        | .SrcValues _ => []) ++
        (if returning.isEmpty && retInto.isEmpty then [] else ["stmt:insert:returning"]) ++
        (if conflict.isSome then ["stmt:insert:conflict"] else []) ++
        whyDataOk d k ++
        match tableOf d tgt with
        | none => whyTable d "stmt:insert" tgt
        | some t =>
          let ccs := cols.filterMap (colOf d t)
          if cols.isEmpty || ccs.length != cols.length || !distinct (ccs.map (·.name)) then
            ["stmt:insert:cols"]
          else match src with
            | .SrcValues rows =>
              (if rows.isEmpty || !rows.all (·.length == ccs.length) then ["stmt:insert:cols"]
                else []) ++
                rows.flatMap fun r => (ccs.zip r).flatMap fun (c, e) =>
                  whyColValue d env {} "stmt:insert:value" c e
            | _ => []
    | .Update tgt alias sets froms wh =>
      (if froms.isEmpty then [] else ["stmt:update:from"]) ++
        (if alias.isSome then ["stmt:update:alias"] else []) ++
        whyDataOk d k ++
        match tableOf d tgt with
        | none => whyTable d "stmt:update" tgt
        | some t =>
          let sc := scopeOf d t tgt
          (if sets.isEmpty then ["stmt:update:cols"] else []) ++
            (sets.flatMap fun
              | .Set [c] e => match colOf d t c with
                | none => ["stmt:update:cols"]
                | some c' =>
                  if t.isKey c'.name then ["stmt:update:key"]
                  else whyColValue d env sc "stmt:update:value" c' e
              | .Set _ _ => ["stmt:update:multi-set"]) ++
            (match wh with
              | some w => ifLowerFails d env sc w
              | none => [])
    | .Delete tgt alias froms wh =>
      (if froms.isEmpty then [] else ["stmt:delete:from"]) ++
        (if alias.isSome then ["stmt:delete:alias"] else []) ++
        whyDataOk d k ++
        match tableOf d tgt with
        | none => whyTable d "stmt:delete" tgt
        | some t => match wh with
          | some w => ifLowerFails d env (scopeOf d t tgt) w
          | none => []
    | .Merge .. => ["stmt:merge"]
    | .Message _ => ["stmt:message"]
  orElse codes (stmtFallback s)

/-! ## Nodes -/

/-- Why `lowerCall` refuses a call. -/
def whyCall (d : Dialect) (env : TyEnv) (n : List String) (args : List Ast.Arg)
    (ret : Option Ast.Expr) : List String :=
  (if ret.isSome then ["node:call:ret"] else []) ++
    match d with
    | .tsql dep Γ =>
      match lookup dep.routines (dotted n) with
      | none => ["node:call:name"]
      | some callee =>
        let tys := calleeTys Γ callee
        (if args.length ≤ tys.length then [] else ["node:call:type"]) ++
          (tys.zip args).flatMap fun (ty, a) =>
            if (lowerTsqlArg d env ty a).isSome then []
            else if (lowerTsqlArgShape d env a).isNone then
              "node:call:args" :: match a with
                | .Arg none e false => whyLower d env {} e
                | _ => []
            else ["node:call:type"]
    | .pg Γ _ =>
      let callee := dotted (n.map pgName)
      match lookup Γ.routines callee with
      | none => ["node:call:name"]
      | some modes =>
        let tys := calleeTys Γ callee
        (if args.length ≤ modes.length then [] else ["node:call:args"]) ++
          (if args.length ≤ tys.length then [] else ["node:call:type"]) ++
          ((modes.zip tys).zip args).flatMap fun ((m, ty), a) =>
            if (lowerPgArg d env ty (m, a)).isSome then []
            else if (lowerPgArgShape d env (m, a)).isNone then
              "node:call:args" :: match m, a with
                | .In, .Arg none e false => whyLower d env {} e
                | _, _ => []
            else ["node:call:type"]

/-- Why `txnBracket` refuses a statement list: a `BEGIN TRAN` inside an
open one (`nested`), a `COMMIT` with none open, one left open at the end,
or something between them that may not stand in an open transaction. -/
def whyBracket (ns : List Ast.Node) : List String :=
  go false ns
where
  go (open_ : Bool) : List Ast.Node → List String
    | [] => if open_ then ["node:txn:bracket"] else []
    | n :: ns =>
      if isTxn "BEGIN" n then (if open_ then ["node:txn:nested"] else []) ++ go true ns
      else if isTxn "COMMIT" n then (if open_ then [] else ["node:txn:commit"]) ++ go false ns
      else (if open_ && !txnInner n then ["node:txn:bracket"] else []) ++ go open_ ns

/-- Why `lowerThrow` refuses a `THROW`. -/
def whyThrow (d : Dialect) (k : Ast.RoutineKind) (cx : Ctx) (args : List Ast.Expr) :
    List String :=
  match d with
  | .pg _ _ => ["node:throw"]
  | .tsql _ _ =>
    if !(k matches .Procedure) then ["node:throw:function"]
    else match args with
      | [] => if cx.inHandler then [] else ["node:throw:place"]
      | _ => if throwOk args then [] else ["node:throw:args"]

/-- Why `lowerRaise` refuses a `RAISE`. A bare `RAISE;` outside a handler
is `node:throw:place`, as T-SQL's bare `THROW;` is. -/
def whyRaise (d : Dialect) (cx : Ctx) (lvl fmt : Option String) (args : List Ast.Expr)
    (opts : List Ast.RaiseOpt) : List String :=
  match d with
  | .tsql _ _ => ["node:raise"]
  | .pg _ _ => match lvl, fmt, args, opts with
    | none, none, [], [] => if cx.inHandler then [] else ["node:throw:place"]
    | _, _, _, _ =>
      (if (lvl.map (·.toUpper == "EXCEPTION")).getD true then [] else ["node:raise:level"]) ++
        (if args.isEmpty && (fmt.map fun f => !f.toList.contains '%').getD true &&
            (fmt.isSome || !opts.isEmpty) then [] else ["node:raise:format"]) ++
        (if raiseOptsOk fmt opts then [] else ["node:raise:opts"])

/-- Why `lowerTxn` refuses a transaction statement: in a function, named,
or an op out of place (`begin`/`commit` outside a bracket, PL/pgSQL
`commit` where it may not stand, `rollback`, `save`, ...). -/
def whyTxn (k : Ast.RoutineKind) (op : String) (name : Option String) : List String :=
  if !(k matches .Procedure) then ["node:txn:function"]
  else if name.isSome then ["node:txn:named"]
  else ["node:txn:" ++ sanitise op]

mutual
  /-- Why `lowerNode` refuses a node, in the context `cx` the lowering
  gives it. -/
  partial def whyNode (d : Dialect) (env : TyEnv) (k : Ast.RoutineKind) (cx : Ctx) :
      Ast.Node → List String
    | .NStmt s => whyStmt d env k s
    | .NBlock (.Block [] body) => orElse (whyNodes d env k cx.inner body) "node:nblock"
    | .NBlock (.Block [.Declare r (some ty) none] [.ForQuery r' q body]) =>
      orElse (whyFor d env k r ty r' q body) "node:for-query"
    | .NBlock (.Block _ _) => ["node:block-decls"]
    | .If c t e =>
      let cond : List String := match existsCond c with
        | some (q, _) =>
          if !(k matches .Procedure) then ["routine:function"]
          else if (lowerExists d env q).isSome then []
          else orElse (whyExists d env q) "expr:exists"
        | none => ifLowerFails d env {} c
      orElse (cond ++ whyNodes d env k cx.inner t ++ whyNodes d env k cx.inner e) "node:if"
    | .While c b =>
      orElse (ifLowerFails d env {} c ++ whyNodes d env k cx.inner b) "node:while"
    | .ForQuery .. => ["node:for-query"]
    | .Call n args ret =>
      orElse ((if cx.handled then ["node:call:handled"] else []) ++
        (if (lowerCall d env n args ret).isSome then [] else whyCall d env n args ret)) "node:call"
    | .Setting n _ => ["node:setting:" ++ sanitise n]
    | .Try b hs =>
      let codes : List String := match d, hs with
        | .tsql _ _, [.Handler [] h] =>
          if !(k matches .Procedure) then ["node:try:function"]
          else if !cx.nested && cx.txn && rollbackRethrow b h then
            -- The idiom where it may stand: its body is not a bracket.
            orElse (whyBracket b) "node:txn:bracket"
          else if rollbackRethrow b h then
            -- The idiom where it may not stand (inside `IF`, `WHILE`, a
            -- block or another handler): its handler is explained by the
            -- code, its body is diagnosed where it stands.
            "node:try:idiom" :: whyNodes d env k cx.body b
          else whyNodes d env k cx.body b ++ whyNodes d env k cx.handler h
        | .pg _ _, [.Handler ["others"] h] =>
          whyNodes d env k cx.body b ++ whyNodes d env k cx.handler h
        | _, _ => ["node:try:handlers"]
      orElse codes "node:try"
    | .Throw args => orElse (whyThrow d k cx args) "node:throw"
    | .Raise lvl fmt args opts => orElse (whyRaise d cx lvl fmt args opts) "node:raise"
    | .Txn op name => orElse (whyTxn k op name) "node:txn"

  /-- The codes of the nodes `lowerNode` refuses. -/
  partial def whyNodes (d : Dialect) (env : TyEnv) (k : Ast.RoutineKind) (cx : Ctx) :
      List Ast.Node → List String
    | [] => []
    | n :: ns =>
      (if (lowerNode d env k cx n).isSome then [] else whyNode d env k cx n) ++
        whyNodes d env k cx ns
end

/-! ## Stores -/

/-- The usable catalog table of a plain query, if any. -/
def queryTable (d : Dialect) (q : Ast.Select) : Option CatTable := do
  let (_, _, _, tgt, _) ← selParts q
  tableOf d tgt

/-- The value of a store into a target declared `ty` that is not in the
slice, by its form (a function, a cast, a literal the target does not
read, ...): why the store cannot be shown to keep it. -/
def whyStoreValue (d : Dialect) (env : TyEnv) (ty : Option Ast.Ty) (v : Ast.Expr) : List String :=
  let want := ty.bind (valTypeOf d)
  if v matches .Null then [] else
  match typedTo d env {} want v with
  | some tv =>
    -- In the slice but not fitting the target: a literal of the wrong
    -- form is named (`expr:bit-literal`), a narrowing only by its store.
    if (want.map fun w => fitsSTy w tv).getD true then [] else whyValue d env {} want v
  | none => whyValue d env {} want v

/-- Why `storesStmt` refuses a statement: the target of the store and its
declared type, and the value's form when it is outside the slice. A
target that is no variable of the routine (`known`) is left to the scope
check (`routine:scope:undeclared`), a pick from a table that is not
usable to the statement's own codes (`stmt:select:table:...`). -/
def whyStoresStmt (d : Dialect) (env wenv : TyEnv) (ret : Option Ast.Ty) (known : Var → Bool) :
    Ast.Stmt → List String
  | .Assign t (.Subquery q) => match target d t, queryTable d q with
    | some x, some _ => if known x then ["store:pick:" ++ varDetail wenv x] else []
    | _, _ => []
  | .SSelect q => match selParts q, queryTable d q with
    | some (items, into, _, _, _), some _ =>
      let assigned := items.filterMap fun
        | .SelItem e _ (some v) => (target d (.Var v)).map (·, e)
        | _ => none
      let intos := (into.zip items).filterMap fun
        | (x, .SelItem e _ _) => (target d x).map (·, e)
      (assigned ++ intos).filterMap fun (x, e) =>
        if !known x || storeOkTy d (lookup wenv x) false (itemTy d env q e) then none
        else some ("store:pick:" ++ varDetail wenv x)
    | _, _ => []
  | .Assign t v => match target d t with
    | some x =>
      if known x then ("store:assign:" ++ varDetail wenv x) :: whyStoreValue d env (lookup wenv x) v
      else []
    | none => []
  | .Declare _ ty (some v) => ("store:declare:" ++ tyDetail ty) :: whyStoreValue d env ty v
  | .Return (some v) => ("store:return:" ++ tyDetail ret) :: whyStoreValue d env ret v
  | _ => []

mutual
  partial def whyStoresNode (d : Dialect) (env wenv : TyEnv) (ret : Option Ast.Ty)
      (known : Var → Bool) : Ast.Node → List String
    | .NStmt s => whyStoresStmt d env wenv ret known s
    | .NBlock (.Block ds body) =>
      (ds.flatMap fun s =>
        if storesStmt d env wenv ret s then [] else whyStoresStmt d env wenv ret known s) ++
        whyStoresNodes d env wenv ret known body
    | .If _ t e => whyStoresNodes d env wenv ret known t ++ whyStoresNodes d env wenv ret known e
    | .While _ b => whyStoresNodes d env wenv ret known b
    | .ForQuery r q b =>
      let items := match selParts q with
        | some (items, _, _, _, _) => items
        | none => []
      b.flatMap fun n => match fieldAssign (pgName r) n with
        | some (x, a) =>
          match items.find? (fun | .SelItem _ (some a') _ => pgName a' == a | _ => false) with
          | some (.SelItem e _ _) =>
            if !known x || storeOkTy d (lookup wenv x) false (itemTy (withRecord d r) env q e) then []
            else ["store:for:" ++ varDetail wenv x]
          | none => ["node:for-query"]
        | none => ["node:for-query"]
    | .Call .. => ["store:call"]
    | .Try b hs =>
      whyStoresNodes d env wenv ret known b ++
        hs.flatMap fun | .Handler _ h => whyStoresNodes d env wenv ret known h
    | .Setting .. | .Throw _ | .Raise .. | .Txn .. => []

  partial def whyStoresNodes (d : Dialect) (env wenv : TyEnv) (ret : Option Ast.Ty)
      (known : Var → Bool) : List Ast.Node → List String
    | [] => []
    | n :: ns =>
      (if storesNode d env wenv ret n then [] else whyStoresNode d env wenv ret known n) ++
        whyStoresNodes d env wenv ret known ns
end

/-- Why `storesRoutine` refuses a routine. `known` says which names are
the routine's variables (parameters and declared locals). -/
def whyStoresRoutine (d : Dialect) (env wenv : TyEnv) (retTy : Ast.Ty → Ast.Ty) (known : Var → Bool) :
    Ast.Routine → List String
  | .Routine kind _ _ returns _ _ (.Block ds body) =>
    let ret := match kind with
      | .Function => returns.map retTy
      | .Procedure => none
    (ds.flatMap fun s =>
      if storesStmt d env wenv ret s then [] else whyStoresStmt d env wenv ret known s) ++
      whyStoresNodes d env wenv ret known body

/-! ## Routines -/

/-- Why `lowerParam` refuses a parameter: a default that is not NULL or a
slice value fitting the declared type (`routine:param:default`), or a
type no value of the slice fits (`routine:param:<type>`). -/
def whyParam (d : Dialect) (env : TyEnv) : Ast.Param → List String
  | .Param _ ty _ dflt =>
    if !dfltOk d env ty dflt && (valTypeOf d ty).isNone then
      ["routine:param:" ++ tyDetail (some ty)]
    else ["routine:param:default"]

/-- A parameter as lowered, or, when its default is refused, without a
default: so that the scope check still knows its name. -/
def paramOrStub (d : Dialect) (env : TyEnv) (p : Ast.Param) : Core.Param :=
  match lowerParam d env p, p with
  | some q, _ => q
  | none, .Param name _ mode _ =>
    ⟨(match d with | .tsql _ _ => name | .pg _ _ => pgName name), lowerMode mode, none⟩

/-- Why `caseSafe` refuses the routine's names. -/
def whyNames (xs : List String) : List String :=
  if !asciiNames xs then ["routine:names:non-ascii"]
  else if !caseSafe xs then ["routine:names:case"]
  else []

/-- Why `wellScoped` refuses the lowered routine. -/
def whyScope (r : Core.Routine) : List String :=
  let declared := r.params.map (·.name) ++ r.locals
  (if distinct declared then [] else ["routine:scope:duplicate"]) ++
    (if r.vars.all declared.contains then [] else ["routine:scope:undeclared"])

/-- Why `lowerT` refuses a T-SQL routine: every stage of the lowering is
asked, in its order, and the ones that refuse are named. -/
def whyT (dep : Deployment) (Γ : Catalog) (a : Ast.Routine) : List String :=
  match a with
  | .Routine kind _ params _ _ _ body =>
    let d := Dialect.tsql dep Γ
    let env := envT a
    let ps := params.flatMap fun p =>
      if (lowerParam d env p).isSome then [] else whyParam d env p
    let (decls, nodes) : List String × List Ast.Node := match body with
      | .Block [] ns => ([], ns)
      | .Block (_ :: _) ns => (["routine:block-decls"], ns)
    let txn :=
      if hasTxnNodes nodes && !(leadingSettings nodes).contains ("XACT_ABORT", true) then
        ["node:txn:no-xact-abort"]
      else []
    let bracket := if txnBracket nodes then [] else orElse (whyBracket nodes) "node:txn:bracket"
    let known : Var → Bool := fun x =>
      (params.map fun | .Param n _ _ _ => n).contains x || ((declsBlock body).map (·.1)).contains x
    let stores :=
      if storesRoutine d env env id a then [] else whyStoresRoutine d env env id known a
    let cx : Ctx := { txn := true }
    let (body, scope) : List String × List String := match lowerNodes d env kind cx nodes with
      | none => (whyNodes d env kind cx nodes, [])
      | some stmts =>
        let ps' := params.map (paramOrStub d env)
        let (ps', stmts) : List Core.Param × List Stmt := match kind with
          | .Procedure => (ps' ++ [statusParam], Stmt.assign statusVar (.lit 0) :: stmts)
          | .Function => (ps', stmts)
        let r : Core.Routine :=
          { params := ps', locals := (declsBlock body).map (·.1) ++ hiddenLocals (seqList stmts),
            body := seqList stmts }
        ([], whyScope r ++ whyNames r.vars)
    finish (ps ++ decls ++ txn ++ bracket ++ stores ++ body ++ scope)

/-- Why `lowerP` refuses a PL/pgSQL routine. -/
def whyP (Γ : Catalog) (b : Ast.Routine) : List String :=
  match b with
  | .Routine kind _ params _ sec _ (.Block decls stmts) =>
    let d := Dialect.pg Γ (pgVars b)
    let env := envP b
    let ps := params.flatMap fun p =>
      if (lowerParam d env p).isSome then [] else whyParam d env p
    let ds := decls.flatMap fun s =>
      if (lowerDecl d env s).isSome then [] else
      match s with
      | .Declare _ _ (some e) => orElse (whyLower d env {} e) "stmt:declare"
      | s => [stmtFallback s]
    let stores :=
      if storesRoutine d env env stripMod b then []
      else whyStoresRoutine d env env stripMod (pgVars b).contains b
    let cx : Ctx := { commit := (kind matches .Procedure) && (sec matches .Caller) }
    let (body, scope) : List String × List String :=
      match lowerNodes d env kind cx stmts, decls.mapM (lowerDecl d env) with
      | none, _ => (whyNodes d env kind cx stmts, [])
      | some _, none => ([], [])
      | some body, some ds' =>
        let ps' := params.map (paramOrStub d env)
        let body := (ds'.map (·.2)).flatten ++ body
        let (ps', body) : List Core.Param × List Stmt :=
          if kind matches .Procedure && !hasStatus ps' then
            (ps' ++ [statusParam], Stmt.assign statusVar (.lit 0) :: body)
          else (ps', body)
        let r : Core.Routine :=
          { params := ps', locals := ds'.map (·.1) ++ hiddenLocals (seqList body),
            body := seqList body }
        ([], whyScope r ++ (if asciiNames r.vars then [] else ["routine:names:non-ascii"]))
    finish (ps ++ ds ++ stores ++ body ++ scope)

end Sqldiff.Mirror
