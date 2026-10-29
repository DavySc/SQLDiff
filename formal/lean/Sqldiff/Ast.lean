/-
Generated from `sqldiff_syntax::encode::SCHEMA` by `sqldiff_core::proof::lean_ast`.
Do not edit: run `SQLDIFF_BLESS=1 cargo test -p sqldiff-core --test lean_syntax`.

The abstract syntax the Rust parser reads, as Lean types, and the decoder
for its one-line encoding (`sqldiff_syntax::encode`). A line decodes only
if it is exactly a value of the schema.
-/

namespace Sqldiff.Ast

/-- The schema hash requests about abstract syntax carry (`sqldiff_core::proof::ast_schema_hash`). -/
def schemaHash : String := "ec077e5c1682af48"

mutual
  inductive Routine where
    | Routine («kind» : RoutineKind) («name» : (List String)) («params» : (List Param)) («returns» : (Option Ty)) («security» : Security) («volatility» : (Option String)) («body» : Block)
  inductive RoutineKind where
    | Procedure
    | Function
  inductive Param where
    | Param («name» : String) («ty» : Ty) («mode» : Mode) («default» : (Option Expr))
  inductive Mode where
    | In
    | Out
    | InOut
  inductive Security where
    | Caller
    | Owner
  inductive Block where
    | Block («decls» : (List Stmt)) («body» : (List Node))
  inductive Node where
    | NStmt («stmt» : Stmt)
    | NBlock («block» : Block)
    | If («cond» : Expr) («then» : (List Node)) («els» : (List Node))
    | While («cond» : Expr) («body» : (List Node))
    | ForQuery («var» : String) («query» : Select) («body» : (List Node))
    | Call («name» : (List String)) («args» : (List Arg)) («ret» : (Option Expr))
    | Setting («name» : String) («on» : Bool)
    | Try («body» : (List Node)) («handlers» : (List Handler))
    | Throw («args» : (List Expr))
    | Raise («level» : (Option String)) («format» : (Option String)) («args» : (List Expr)) («using» : (List RaiseOpt))
    | Txn («op» : String) («name» : (Option String))
  inductive Handler where
    | Handler («conds» : (List String)) («body» : (List Node))
  inductive RaiseOpt where
    | RaiseOpt («name» : String) («value» : Expr)
  inductive Arg where
    | Arg («name» : (Option String)) («value» : Expr) («output» : Bool)
  inductive Ty where
    | Ty («name» : String) («args» : (List String))
  inductive Expr where
    | Name («parts» : (List String))
    | Var («name» : String)
    | Num («text» : String)
    | Str («value» : String)
    | Null
    | Star («qual» : (Option String))
    | CallE («call» : CallF)
    | Bin («op» : String) («l» : Expr) («r» : Expr)
    | Unary («op» : String) («x» : Expr)
    | IsNull («x» : Expr) («neg» : Bool)
    | InList («x» : Expr) («list» : (List Expr)) («neg» : Bool)
    | InQuery («x» : Expr) («query» : Select) («neg» : Bool)
    | Between («x» : Expr) («lo» : Expr) («hi» : Expr) («neg» : Bool)
    | Like («x» : Expr) («pattern» : Expr) («neg» : Bool) («ci» : Bool)
    | Cast («x» : Expr) («ty» : Ty)
    | Case («operand» : (Option Expr)) («whens» : (List When2)) («els» : (Option Expr))
    | Exists («query» : Select)
    | Subquery («query» : Select)
    | Tuple («xs» : (List Expr))
    | Special («name» : String)
    | Extract («field» : String) («x» : Expr)
  inductive When2 where
    | When2 («cond» : Expr) («result» : Expr)
  inductive CallF where
    | CallF («name» : (List String)) («args» : (List Expr)) («distinct» : Bool) («order» : (List OrderItem)) («within_group» : Bool) («over» : Bool) («star» : Bool)
  inductive OrderItem where
    | OrderItem («expr» : Expr) («desc» : Bool) («nulls_first» : (Option Bool))
  inductive SelItem where
    | SelItem («expr» : Expr) («alias» : (Option String)) («assign» : (Option String))
  inductive Cte where
    | Cte («name» : String) («cols» : (List String)) («query» : Select)
  inductive SetOp where
    | SetOp («op» : String) («query» : Select)
  inductive Select where
    | Select («ctes» : (List Cte)) («for_json» : Bool) («set_ops» : (List SetOp)) («distinct» : Bool) («top» : (Option Expr)) («items» : (List SelItem)) («into» : (List Expr)) («into_strict» : Bool) («from» : (List From)) («where» : (Option Expr)) («group_by» : (List Expr)) («having» : (Option Expr)) («order_by» : (List OrderItem)) («offset» : (Option Expr)) («limit» : (Option Expr)) («locking» : (List String)) («values» : (Option (List (List Expr))))
  inductive From where
    | Table («name» : (List String)) («alias» : (Option String)) («hints» : (List String))
    | Sub («query» : Select) («alias» : (Option String)) («cols» : (List String))
    | Values («rows» : (List (List Expr))) («alias» : (Option String)) («cols» : (List String))
    | Func («name» : (List String)) («args» : (List Expr)) («schema» : (List Col)) («alias» : (Option String))
    | DataChange («kind» : String) («stmt» : Stmt) («alias» : (Option String))
    | Join («kind» : String) («left» : From) («right» : From) («on» : (Option Expr))
  inductive Col where
    | Col («name» : String) («ty» : Ty)
  inductive Stmt where
    | SSelect («query» : Select)
    | Insert («target» : (List String)) («cols» : (List String)) («source» : Src) («returning» : (List SelItem)) («returning_into» : (List Expr)) («on_conflict» : (Option Conflict))
    | Update («target» : (List String)) («alias» : (Option String)) («sets» : (List Set)) («from» : (List From)) («where» : (Option Expr))
    | Delete («target» : (List String)) («alias» : (Option String)) («from» : (List From)) («where» : (Option Expr))
    | Merge («target» : (List String)) («alias» : (Option String)) («hints» : (List String)) («source» : From) («on» : Expr) («whens» : (List When))
    | Assign («target» : Expr) («value» : Expr)
    | Return («value» : (Option Expr))
    | Declare («name» : String) («ty» : (Option Ty)) («default» : (Option Expr))
    | Message («x» : Expr)
  inductive Src where
    | SrcValues («rows» : (List (List Expr)))
    | SrcQuery («query» : Select)
    | SrcDefault
  inductive Conflict where
    | Conflict («cols» : (List String)) («action» : Action)
  inductive Action where
    | Nothing
    | DoUpdate («sets» : (List Set1)) («where» : (Option Expr))
  inductive Set where
    | Set («cols» : (List String)) («value» : Expr)
  inductive Set1 where
    | Set1 («col» : String) («value» : Expr)
  inductive When where
    | When («matched» : Bool) («by_source» : Bool) («cond» : (Option Expr)) («action» : MAction)
  inductive MAction where
    | MUpdate («sets» : (List Set1))
    | MDelete
    | MInsert («cols» : (List String)) («values» : (List Expr))
end

/-- A line in its generic form (`sqldiff_syntax::encode::Value`). -/
inductive Value where
  | ctor (tag : String) (kids : List Value)
  | list (xs : List Value)
  | str (hex : String)
  | none
  | bool (b : Bool)

private def isHexDigit (c : Char) : Bool := c.isDigit || ('a' ≤ c && c ≤ 'f')

private def hexVal (c : Char) : Nat :=
  if c.isDigit then c.toNat - '0'.toNat else c.toNat - 'a'.toNat + 10

mutual
  private partial def value : List Char → Option (Value × List Char)
    | '(' :: rest =>
      let tag := rest.takeWhile Char.isAlphanum
      if tag.isEmpty then none
      else match fields (rest.drop tag.length) with
        | some (kids, ')' :: rest) => some (.ctor (String.ofList tag) kids, rest)
        | _ => none
    | '[' :: ']' :: rest => some (.list [], rest)
    | '[' :: rest =>
      match value rest with
      | some (x, rest) =>
        match fields rest with
        | some (xs, ']' :: rest) => some (.list (x :: xs), rest)
        | _ => none
      | none => none
    | 's' :: ':' :: rest =>
      let h := rest.takeWhile isHexDigit
      if h.length % 2 == 0 then some (.str (String.ofList h), rest.drop h.length) else none
    | '-' :: rest => some (.none, rest)
    | 't' :: rest => some (.bool true, rest)
    | 'f' :: rest => some (.bool false, rest)
    | _ => none

  /-- ` v` repeated. -/
  private partial def fields : List Char → Option (List Value × List Char)
    | ' ' :: rest =>
      match value rest with
      | some (x, rest) =>
        match fields rest with
        | some (xs, rest) => some (x :: xs, rest)
        | none => none
      | none => none
    | cs => some ([], cs)
end

/-- The value a whole line holds, if it is exactly one value. -/
def parseLine (s : String) : Option Value :=
  match value s.toList with
  | some (v, []) => some v
  | _ => none

private def bytes : List Char → Option (List UInt8)
  | a :: b :: rest => (bytes rest).map (fun bs => (hexVal a * 16 + hexVal b).toUInt8 :: bs)
  | [] => some []
  | _ => none

private def str? : Value → Option String
  | .str h => (bytes h.toList).bind (fun bs => String.fromUTF8? ⟨bs.toArray⟩)
  | _ => none

private def bool? : Value → Option Bool
  | .bool b => some b
  | _ => none

private def list? {α : Type} : Value → (Value → Option α) → Option (List α)
  | .list xs, f => xs.mapM f
  | _, _ => none

private def opt? {α : Type} : Value → (Value → Option α) → Option (Option α)
  | .none, _ => some none
  | v, f => (f v).map some

mutual
  partial def decodeRoutine : Value → Option Routine
    | .ctor "Routine" [v0, v1, v2, v3, v4, v5, v6] => do
      let x0 ← decodeRoutineKind v0
      let x1 ← list? v1 (fun x => str? x)
      let x2 ← list? v2 (fun x => decodeParam x)
      let x3 ← opt? v3 (fun x => decodeTy x)
      let x4 ← decodeSecurity v4
      let x5 ← opt? v5 (fun x => str? x)
      let x6 ← decodeBlock v6
      pure (.Routine x0 x1 x2 x3 x4 x5 x6)
    | _ => none
  partial def decodeRoutineKind : Value → Option RoutineKind
    | .ctor "Procedure" [] => some .Procedure
    | .ctor "Function" [] => some .Function
    | _ => none
  partial def decodeParam : Value → Option Param
    | .ctor "Param" [v0, v1, v2, v3] => do
      let x0 ← str? v0
      let x1 ← decodeTy v1
      let x2 ← decodeMode v2
      let x3 ← opt? v3 (fun x => decodeExpr x)
      pure (.Param x0 x1 x2 x3)
    | _ => none
  partial def decodeMode : Value → Option Mode
    | .ctor "In" [] => some .In
    | .ctor "Out" [] => some .Out
    | .ctor "InOut" [] => some .InOut
    | _ => none
  partial def decodeSecurity : Value → Option Security
    | .ctor "Caller" [] => some .Caller
    | .ctor "Owner" [] => some .Owner
    | _ => none
  partial def decodeBlock : Value → Option Block
    | .ctor "Block" [v0, v1] => do
      let x0 ← list? v0 (fun x => decodeStmt x)
      let x1 ← list? v1 (fun x => decodeNode x)
      pure (.Block x0 x1)
    | _ => none
  partial def decodeNode : Value → Option Node
    | .ctor "NStmt" [v0] => do
      let x0 ← decodeStmt v0
      pure (.NStmt x0)
    | .ctor "NBlock" [v0] => do
      let x0 ← decodeBlock v0
      pure (.NBlock x0)
    | .ctor "If" [v0, v1, v2] => do
      let x0 ← decodeExpr v0
      let x1 ← list? v1 (fun x => decodeNode x)
      let x2 ← list? v2 (fun x => decodeNode x)
      pure (.If x0 x1 x2)
    | .ctor "While" [v0, v1] => do
      let x0 ← decodeExpr v0
      let x1 ← list? v1 (fun x => decodeNode x)
      pure (.While x0 x1)
    | .ctor "ForQuery" [v0, v1, v2] => do
      let x0 ← str? v0
      let x1 ← decodeSelect v1
      let x2 ← list? v2 (fun x => decodeNode x)
      pure (.ForQuery x0 x1 x2)
    | .ctor "Call" [v0, v1, v2] => do
      let x0 ← list? v0 (fun x => str? x)
      let x1 ← list? v1 (fun x => decodeArg x)
      let x2 ← opt? v2 (fun x => decodeExpr x)
      pure (.Call x0 x1 x2)
    | .ctor "Setting" [v0, v1] => do
      let x0 ← str? v0
      let x1 ← bool? v1
      pure (.Setting x0 x1)
    | .ctor "Try" [v0, v1] => do
      let x0 ← list? v0 (fun x => decodeNode x)
      let x1 ← list? v1 (fun x => decodeHandler x)
      pure (.Try x0 x1)
    | .ctor "Throw" [v0] => do
      let x0 ← list? v0 (fun x => decodeExpr x)
      pure (.Throw x0)
    | .ctor "Raise" [v0, v1, v2, v3] => do
      let x0 ← opt? v0 (fun x => str? x)
      let x1 ← opt? v1 (fun x => str? x)
      let x2 ← list? v2 (fun x => decodeExpr x)
      let x3 ← list? v3 (fun x => decodeRaiseOpt x)
      pure (.Raise x0 x1 x2 x3)
    | .ctor "Txn" [v0, v1] => do
      let x0 ← str? v0
      let x1 ← opt? v1 (fun x => str? x)
      pure (.Txn x0 x1)
    | _ => none
  partial def decodeHandler : Value → Option Handler
    | .ctor "Handler" [v0, v1] => do
      let x0 ← list? v0 (fun x => str? x)
      let x1 ← list? v1 (fun x => decodeNode x)
      pure (.Handler x0 x1)
    | _ => none
  partial def decodeRaiseOpt : Value → Option RaiseOpt
    | .ctor "RaiseOpt" [v0, v1] => do
      let x0 ← str? v0
      let x1 ← decodeExpr v1
      pure (.RaiseOpt x0 x1)
    | _ => none
  partial def decodeArg : Value → Option Arg
    | .ctor "Arg" [v0, v1, v2] => do
      let x0 ← opt? v0 (fun x => str? x)
      let x1 ← decodeExpr v1
      let x2 ← bool? v2
      pure (.Arg x0 x1 x2)
    | _ => none
  partial def decodeTy : Value → Option Ty
    | .ctor "Ty" [v0, v1] => do
      let x0 ← str? v0
      let x1 ← list? v1 (fun x => str? x)
      pure (.Ty x0 x1)
    | _ => none
  partial def decodeExpr : Value → Option Expr
    | .ctor "Name" [v0] => do
      let x0 ← list? v0 (fun x => str? x)
      pure (.Name x0)
    | .ctor "Var" [v0] => do
      let x0 ← str? v0
      pure (.Var x0)
    | .ctor "Num" [v0] => do
      let x0 ← str? v0
      pure (.Num x0)
    | .ctor "Str" [v0] => do
      let x0 ← str? v0
      pure (.Str x0)
    | .ctor "Null" [] => some .Null
    | .ctor "Star" [v0] => do
      let x0 ← opt? v0 (fun x => str? x)
      pure (.Star x0)
    | .ctor "CallE" [v0] => do
      let x0 ← decodeCallF v0
      pure (.CallE x0)
    | .ctor "Bin" [v0, v1, v2] => do
      let x0 ← str? v0
      let x1 ← decodeExpr v1
      let x2 ← decodeExpr v2
      pure (.Bin x0 x1 x2)
    | .ctor "Unary" [v0, v1] => do
      let x0 ← str? v0
      let x1 ← decodeExpr v1
      pure (.Unary x0 x1)
    | .ctor "IsNull" [v0, v1] => do
      let x0 ← decodeExpr v0
      let x1 ← bool? v1
      pure (.IsNull x0 x1)
    | .ctor "InList" [v0, v1, v2] => do
      let x0 ← decodeExpr v0
      let x1 ← list? v1 (fun x => decodeExpr x)
      let x2 ← bool? v2
      pure (.InList x0 x1 x2)
    | .ctor "InQuery" [v0, v1, v2] => do
      let x0 ← decodeExpr v0
      let x1 ← decodeSelect v1
      let x2 ← bool? v2
      pure (.InQuery x0 x1 x2)
    | .ctor "Between" [v0, v1, v2, v3] => do
      let x0 ← decodeExpr v0
      let x1 ← decodeExpr v1
      let x2 ← decodeExpr v2
      let x3 ← bool? v3
      pure (.Between x0 x1 x2 x3)
    | .ctor "Like" [v0, v1, v2, v3] => do
      let x0 ← decodeExpr v0
      let x1 ← decodeExpr v1
      let x2 ← bool? v2
      let x3 ← bool? v3
      pure (.Like x0 x1 x2 x3)
    | .ctor "Cast" [v0, v1] => do
      let x0 ← decodeExpr v0
      let x1 ← decodeTy v1
      pure (.Cast x0 x1)
    | .ctor "Case" [v0, v1, v2] => do
      let x0 ← opt? v0 (fun x => decodeExpr x)
      let x1 ← list? v1 (fun x => decodeWhen2 x)
      let x2 ← opt? v2 (fun x => decodeExpr x)
      pure (.Case x0 x1 x2)
    | .ctor "Exists" [v0] => do
      let x0 ← decodeSelect v0
      pure (.Exists x0)
    | .ctor "Subquery" [v0] => do
      let x0 ← decodeSelect v0
      pure (.Subquery x0)
    | .ctor "Tuple" [v0] => do
      let x0 ← list? v0 (fun x => decodeExpr x)
      pure (.Tuple x0)
    | .ctor "Special" [v0] => do
      let x0 ← str? v0
      pure (.Special x0)
    | .ctor "Extract" [v0, v1] => do
      let x0 ← str? v0
      let x1 ← decodeExpr v1
      pure (.Extract x0 x1)
    | _ => none
  partial def decodeWhen2 : Value → Option When2
    | .ctor "When2" [v0, v1] => do
      let x0 ← decodeExpr v0
      let x1 ← decodeExpr v1
      pure (.When2 x0 x1)
    | _ => none
  partial def decodeCallF : Value → Option CallF
    | .ctor "CallF" [v0, v1, v2, v3, v4, v5, v6] => do
      let x0 ← list? v0 (fun x => str? x)
      let x1 ← list? v1 (fun x => decodeExpr x)
      let x2 ← bool? v2
      let x3 ← list? v3 (fun x => decodeOrderItem x)
      let x4 ← bool? v4
      let x5 ← bool? v5
      let x6 ← bool? v6
      pure (.CallF x0 x1 x2 x3 x4 x5 x6)
    | _ => none
  partial def decodeOrderItem : Value → Option OrderItem
    | .ctor "OrderItem" [v0, v1, v2] => do
      let x0 ← decodeExpr v0
      let x1 ← bool? v1
      let x2 ← opt? v2 (fun x => bool? x)
      pure (.OrderItem x0 x1 x2)
    | _ => none
  partial def decodeSelItem : Value → Option SelItem
    | .ctor "SelItem" [v0, v1, v2] => do
      let x0 ← decodeExpr v0
      let x1 ← opt? v1 (fun x => str? x)
      let x2 ← opt? v2 (fun x => str? x)
      pure (.SelItem x0 x1 x2)
    | _ => none
  partial def decodeCte : Value → Option Cte
    | .ctor "Cte" [v0, v1, v2] => do
      let x0 ← str? v0
      let x1 ← list? v1 (fun x => str? x)
      let x2 ← decodeSelect v2
      pure (.Cte x0 x1 x2)
    | _ => none
  partial def decodeSetOp : Value → Option SetOp
    | .ctor "SetOp" [v0, v1] => do
      let x0 ← str? v0
      let x1 ← decodeSelect v1
      pure (.SetOp x0 x1)
    | _ => none
  partial def decodeSelect : Value → Option Select
    | .ctor "Select" [v0, v1, v2, v3, v4, v5, v6, v7, v8, v9, v10, v11, v12, v13, v14, v15, v16] => do
      let x0 ← list? v0 (fun x => decodeCte x)
      let x1 ← bool? v1
      let x2 ← list? v2 (fun x => decodeSetOp x)
      let x3 ← bool? v3
      let x4 ← opt? v4 (fun x => decodeExpr x)
      let x5 ← list? v5 (fun x => decodeSelItem x)
      let x6 ← list? v6 (fun x => decodeExpr x)
      let x7 ← bool? v7
      let x8 ← list? v8 (fun x => decodeFrom x)
      let x9 ← opt? v9 (fun x => decodeExpr x)
      let x10 ← list? v10 (fun x => decodeExpr x)
      let x11 ← opt? v11 (fun x => decodeExpr x)
      let x12 ← list? v12 (fun x => decodeOrderItem x)
      let x13 ← opt? v13 (fun x => decodeExpr x)
      let x14 ← opt? v14 (fun x => decodeExpr x)
      let x15 ← list? v15 (fun x => str? x)
      let x16 ← opt? v16 (fun x => list? x (fun x => list? x (fun x => decodeExpr x)))
      pure (.Select x0 x1 x2 x3 x4 x5 x6 x7 x8 x9 x10 x11 x12 x13 x14 x15 x16)
    | _ => none
  partial def decodeFrom : Value → Option From
    | .ctor "Table" [v0, v1, v2] => do
      let x0 ← list? v0 (fun x => str? x)
      let x1 ← opt? v1 (fun x => str? x)
      let x2 ← list? v2 (fun x => str? x)
      pure (.Table x0 x1 x2)
    | .ctor "Sub" [v0, v1, v2] => do
      let x0 ← decodeSelect v0
      let x1 ← opt? v1 (fun x => str? x)
      let x2 ← list? v2 (fun x => str? x)
      pure (.Sub x0 x1 x2)
    | .ctor "Values" [v0, v1, v2] => do
      let x0 ← list? v0 (fun x => list? x (fun x => decodeExpr x))
      let x1 ← opt? v1 (fun x => str? x)
      let x2 ← list? v2 (fun x => str? x)
      pure (.Values x0 x1 x2)
    | .ctor "Func" [v0, v1, v2, v3] => do
      let x0 ← list? v0 (fun x => str? x)
      let x1 ← list? v1 (fun x => decodeExpr x)
      let x2 ← list? v2 (fun x => decodeCol x)
      let x3 ← opt? v3 (fun x => str? x)
      pure (.Func x0 x1 x2 x3)
    | .ctor "DataChange" [v0, v1, v2] => do
      let x0 ← str? v0
      let x1 ← decodeStmt v1
      let x2 ← opt? v2 (fun x => str? x)
      pure (.DataChange x0 x1 x2)
    | .ctor "Join" [v0, v1, v2, v3] => do
      let x0 ← str? v0
      let x1 ← decodeFrom v1
      let x2 ← decodeFrom v2
      let x3 ← opt? v3 (fun x => decodeExpr x)
      pure (.Join x0 x1 x2 x3)
    | _ => none
  partial def decodeCol : Value → Option Col
    | .ctor "Col" [v0, v1] => do
      let x0 ← str? v0
      let x1 ← decodeTy v1
      pure (.Col x0 x1)
    | _ => none
  partial def decodeStmt : Value → Option Stmt
    | .ctor "SSelect" [v0] => do
      let x0 ← decodeSelect v0
      pure (.SSelect x0)
    | .ctor "Insert" [v0, v1, v2, v3, v4, v5] => do
      let x0 ← list? v0 (fun x => str? x)
      let x1 ← list? v1 (fun x => str? x)
      let x2 ← decodeSrc v2
      let x3 ← list? v3 (fun x => decodeSelItem x)
      let x4 ← list? v4 (fun x => decodeExpr x)
      let x5 ← opt? v5 (fun x => decodeConflict x)
      pure (.Insert x0 x1 x2 x3 x4 x5)
    | .ctor "Update" [v0, v1, v2, v3, v4] => do
      let x0 ← list? v0 (fun x => str? x)
      let x1 ← opt? v1 (fun x => str? x)
      let x2 ← list? v2 (fun x => decodeSet x)
      let x3 ← list? v3 (fun x => decodeFrom x)
      let x4 ← opt? v4 (fun x => decodeExpr x)
      pure (.Update x0 x1 x2 x3 x4)
    | .ctor "Delete" [v0, v1, v2, v3] => do
      let x0 ← list? v0 (fun x => str? x)
      let x1 ← opt? v1 (fun x => str? x)
      let x2 ← list? v2 (fun x => decodeFrom x)
      let x3 ← opt? v3 (fun x => decodeExpr x)
      pure (.Delete x0 x1 x2 x3)
    | .ctor "Merge" [v0, v1, v2, v3, v4, v5] => do
      let x0 ← list? v0 (fun x => str? x)
      let x1 ← opt? v1 (fun x => str? x)
      let x2 ← list? v2 (fun x => str? x)
      let x3 ← decodeFrom v3
      let x4 ← decodeExpr v4
      let x5 ← list? v5 (fun x => decodeWhen x)
      pure (.Merge x0 x1 x2 x3 x4 x5)
    | .ctor "Assign" [v0, v1] => do
      let x0 ← decodeExpr v0
      let x1 ← decodeExpr v1
      pure (.Assign x0 x1)
    | .ctor "Return" [v0] => do
      let x0 ← opt? v0 (fun x => decodeExpr x)
      pure (.Return x0)
    | .ctor "Declare" [v0, v1, v2] => do
      let x0 ← str? v0
      let x1 ← opt? v1 (fun x => decodeTy x)
      let x2 ← opt? v2 (fun x => decodeExpr x)
      pure (.Declare x0 x1 x2)
    | .ctor "Message" [v0] => do
      let x0 ← decodeExpr v0
      pure (.Message x0)
    | _ => none
  partial def decodeSrc : Value → Option Src
    | .ctor "SrcValues" [v0] => do
      let x0 ← list? v0 (fun x => list? x (fun x => decodeExpr x))
      pure (.SrcValues x0)
    | .ctor "SrcQuery" [v0] => do
      let x0 ← decodeSelect v0
      pure (.SrcQuery x0)
    | .ctor "SrcDefault" [] => some .SrcDefault
    | _ => none
  partial def decodeConflict : Value → Option Conflict
    | .ctor "Conflict" [v0, v1] => do
      let x0 ← list? v0 (fun x => str? x)
      let x1 ← decodeAction v1
      pure (.Conflict x0 x1)
    | _ => none
  partial def decodeAction : Value → Option Action
    | .ctor "Nothing" [] => some .Nothing
    | .ctor "DoUpdate" [v0, v1] => do
      let x0 ← list? v0 (fun x => decodeSet1 x)
      let x1 ← opt? v1 (fun x => decodeExpr x)
      pure (.DoUpdate x0 x1)
    | _ => none
  partial def decodeSet : Value → Option Set
    | .ctor "Set" [v0, v1] => do
      let x0 ← list? v0 (fun x => str? x)
      let x1 ← decodeExpr v1
      pure (.Set x0 x1)
    | _ => none
  partial def decodeSet1 : Value → Option Set1
    | .ctor "Set1" [v0, v1] => do
      let x0 ← str? v0
      let x1 ← decodeExpr v1
      pure (.Set1 x0 x1)
    | _ => none
  partial def decodeWhen : Value → Option When
    | .ctor "When" [v0, v1, v2, v3] => do
      let x0 ← bool? v0
      let x1 ← bool? v1
      let x2 ← opt? v2 (fun x => decodeExpr x)
      let x3 ← decodeMAction v3
      pure (.When x0 x1 x2 x3)
    | _ => none
  partial def decodeMAction : Value → Option MAction
    | .ctor "MUpdate" [v0] => do
      let x0 ← list? v0 (fun x => decodeSet1 x)
      pure (.MUpdate x0)
    | .ctor "MDelete" [] => some .MDelete
    | .ctor "MInsert" [v0, v1] => do
      let x0 ← list? v0 (fun x => str? x)
      let x1 ← list? v1 (fun x => decodeExpr x)
      pure (.MInsert x0 x1)
    | _ => none
end

/-- A routine from its line, if the line is exactly one. -/
def decodeRoutineLine (s : String) : Option Routine := (parseLine s).bind decodeRoutine

end Sqldiff.Ast
