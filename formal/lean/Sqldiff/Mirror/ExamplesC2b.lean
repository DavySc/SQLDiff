/-
Checked examples for C2b, strings: confirmed and refused pairs, the
residual assumptions each confirmation names, and runs of the core
interpreter showing NULL against the empty string, concatenation, T-SQL's
trailing-space padding against PostgreSQL's exact comparison, `LEN`
against `length`, and why `ISNULL` must not truncate. Every `#guard` runs
on every `lake build`.
-/
import Sqldiff.Mirror.Examples

namespace Sqldiff.Mirror.ExamplesC2b

open Sqldiff.Core (SExpr)
open Sqldiff.Mirror.Examples
open Sqldiff.Gate (Answer)

/-! ## Building blocks -/

def vc (n : Nat) : Ast.Ty := .Ty "VARCHAR" [toString n]
def nvc (n : Nat) : Ast.Ty := .Ty "NVARCHAR" [toString n]
def text : Ast.Ty := .Ty "TEXT" []
def S (s : String) : V := .str s
def str (s : String) : Ast.Expr := .Str s
def tr : Ast.Expr := tv "@r"
def pr : Ast.Expr := pv "r"

/-- The target catalog with the collation fact (`Catalog.binaryCollation`). -/
def Γb : Catalog := { routines := [], binaryCollation := true }

def concatNull : Ast.Node := .Setting "CONCAT_NULL_YIELDS_NULL" true

/-- T-SQL `r @a, @b, @r OUTPUT`: `@a` and `@b` of type `ta`, `@r` of type
`trt`. PL/pgSQL `r(a, b, INOUT r)`: `a` and `b` of type `pat`, `r` of
type `prt`. -/
def sT (body : List Ast.Node) (ta : Ast.Ty := vc 10) (trt : Ast.Ty := vc 20) : Ast.Routine :=
  tsqlProc ["r"] [inT "@a" ta, inT "@b" ta, ioT "@r" trt] body
def sP (body : List Ast.Node) (decls : List Ast.Stmt := []) (pat : Ast.Ty := text)
    (prt : Ast.Ty := text) : Ast.Routine :=
  pgProc ["r"] [inP "a" pat, inP "b" pat, ioP "r" prt] decls body

def cs (g : Catalog) (a b : Ast.Routine) : Answer := check dep g pairCert a b
def runT (g : Catalog) (a : Ast.Routine) (args : List V) := run (lowerT dep g a) args 30
def runP (g : Catalog) (b : Ast.Routine) (args : List V) := run (lowerP g b) args 30

/-! ## Concatenation

```sql
CREATE PROCEDURE r @a varchar(10), @b varchar(10), @r varchar(20) OUTPUT
AS SET @r = @a + @b;
-- PostgreSQL
CREATE PROCEDURE r(a text, b text, INOUT r text) ... r := a || b;
```

`@a + @b` has at most 20 bytes, so `@r` keeps it whole, and `text`
takes any string. -/

def catT : Ast.Routine := sT [set tr (.Bin "+" ta tb)]
def catP : Ast.Routine := sP [set pr (.Bin "||" pa pb)]

#guard isC2a (cs ∅ catT catP)
#guard residualOf dep ∅ catT == some ["ASM-CONCAT-NULL", "ASM-TSQL-STRING-ARGS"]
#guard runT ∅ catT [S "ab", S "cd", N] == some (.done [S "abcd", I 0] none 0 [] [])
#guard runP ∅ catP [S "ab", S "cd", N] == runT ∅ catT [S "ab", S "cd", N]
-- NULL in either operand: NULL (`CONCAT_NULL_YIELDS_NULL ON`).
#guard runT ∅ catT [N, S "cd", S "x"] == some (.done [N, I 0] none 0 [] [])
#guard runP ∅ catP [N, S "cd", S "x"] == runT ∅ catT [N, S "cd", S "x"]
-- The empty string is not NULL.
#guard runT ∅ catT [S "", S "", N] == some (.done [S "", I 0] none 0 [] [])
#guard runP ∅ catP [S "", S "", N] == runT ∅ catT [S "", S "", N]
-- `SET CONCAT_NULL_YIELDS_NULL ON` first discharges `ASM-CONCAT-NULL`.
def catOnT : Ast.Routine := sT [concatNull, set tr (.Bin "+" ta tb)]
#guard isC2a (cs ∅ catOnT catP)
#guard residualOf dep ∅ catOnT == some ["ASM-TSQL-STRING-ARGS"]
-- Not first: it does not cover what runs before it.
def catLateT : Ast.Routine := sT [set tr (.Bin "+" ta tb), concatNull]
#guard residualOf dep ∅ catLateT == some ["ASM-CONCAT-NULL", "ASM-TSQL-STRING-ARGS"]
-- `OFF` (NULL is the empty string) is outside the spec.
#guard reasonOf (cs ∅ (sT [.Setting "CONCAT_NULL_YIELDS_NULL" false, set tr (.Bin "+" ta tb)])
    catP) == "the T-SQL routine is outside the C1 spec"

/-! ### Refused or outside the slice -/

-- `@r varchar(15)` cannot hold 20 bytes: T-SQL would truncate where
-- PostgreSQL keeps (or, into `varchar(15)`, raises). Outside the slice: the
-- pair is only a C1 mirror (it rests on `ASM-C1-OPERATORS`).
#guard isC1 (cs ∅ (sT [set tr (.Bin "+" ta tb)] (trt := vc 15)) catP)
-- A PostgreSQL parameter carries no length, so `a || b` has no bound and
-- is not stored in a parameter declared `varchar(20)` (held to its
-- declared length to be safe either way).
#guard isC1 (cs ∅ catT (sP [set pr (.Bin "||" pa pb)] (prt := .Ty "VARCHAR" ["20"])))
-- `varchar` without a length is `text` in PostgreSQL.
#guard isC2a (cs ∅ catT (sP [set pr (.Bin "||" pa pb)] (prt := .Ty "VARCHAR" [])))
-- Over 4000 the T-SQL `+` is outside the slice (8000-byte truncation):
-- `+` and `||` stay two operators.
#guard reasonOf (cs ∅ (sT [set tr (.Bin "+" ta tb)] (ta := nvc 4000) (trt := nvc 4000)) catP) ==
  "the lowered routines differ beyond the renaming"
-- `nvarchar(2000) + nvarchar(2000)` is 4000: in.
#guard isC2a (cs ∅ (sT [set tr (.Bin "+" ta tb)] (ta := nvc 2000) (trt := nvc 4000)) catP)
-- `max` types are outside: `+` stays an operator, refused.
#guard reasonOf (cs ∅ (sT [set tr (.Bin "+" ta tb)] (ta := .Ty "VARCHAR" ["MAX"])
  (trt := .Ty "VARCHAR" ["MAX"])) catP) == "the lowered routines differ beyond the renaming"
-- T-SQL `varchar` without a length (1 in a declaration) is outside.
#guard (strTypeT (.Ty "VARCHAR" [])).isNone
#guard strTypeT (.Ty "VARCHAR" ["4000"]) == some (some 4000, false)
#guard (strTypeT (.Ty "VARCHAR" ["4001"])).isNone
#guard (strTypeT (.Ty "CHAR" ["10"])).isNone
#guard (strTypeP (.Ty "CHAR" ["10"])).isNone
#guard strTypeP (.Ty "CHARACTER VARYING" ["10"]) == some (some 10, false)
-- PostgreSQL `+` on strings is not concatenation.
#guard reasonOf (cs ∅ catT (sP [set pr (.Bin "+" pa pb)])) ==
  "the lowered routines differ beyond the renaming"
-- T-SQL `||` (SQL Server 2025) is outside the slice.
#guard (typed (.tsql dep ∅) [("@a", vc 10), ("@b", vc 10)] {} (.Bin "||" ta tb)).isNone
-- A NULL literal operand: outside.
#guard (typed (.tsql dep ∅) [("@a", vc 10)] {} (.Bin "+" ta .Null)).isNone
-- A string and an integer: an implicit conversion, outside.
#guard (typed (.tsql dep ∅) [("@a", vc 10), ("@i", int)] {} (.Bin "+" ta (tv "@i"))).isNone

/-! ## Assignment and literals -/

-- `DECLARE @x varchar(5) = 'abc'` and `x varchar(5) := 'abc'`.
def litT : Ast.Routine :=
  tsqlProc ["r"] [inT "@a" (vc 10), inT "@b" (vc 10), ioT "@r" (vc 20)]
    [.NStmt (.Declare "@x" (some (vc 5)) (some (str "abc"))), set tr (.Bin "+" (tv "@x") ta)]
def litP : Ast.Routine :=
  sP [set pr (.Bin "||" (pv "x") pa)] [.Declare "x" (some (.Ty "VARCHAR" ["5"])) (some (str "abc"))]
def litCert : Cert := ⟨pairCert.vars ++ [("@x", "x")]⟩
#guard isC2a (check dep ∅ litCert litT litP)
#guard run (lowerT dep ∅ litT) [S "d", N, N] 30 == some (.done [S "abcd", I 0] none 0 [] [])
#guard run (lowerP ∅ litP) [S "d", N, N] 30 == run (lowerT dep ∅ litT) [S "d", N, N] 30
/- `'abcdef'` into `varchar(3)`: T-SQL gives `abc` (probed, eval/casts/
assign), PostgreSQL raises (probed). Outside the slice on both sides. -/
def longT : Ast.Routine :=
  tsqlProc ["r"] [] [.NStmt (.Declare "@x" (some (vc 3)) (some (str "abcdef")))]
def longP : Ast.Routine :=
  pgProc ["r"] [] [.Declare "x" (some (.Ty "VARCHAR" ["3"])) (some (str "abcdef"))] []
#guard !sliceT dep ∅ longT
#guard !sliceP ∅ longP
-- An `nvarchar` value is not stored in a `varchar` (characters outside
-- the code page would be replaced); the other way round is fine.
#guard !sliceT dep ∅ (sT [set tr ta] (ta := nvc 10) (trt := vc 20))
#guard sliceT dep ∅ (sT [set tr ta] (ta := vc 10) (trt := nvc 20))
-- Literals: printable ASCII without a backslash.
#guard strLitOk "it''s" && strLitOk "" && strLitOk "a b~"
#guard !strLitOk "é" && !strLitOk "a\\b" && !strLitOk "tab\t"
-- A non-ASCII literal stays an uninterpreted literal form (`str:é`): C1.
#guard isC1 (cs ∅ (sT [set tr (str "é")]) (sP [set pr (str "é")]))
-- A string is never stored in an integer, nor an integer in a string.
#guard !sliceT dep ∅ (tsqlProc ["r"] [ioT "@r"] [set tr (str "5")])
#guard !sliceT dep ∅ (sT [set tr (num 5)])

/-! ## `ISNULL` against `COALESCE`

`ISNULL(x, y)` has `x`'s type: a longer `y` is cut to `x`'s length. -/

-- `ISNULL(@a, 'none')`, `@a varchar(10)`: `'none'` fits, so it is `COALESCE`.
#guard isC2a (cs ∅ (sT [set tr (fn "ISNULL" [ta, str "none"])])
  (sP [set pr (fn "COALESCE" [pa, str "none"])]))
#guard isC2a (cs ∅ (sT [set tr (fn "ISNULL" [ta, tb])])
  (sP [set pr (fn "COALESCE" [pa, pb])]))
#guard runT ∅ (sT [set tr (fn "ISNULL" [ta, str ""])]) [N, N, N] ==
  some (.done [S "", I 0] none 0 [] [])
/- `ISNULL(@a, 'abcdef')` with `@a varchar(3)` is `abc` when `@a` is
NULL, `COALESCE` gives `abcdef`: outside the slice, so `ISNULL` and
`COALESCE` stay two operators and the pair is refused. -/
#guard reasonOf (cs ∅ (sT [set tr (fn "ISNULL" [ta, str "abcdef"])] (ta := vc 3))
    (sP [set pr (fn "COALESCE" [pa, str "abcdef"])])) ==
  "the lowered routines differ beyond the renaming"
-- `@b varchar(10)` into `@a varchar(5)`: may not fit.
#guard (typed (.tsql dep ∅) [("@a", vc 5), ("@b", vc 10)] {} (fn "ISNULL" [ta, tb])).isNone
-- `nvarchar` into `varchar`: outside; `varchar` into `nvarchar`: in.
#guard (typed (.tsql dep ∅) [("@a", vc 10), ("@b", nvc 5)] {} (fn "ISNULL" [ta, tb])).isNone
#guard (typed (.tsql dep ∅) [("@a", nvc 10), ("@b", vc 5)] {} (fn "ISNULL" [ta, tb])).isSome
-- The first argument must be a variable: an expression's type length is
-- not taken on trust.
#guard (typed (.tsql dep ∅) [("@a", vc 10), ("@b", vc 10)] {}
  (fn "ISNULL" [.Bin "+" ta tb, str "x"])).isNone
-- `COALESCE` and `CASE` over strings.
#guard isC2a (cs ∅ (sT [set tr (fn "COALESCE" [ta, tb, str ""])])
  (sP [set pr (fn "COALESCE" [pa, pb, str ""])]))
#guard isC2a (cs ∅ (sT [set tr (.Case none [.When2 (.IsNull ta false) (str "none")] (some ta))])
  (sP [set pr (.Case none [.When2 (.IsNull pa false) (str "none")] (some pa))]))
-- A string and an integer do not go together.
#guard (typed (.tsql dep ∅) [("@a", vc 10)] {} (fn "COALESCE" [ta, num 0])).isNone

/-! ## Comparison

```sql
IF @a = @b SET @r = 'eq' ELSE SET @r = 'ne';
-- PostgreSQL, the provable form
IF rtrim(a) = rtrim(b) THEN r := 'eq'; ELSE r := 'ne'; END IF;
```

Only with the collation fact. T-SQL pads with spaces, so `'x' = 'x '` is
TRUE; PostgreSQL compares `text` exactly. -/

def eqT : Ast.Routine := sT [.If (.Bin "=" ta tb) [set tr (str "eq")] [set tr (str "ne")]]
def eqP (c : Ast.Expr) : Ast.Routine := sP [.If c [set pr (str "eq")] [set pr (str "ne")]]
def rt (e : Ast.Expr) : Ast.Expr := fn "rtrim" [e]

#guard isC2a (cs Γb eqT (eqP (.Bin "=" (rt pa) (rt pb))))
#guard residualOf dep Γb eqT ==
  some ["ASM-TSQL-ANSI-NULLS", "ASM-BINARY-COLLATION", "ASM-TSQL-STRING-ARGS"]
#guard runT Γb eqT [S "x", S "x  ", N] == some (.done [S "eq", I 0] none 0 [] [])
#guard runP Γb (eqP (.Bin "=" (rt pa) (rt pb))) [S "x", S "x  ", N] ==
  runT Γb eqT [S "x", S "x  ", N]
-- Plain `a = b` is not the T-SQL comparison: refused, and it does differ.
#guard reasonOf (cs Γb eqT (eqP (.Bin "=" pa pb))) ==
  "the lowered routines differ beyond the renaming"
#guard runP Γb (eqP (.Bin "=" pa pb)) [S "x", S "x  ", N] == some (.done [S "ne", I 0] none 0 [] [])
-- Leading spaces and tabs count on both sides.
#guard runT Γb eqT [S " x", S "x", N] == some (.done [S "ne", I 0] none 0 [] [])
#guard runT Γb eqT [S "x\t", S "x", N] == some (.done [S "ne", I 0] none 0 [] [])
-- Case counts (a binary collation).
#guard runT Γb eqT [S "X", S "x", N] == some (.done [S "ne", I 0] none 0 [] [])
-- NULL: UNKNOWN, the `ELSE` branch; the empty string equals spaces.
#guard runT Γb eqT [N, N, N] == some (.done [S "ne", I 0] none 0 [] [])
#guard runT Γb eqT [S "", S "   ", N] == some (.done [S "eq", I 0] none 0 [] [])
-- A literal is trimmed when the pair is lowered: `@a = 'x '` is
-- `rtrim(a) = 'x'`.
#guard isC2a (cs Γb (sT [.If (.Bin "=" ta (str "x ")) [set tr (str "eq")] []])
  (sP [.If (.Bin "=" (rt pa) (str "x")) [set pr (str "eq")] []]))
-- `<>` is `NOT =`.
#guard isC2a (cs Γb (sT [.If (.Bin "<>" ta tb) [set tr (str "ne")] []])
  (sP [.If (.Bin "<>" (rt pa) (rt pb)) [set pr (str "ne")] []]))
/- Without the fact, string `=` is not in the slice: T-SQL's and
PostgreSQL's `=` stay one uninterpreted operator, so the plain pair is a
C1 mirror only (under `ASM-C1-OPERATORS`, which no deployment can
accept), and the `rtrim` form is refused. -/
#guard isC1 (cs ∅ eqT (eqP (.Bin "=" pa pb)))
#guard reasonOf (cs ∅ eqT (eqP (.Bin "=" (rt pa) (rt pb)))) ==
  "the lowered routines differ beyond the renaming"
-- `<` and `LIKE` stay outside even with the fact.
#guard (typed (.tsql dep Γb) [("@a", vc 10), ("@b", vc 10)] {} (.Bin "<" ta tb)).isNone
#guard isC1 (cs Γb (sT [.If (.Like ta tb false false) [set tr (str "eq")] []])
  (sP [.If (.Like pa pb false false) [set pr (str "eq")] []]))
-- `IS NULL` of a string needs no fact.
#guard isC2a (cs ∅ (sT [.If (.IsNull ta false) [set tr (str "null")] []])
  (sP [.If (.IsNull pa false) [set pr (str "null")] []]))

/-! ## `LEN` against `length`

T-SQL `LEN` leaves out trailing spaces, PostgreSQL `length` does not:
`LEN(@a)` is `length(rtrim(a))`. -/

def lenT : Ast.Routine :=
  tsqlProc ["r"] [inT "@a" (vc 10), inT "@b" (vc 10), ioT "@r" int] [set tr (fn "LEN" [ta])]
def lenP (e : Ast.Expr) : Ast.Routine :=
  pgProc ["r"] [inP "a" text, inP "b" text, ioP "r" integer] [] [set pr e]

#guard isC2a (cs ∅ lenT (lenP (fn "length" [rt pa])))
#guard isC2a (cs ∅ lenT (lenP (fn "char_length" [rt pa])))
#guard residualOf dep ∅ lenT == some ["ASM-TSQL-LEN-BMP", "ASM-TSQL-STRING-ARGS"]
#guard runT ∅ lenT [S "ab  ", N, N] == some (.done [I 2, I 0] none 0 [] [])
#guard runP ∅ (lenP (fn "length" [rt pa])) [S "ab  ", N, N] == runT ∅ lenT [S "ab  ", N, N]
#guard runT ∅ lenT [S "  ", N, N] == some (.done [I 0, I 0] none 0 [] [])
#guard runT ∅ lenT [N, N, N] == some (.done [N, I 0] none 0 [] [])
-- `length(a)` counts the trailing spaces: refused, and it differs.
#guard reasonOf (cs ∅ lenT (lenP (fn "length" [pa]))) ==
  "the lowered routines differ beyond the renaming"
#guard runP ∅ (lenP (fn "length" [pa])) [S "ab  ", N, N] == some (.done [I 4, I 0] none 0 [] [])

/-! ## Functions, calls and parameters -/

-- A function returning `varchar(10)`: `@a + 'x'` has at most 6 bytes.
def fT (ret : Ast.Ty) : Ast.Routine :=
  .Routine .Function ["f"] [inT "@a" (vc 5)] (some ret) .Caller none
    (.Block [] [.NStmt (.Return (some (.Bin "+" ta (str "x"))))])
def fP : Ast.Routine :=
  .Routine .Function ["f"] [inP "a" (vc 5)] (some text) .Caller none
    (.Block [] [.NStmt (.Return (some (.Bin "||" pa (str "x"))))])
#guard isC2a (check dep ∅ fnCert (fT (vc 10)) fP)
-- Returning `varchar(5)`: T-SQL would truncate.
#guard isC1 (check dep ∅ fnCert (fT (vc 5)) fP)
-- A string passed to a call, or an `OUTPUT` argument into a string
-- variable, is outside the slice: the conversions at the callee's
-- boundary differ between the dialects.
#guard !sliceT dep Γ (sT [.Call ["dbo", "log_value"] [arg ta false, arg tr true] none])
-- A parameter default must fit too.
#guard !sliceT dep ∅ (tsqlProc ["r"] [.Param "@a" (vc 3) .In (some (str "abcdef"))] [])
#guard sliceT dep ∅ (tsqlProc ["r"] [.Param "@a" (vc 3) .In (some (str "abc"))] [])

/-! ## The core's string operators (`SExpr.eval`) -/

def ev2 (s : SExpr) (a b : V) : Core.Res Unit := s.eval (σ2 a b) Core.nullStore
#guard Core.rtrimSp "ab  " == "ab"
#guard Core.rtrimSp "  " == ""
#guard Core.rtrimSp " a\t" == " a\t"
#guard ev2 (.concat (.var "a") (.var "b")) (S "a") (S "b") == .ok (S "ab")
#guard ev2 (.concat (.var "a") (.var "b")) (S "a") N == .ok N
#guard ev2 (.streq (.var "a") (.var "b")) (S "a") (S "a ") == .ok (.bool false)
#guard ev2 (.streq (mkRtrim (.var "a")) (mkRtrim (.var "b"))) (S "a") (S "a ") == .ok (.bool true)
#guard ev2 (.streq (.var "a") (.var "b")) N (S "a") == .ok N
#guard ev2 (.len (.var "a")) (S "héé") N == .ok (I 3)
#guard ev2 (.isNull (.var "a") false) (S "") N == .ok (.bool false)
-- `mkRtrim` folds literals and repeated trims (`mkRtrim_eval`).
#guard mkRtrim (.str "x  ") == .str "x"
#guard mkRtrim (mkRtrim (.var "a")) == .rtrim (.var "a")

end Sqldiff.Mirror.ExamplesC2b
