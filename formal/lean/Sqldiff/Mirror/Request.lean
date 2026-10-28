/-
The mirror check as a `sqldiff-check/3` request (see `Main.lean`):
decoding of its fields and the answer. A field that does not decode, a
routine that does not decode, or an abstract syntax schema other than
`Sqldiff.Ast.schemaHash` is answered "unconfirmed".

Field encodings, all ASCII without tabs; NAME is the UTF-8 bytes of a
string in lower-case hex, two digits per byte:

  DEPLOYMENT  R or R,PAIR,PAIR,...   R is 1 when row counts are observed, else 0;
                                     PAIR is NAME:NAME, a T-SQL routine name
                                     (`.`-joined) and its target routine
  CATALOG     - or ENTRY,ENTRY,...   ENTRY is NAME:MODES, a target routine (folded
                                     as `Sqldiff.Mirror.pgName` folds) and one
                                     letter per parameter: i IN, o OUT, b INOUT
  CERT        - or PAIR,PAIR,...     PAIR is NAME:NAME, a T-SQL variable and its
                                     PL/pgSQL name
-/
import Sqldiff.Ast
import Sqldiff.Mirror.Check

namespace Sqldiff.Mirror.Request

open Sqldiff.Gate (Answer)

private def hexDigit (c : Char) : Option Nat :=
  if c.isDigit then some (c.toNat - '0'.toNat)
  else if 'a' ≤ c && c ≤ 'f' then some (c.toNat - 'a'.toNat + 10)
  else none

private def hexBytes : List Char → Option (List UInt8)
  | a :: b :: rest => do
    let x ← hexDigit a
    let y ← hexDigit b
    let bs ← hexBytes rest
    pure ((x * 16 + y).toUInt8 :: bs)
  | [] => some []
  | [_] => none

/-- A NAME: lower-case hex of UTF-8 bytes. -/
def name? (s : String) : Option String :=
  (hexBytes s.toList).bind fun bs => String.fromUTF8? ⟨bs.toArray⟩

/-- `-` is the empty list; otherwise comma-separated items. -/
def items? {α : Type} (s : String) (f : String → Option α) : Option (List α) :=
  if s = "-" then some [] else (s.splitOn ",").mapM f

def pair? (s : String) : Option (String × String) :=
  match s.splitOn ":" with
  | [a, b] => do pure (← name? a, ← name? b)
  | _ => none

def mode? : Char → Option Ast.Mode
  | 'i' => some .In
  | 'o' => some .Out
  | 'b' => some .InOut
  | _ => none

def deployment? (s : String) : Option Deployment :=
  match s.splitOn "," with
  | r :: ps => do
    let rc ← match r with
      | "0" => some false
      | "1" => some true
      | _ => none
    let routines ← ps.mapM pair?
    pure { rowCounts := rc, routines }
  | [] => none

def catalog? (s : String) : Option Catalog :=
  items? s fun e => match e.splitOn ":" with
    | [n, ms] => do pure (← name? n, ← ms.toList.mapM mode?)
    | _ => none

def cert? (s : String) : Option Cert := (items? s pair?).map Cert.mk

/-- The answer to a well-formed `mirror` request. -/
def answer (schema dep cat cert ra rb : String) : Answer :=
  if schema != Sqldiff.Ast.schemaHash then
    .unconfirmed "the request's abstract syntax schema is not this checker's: generate formal/lean/Sqldiff/Ast.lean again and rebuild"
  else
    match deployment? dep, catalog? cat, cert? cert with
    | some d, some g, some c =>
      match Ast.decodeRoutineLine ra, Ast.decodeRoutineLine rb with
      | some a, some b => check d g c a b
      | none, _ => .unconfirmed "routine A does not decode"
      | _, none => .unconfirmed "routine B does not decode"
    | none, _, _ => .unconfirmed "the deployment does not decode"
    | _, none, _ => .unconfirmed "the catalog does not decode"
    | _, _, none => .unconfirmed "the certificate does not decode"

#guard name? "40785f31" == some "@x_1"
#guard name? "4" == none
#guard name? "4G" == none
#guard (cert? "4078:78,4079:79").map (·.vars) == some [("@x", "x"), ("@y", "y")]
#guard (cert? "-").map (·.vars) == some []
#guard (catalog? "70:ib").isSome
#guard (catalog? "70:ix").isNone
#guard (deployment? "0").map (·.rowCounts) == some false
#guard (deployment? "2").isNone

end Sqldiff.Mirror.Request
