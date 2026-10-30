/-
Checked examples for C3d part 2, catalog tables: read-only tables with
opaque columns, constraints the reads do not see, and keys that are not
wholly visible ("The C3d tables" in `Sqldiff/Mirror/Lower.lean`). A read
of a read-only table is confirmed; a write to one is refused; a column
outside the slice is a name only, never read, and counts for ambiguity
and the PL/pgSQL column-variable conflict; a key with an opaque or a
string part leaves the table without a key. Runs of the core interpreter
show the projection read alike on both sides. Every `#guard` runs on
every `lake build`.
-/
import Sqldiff.Mirror.ExamplesC3d

namespace Sqldiff.Mirror.ExamplesC3d2

open Sqldiff.Core (Obs Db)
open Sqldiff.Mirror.Examples
open Sqldiff.Mirror.ExamplesC3a (tc runD nocount upd ins del)
open Sqldiff.Mirror.ExamplesC3b (asg als item countStarE one field)
open Sqldiff.Mirror.ExamplesC3d (ordT custT tbl jn qn eq and_ selF selectF forF rCert)
open Sqldiff.Gate (Answer)

/-! ## The tables

```sql
-- T-SQL
CREATE TABLE dbo.Item (Id int IDENTITY(1,1) NOT NULL PRIMARY KEY,
  Name varchar(20) NOT NULL, Qty decimal(12,2) NOT NULL,
  Added datetime2 NOT NULL DEFAULT SYSDATETIME());
CREATE TABLE dbo.Line (OrdId int NOT NULL REFERENCES dbo.Ord (Id),
  LineNo int NOT NULL, ItemId int NOT NULL REFERENCES dbo.Item (Id),
  Qty int NOT NULL CHECK (Qty > 0), Price decimal(12,2) NOT NULL,
  PRIMARY KEY (OrdId, LineNo));
CREATE TABLE dbo.Tag (Code char(4) NOT NULL PRIMARY KEY, N int NULL);
CREATE TABLE dbo.Tag2 (K int NOT NULL, Code char(4) NOT NULL, N int NULL,
  PRIMARY KEY (K, Code));
CREATE TABLE dbo.Feed (ItemId int NOT NULL, Price decimal(12,2) NOT NULL);
-- PostgreSQL: the same, `integer GENERATED ALWAYS AS IDENTITY`,
-- `numeric(12,2)`, `timestamp DEFAULT CURRENT_TIMESTAMP`, `char(4)`.
```

`Qty` of `Item`, `Price`, `Added` and `Code` are of types outside the
slice (`decimal`/`numeric`, `datetime2` = `datetime2(7)` against
`timestamp(6)`, `char(4)`): opaque. `Tag`'s key is opaque, `Tag2`'s
half opaque, `Feed` has none. -/

def opq (n t : String) (key : Bool := false) : OpqCol := ⟨n, t, key⟩

def itemT : CatTable :=
  { name := "item", readOnly := true
    cols := [⟨"id", "Id", true, true, .int .i32, none⟩, ⟨"name", "Name", true, false, .str 20 false, none⟩]
    opaqueCols := [opq "qty" "Qty", opq "added" "Added"] }
def lineT : CatTable :=
  { name := "line", readOnly := true
    cols := [⟨"ordid", "OrdId", true, true, .int .i32, none⟩, ⟨"lineno", "LineNo", true, true, .int .i32, none⟩,
      ⟨"itemid", "ItemId", true, false, .int .i32, none⟩, ⟨"qty", "Qty", true, false, .int .i32, none⟩]
    opaqueCols := [opq "price" "Price"] }
def tagT : CatTable :=
  { name := "tag", readOnly := true, cols := [⟨"n", "N", false, false, .int .i32, none⟩]
    opaqueCols := [opq "code" "Code" true] }
def tag2T : CatTable :=
  { name := "tag2", readOnly := true
    cols := [⟨"k", "K", true, true, .int .i32, none⟩, ⟨"n", "N", false, false, .int .i32, none⟩]
    opaqueCols := [opq "code" "Code" true] }
def feedT : CatTable :=
  { name := "feed", readOnly := true, cols := [⟨"itemid", "ItemId", true, false, .int .i32, none⟩]
    opaqueCols := [opq "price" "Price"] }

def Γr : Catalog := { routines := [], tables := [ordT, custT, itemT, lineT, tagT, tag2T, feedT] }
def depr : Deployment :=
  { rowCounts := false, routines := [],
    tables := [("dbo.Ord", "ord"), ("dbo.Cust", "cust"), ("dbo.Item", "item"), ("dbo.Line", "line"),
      ("dbo.Tag", "tag"), ("dbo.Tag2", "tag2"), ("dbo.Feed", "feed")] }

def checkR (a b : Ast.Routine) (cert : Cert := rCert) : Answer := check depr Γr cert a b

/-! ## What the catalog gives the core

The core table is the projection on the slice columns; the key stays only
when it is wholly visible. -/

#guard itemT.wellFormed && lineT.wellFormed && tagT.wellFormed && tag2T.wellFormed && feedT.wellFormed
#guard itemT.toDef == ⟨"item", [⟨"id", true, true⟩, ⟨"name", true, false⟩]⟩
#guard lineT.toDef.cols.map (·.key) == [true, true, false, false]
-- An opaque key column, alone or as half of the key: no key at all.
#guard tagT.toDef == ⟨"tag", [⟨"n", false, false⟩]⟩
#guard tag2T.toDef == ⟨"tag2", [⟨"k", true, false⟩, ⟨"n", false, false⟩]⟩
-- A string key column is usable in a read-only table and leaves it keyless.
def strKeyT : CatTable :=
  { name := "sk", readOnly := true
    cols := [⟨"code", "Code", true, true, .str 4 false, none⟩, ⟨"n", "N", false, false, .int .i32, none⟩] }
#guard strKeyT.wellFormed && !strKeyT.toDef.hasKey
#guard !{ strKeyT with readOnly := false }.wellFormed
-- Opaque columns only in a read-only table; their names count among the
-- others (distinct, T-SQL spellings distinct up to case, ASCII, no `.`).
#guard !{ itemT with readOnly := false }.wellFormed
#guard !{ itemT with opaqueCols := [opq "name" "Nm"] }.wellFormed
#guard !{ itemT with opaqueCols := [opq "nm" "NAME"] }.wellFormed
#guard !{ itemT with opaqueCols := [opq "a.b" "A"] }.wellFormed
#guard !{ itemT with opaqueCols := [opq "é" "é"] }.wellFormed
-- At least one slice column.
#guard !{ feedT with cols := [] }.wellFormed

/-! ## Confirmed: reads

The converter's pair (`sqldiff convert --to postgres`, comments left
out), reading the two read-only tables by key through a join, by
`COUNT(*)` over a keyless table, by `EXISTS` and by a scalar subquery
over a table whose key is opaque:

```sql
CREATE PROCEDURE dbo.r @id int, @q int OUTPUT AS
BEGIN
  SELECT @q = l.Qty FROM dbo.Line l JOIN dbo.Item i ON i.Id = l.ItemId
    WHERE l.OrdId = @id AND l.LineNo = 1;
  SELECT @q = COUNT(*) FROM dbo.Feed WHERE ItemId = @id;
  IF EXISTS (SELECT * FROM dbo.Item WHERE Id = @id) SET @q = @q + 1;
  SET @q = (SELECT t.N FROM dbo.Tag t WHERE t.N > @id);
END
```
-/

def lineJoin (l i : List String) : List Ast.From :=
  [jn (tbl l (some "l")) (tbl i (some "i")) (eq (qn "i" "Id") (qn "l" "ItemId"))]
def lineJoinP : List Ast.From :=
  [jn (tbl ["line"] (some "l")) (tbl ["item"] (some "i")) (eq (qn "i" "id") (qn "l" "itemid"))]
def byKeyT : Ast.Expr := and_ (eq (qn "l" "OrdId") (tv "@id")) (eq (qn "l" "LineNo") (num 1))
def byKeyP : Ast.Expr := and_ (eq (qn "l" "ordid") (pv "p_id")) (eq (qn "l" "lineno") (num 1))

def readBodyT : List Ast.Node := [
  selectF [asg "@q" (qn "l" "Qty")] (lineJoin ["dbo", "Line"] ["dbo", "Item"]) (some byKeyT),
  selectF [asg "@q" countStarE] [tbl ["dbo", "Feed"]] (some (eq (tc "ItemId") (tv "@id"))),
  .If (.Exists (selF [item (.Star none)] [tbl ["dbo", "Item"]] (some (eq (tc "Id") (tv "@id")))))
    [set (tv "@q") (.Bin "+" (tv "@q") (num 1))] [],
  set (tv "@q") (.Subquery (selF [item (qn "t" "N")] [tbl ["dbo", "Tag"] (some "t")]
    (some (.Bin ">" (qn "t" "N") (tv "@id")))))]
def readBodyP : List Ast.Node := [
  forF [als (qn "l" "qty") "sqldiff_c1"] lineJoinP (some byKeyP) [field "p_q" "sqldiff_r" "sqldiff_c1"],
  .NStmt (.SSelect (selF [item countStarE] [tbl ["feed"]] (some (eq (pv "itemid") (pv "p_id"))) [pv "p_q"])),
  .If (.Exists (selF [item (.Star none)] [tbl ["item"]] (some (eq (pv "id") (pv "p_id")))))
    [set (pv "p_q") (.Bin "+" (pv "p_q") (num 1))] [],
  set (pv "p_q") (.Subquery (selF [item (qn "t" "n")] [tbl ["tag"] (some "t")]
    (some (.Bin ">" (qn "t" "n") (pv "p_id")))))]

def readT (body : List Ast.Node := readBodyT) (lead : List Ast.Node := [xactAbort, nocount]) : Ast.Routine :=
  tsqlProc ["dbo", "r"] [inT "@id", ioT "@q"] (lead ++ body)
def readP (body : List Ast.Node := readBodyP) (ps : List Ast.Param := [inP "p_id", ioP "p_q"]) :
    Ast.Routine :=
  pgProc ["r"] ps [] body

#guard isC2a (checkR readT readP)
#guard residualOf depr Γr readT == some ["ASM-TSQL-ARITH-ERRORS", "ASM-TSQL-ANSI-NULLS",
  "ASM-TSQL-STRING-ARGS", "ASM-NO-CALLER-TXN", "ASM-TSQL-ROWCOUNT-0"]
-- The pick by the whole key through the join is closed: both keys visible.
#guard (lowerT depr Γr (readT (readBodyT.take 1) [])).isSome

/-! ## Refused: writes to a read-only table -/

def writeT (n : Ast.Node) : Ast.Routine := tsqlProc ["dbo", "r"] [inT "@id", ioT "@q"] [nocount, n]
def writeP (n : Ast.Node) : Ast.Routine := pgProc ["r"] [inP "p_id", ioP "p_q"] [] [n]
#guard (lowerT depr Γr (writeT (del ["dbo", "Line"] (some (eq (tc "OrdId") (tv "@id")))))).isNone
#guard whyT depr Γr (writeT (del ["dbo", "Line"] (some (eq (tc "OrdId") (tv "@id"))))) ==
  ["stmt:delete:table:read-only"]
#guard (lowerT depr Γr (writeT (upd ["dbo", "Line"] [("Qty", num 1)] (some (eq (tc "OrdId") (tv "@id")))))).isNone
#guard whyT depr Γr (writeT (upd ["dbo", "Line"] [("Qty", num 1)] (some (eq (tc "OrdId") (tv "@id"))))) ==
  ["stmt:update:table:read-only"]
#guard (lowerT depr Γr (writeT (ins ["dbo", "Feed"] ["ItemId"] [[tv "@id"]]))).isNone
#guard whyT depr Γr (writeT (ins ["dbo", "Feed"] ["ItemId"] [[tv "@id"]])) == ["stmt:insert:table:read-only"]
#guard (lowerP Γr (writeP (del ["line"] (some (eq (pv "ordid") (pv "p_id")))))).isNone
#guard whyP Γr (writeP (ins ["feed"] ["itemid"] [[pv "p_id"]])) == ["stmt:insert:table:read-only"]
-- The same table, not read-only, is written.
def Γw : Catalog := { Γr with tables := [{ feedT with readOnly := false, opaqueCols := [] }] }
#guard (lowerT depr Γw (writeT (ins ["dbo", "Feed"] ["ItemId"] [[tv "@id"]]))).isSome

/-! ## Refused: an opaque column read

Qualified or not, in the select list or a condition, on the C1 path too. -/

def pickR (froms : List Ast.From) (wh : Option Ast.Expr) (e : Ast.Expr) : Ast.Routine :=
  tsqlProc ["r"] [inT "@id", ioT "@q"] [selectF [asg "@q" e] froms wh]
#guard (lowerT depr Γr (pickR [tbl ["dbo", "Line"] (some "l")] (some byKeyT) (qn "l" "Price"))).isNone
#guard whyT depr Γr (pickR [tbl ["dbo", "Line"] (some "l")] (some byKeyT) (qn "l" "Price")) ==
  ["stmt:select:column:opaque"]
#guard (lowerT depr Γr (pickR [tbl ["dbo", "Line"]] (some (and_ (eq (tc "OrdId") (tv "@id"))
  (eq (tc "LineNo") (num 1)))) (tc "Price"))).isNone
#guard whyT depr Γr (pickR [tbl ["dbo", "Line"]] (some (and_ (eq (tc "OrdId") (tv "@id"))
  (eq (tc "LineNo") (num 1)))) (tc "Price")) == ["stmt:select:column:opaque"]
-- In a condition, under an uninterpreted operator (the C1 path).
#guard (lowerT depr Γr (tsqlProc ["r"] [inT "@id", ioT "@q"] [selectF [asg "@q" countStarE]
  [tbl ["dbo", "Line"] (some "l")] (some (.Bin ">" (qn "l" "Price") (num 0)))])).isNone
#guard (lowerT depr Γr (tsqlProc ["r"] [inT "@id", ioT "@q"] [selectF [asg "@q" countStarE]
  [tbl ["dbo", "Line"] (some "l")] (some (.IsNull (fn "ABS" [qn "l" "Price"]) false))])).isNone
#guard (lowerP Γr (pgProc ["r"] [inP "p_id", ioP "p_q"] [] [.NStmt (.SSelect (selF [item countStarE]
  [tbl ["line"]] (some (.IsNull (pv "price") false)) [pv "p_q"]))])).isNone
#guard whyP Γr (pgProc ["r"] [inP "p_id", ioP "p_q"] [] [.NStmt (.SSelect (selF [item countStarE]
  [tbl ["line"]] (some (.IsNull (pv "price") false)) [pv "p_q"]))]) == ["stmt:select:column:opaque"]
-- Under T-SQL's case rules too: `PRICE` is the opaque `Price` (a
-- case-insensitive collation), never a column of the slice.
#guard (lowerT depr Γr (pickR [tbl ["dbo", "Line"]] (some (and_ (eq (tc "OrdId") (tv "@id"))
  (eq (tc "LineNo") (num 1)))) (tc "PRICE"))).isNone

/-! ## Refused: names an opaque column makes ambiguous

`Qty` is a slice column of `Line` and an opaque one of `Item`: unqualified
it is ambiguous (T-SQL 209, PostgreSQL 42702, eval/queries Q19). Without
the opaque name in the holders, it would resolve to `l.Qty`. -/

def ambQtyT : Ast.Routine := pickR (lineJoin ["dbo", "Line"] ["dbo", "Item"]) (some byKeyT) (tc "Qty")
#guard (lowerT depr Γr ambQtyT).isNone
#guard whyT depr Γr ambQtyT == ["name:ambiguous"]
#guard (lowerT depr Γr (pickR (lineJoin ["dbo", "Line"] ["dbo", "Item"]) (some byKeyT) (qn "l" "Qty"))).isSome
def forR (e : Ast.Expr) (wh : Ast.Expr := byKeyP) (ps : List Ast.Param := [inP "p_id", ioP "p_q"]) :
    Ast.Routine :=
  pgProc ["r"] ps [] [forF [als e "sqldiff_c1"] lineJoinP (some wh) [field "p_q" "sqldiff_r" "sqldiff_c1"]]
#guard (lowerP Γr (forR (pv "qty"))).isNone
#guard whyP Γr (forR (pv "qty")) == ["name:ambiguous"]
#guard (lowerP Γr (forR (qn "l" "qty"))).isSome
-- A name that is an opaque column of the one source: no variable either.
#guard (lowerP Γr (forR (qn "l" "qty") (and_ byKeyP (.IsNull (pv "added") true)))).isNone

/-! ## Refused: a PL/pgSQL variable spelt as an opaque column

A parameter `price` in a query over `line`, whose `price` is opaque
(42702, eval/queries Q19): the name is no variable there. -/

#guard (lowerP Γr (forR (qn "l" "qty") (and_ byKeyP (.Bin ">" (qn "l" "qty") (pv "price")))
  [inP "p_id", ioP "p_q", inP "price"])).isNone
#guard whyP Γr (forR (qn "l" "qty") (and_ byKeyP (.Bin ">" (qn "l" "qty") (pv "price")))
  [inP "p_id", ioP "p_q", inP "price"]) == ["name:column-variable"]
-- The same parameter under another name reads as a variable.
#guard (lowerP Γr (forR (qn "l" "qty") (and_ byKeyP (.Bin ">" (qn "l" "qty") (pv "p_price")))
  [inP "p_id", ioP "p_q", inP "p_price"])).isSome

/-! ## Keys with an opaque part

`Tag2`'s key is (`K`, `Code`), `Code` opaque. A pick that fixes `K`
only would be closed if `K` alone were the key; two rows may share `K`
(eval/queries Q16), so the table has no key in the core and the pick is
refused. The scalar subquery stands, counting as able to fail. -/

def tag2Pick : Ast.Routine :=
  pickR [tbl ["dbo", "Tag2"] (some "t")] (some (eq (qn "t" "K") (tv "@id"))) (qn "t" "N")
#guard (lowerT depr Γr tag2Pick).isNone
#guard whyT depr Γr tag2Pick == ["stmt:select:many-rows"]
-- With the key read as visible (`Code` a slice column of the key), the
-- same pick fixing only `K` is still refused, and fixing both is closed:
-- the refusal above comes from the opaque part.
def tag2Vis : CatTable :=
  { tag2T with opaqueCols := [], cols := tag2T.cols ++ [⟨"code", "Code", true, true, .int .i32, none⟩] }
#guard (lowerT depr { Γr with tables := [tag2Vis] } (pickR [tbl ["dbo", "Tag2"] (some "t")]
  (some (and_ (eq (qn "t" "K") (tv "@id")) (eq (qn "t" "Code") (num 1)))) (qn "t" "N"))).isSome
def scalarTag2T : Ast.Routine :=
  tsqlProc ["r"] [inT "@id", ioT "@q"] [set (tv "@q") (.Subquery (selF [item (qn "t" "N")]
    [tbl ["dbo", "Tag2"] (some "t")] (some (eq (qn "t" "K") (tv "@id")))))]
def scalarTag2P : Ast.Routine :=
  pgProc ["r"] [inP "p_id", ioP "p_q"] [] [set (pv "p_q") (.Subquery (selF [item (qn "t" "n")]
    [tbl ["tag2"] (some "t")] (some (eq (qn "t" "k") (pv "p_id")))))]
#guard isC1 (checkR scalarTag2T scalarTag2P)
#guard isC2a (checkR (tsqlProc ["r"] [inT "@id", ioT "@q"]
  [xactAbort, set (tv "@q") (.Subquery (selF [item (qn "t" "N")] [tbl ["dbo", "Tag2"] (some "t")]
    (some (eq (qn "t" "K") (tv "@id")))))]) scalarTag2P)

/-! ## The core as an oracle

Rows of `line` as the core sees them (`ordid`, `lineno`, `itemid`,
`qty`; `price` dropped): (1, 1, 10, 5), (1, 2, 11, 7), (2, 1, 10, 9); of
`item` (`id`, `name`): (10, 'a'), (11, 'b'); `feed` (`itemid`): 10, 10,
11; `tag` (`n`): 1, 2. -/

def S (s : String) : V := .str s
def dbr : Db Unit :=
  [("line", [[I 1, I 1, I 10, I 5], [I 1, I 2, I 11, I 7], [I 2, I 1, I 10, I 9]]),
   ("item", [[I 10, S "a"], [I 11, S "b"]]),
   ("feed", [[I 10], [I 10], [I 11]]),
   ("tag", [[I 1], [I 2]])]

def rT := lowerT depr Γr readT
def rP := lowerP Γr readP

/- `r(1)`: line (1, 1) has `Qty` 5; then `COUNT(*)` of `feed` rows for
item 1 is 0; no item 1: `EXISTS` is FALSE; one `tag` row has `N > 1`:
the scalar subquery is 2. -/
#guard runD rT [I 1, N] dbr == some (.done [I 2, I 0] none 0 dbr [])
#guard runD rP [I 1, N] dbr == runD rT [I 1, N] dbr
/- `r(10)`: no line of order 10 (`@q` keeps NULL); two `feed` rows for
item 10: 2; item 10 exists: 3; no `tag` row has `N > 10`: NULL. -/
#guard runD rT [I 10, N] dbr == some (.done [N, I 0] none 0 dbr [])
#guard runD rP [I 10, N] dbr == runD rT [I 10, N] dbr
/- `r(0)`: two `tag` rows have `N > 0`: `cardinality` on both sides. -/
#guard runD rT [I 0, N] dbr == some (.failed .cardinality [])
#guard runD rP [I 0, N] dbr == runD rT [I 0, N] dbr

end Sqldiff.Mirror.ExamplesC3d2
