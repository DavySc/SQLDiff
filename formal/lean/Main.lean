/-
`sqldiff-check`: answers one request per line on stdin with one JSON line
on stdout. A request is tab-separated:

  sqldiff-check/2 ID SCHEMA DIALECT-A TARGET-A TREE-A DIALECT-B TARGET-B TREE-B end

ID is digits; SCHEMA is `sqldiff_core::proof::schema_hash`, which must be
`Sqldiff.Syntax.schemaHash`; the trees come from
`sqldiff_core::proof::tree_line` and contain no tab. Anything else is
answered "unconfirmed".
-/
import Sqldiff.Gate

open Sqldiff.Gate

def protocol : String := "sqldiff-check/2"

/-- The JSON answer. `id` is digits and the other strings are constants
of this program, so nothing needs escaping. -/
def answerLine (id : String) : Answer → String
  | .confirmed t => s!"\{\"id\":\"{id}\",\"proof\":\"confirmed\",\"theorem\":\"{t}\"}"
  | .unconfirmed r => s!"\{\"id\":\"{id}\",\"proof\":\"unconfirmed\",\"reason\":\"{r}\"}"

/-- The answer for a well-formed request. -/
def decide (schema da ta xa db tb xb : String) : Answer :=
  if schema != Sqldiff.Syntax.schemaHash then
    .unconfirmed "the request's syntax schema is not this checker's: generate formal/lean/Sqldiff/Syntax.lean again and rebuild"
  else
    match Sqldiff.Tree.decode xa, Sqldiff.Tree.decode xb with
    | some ta', some tb' => check ⟨da, ta, ta'⟩ ⟨db, tb, tb'⟩
    | none, _ => .unconfirmed "tree A does not decode"
    | _, none => .unconfirmed "tree B does not decode"

def handle (line : String) : String :=
  match line.splitOn "\t" with
  | [p, id, schema, da, ta, xa, db, tb, xb, fin] =>
    if p == protocol && !id.isEmpty && id.all Char.isDigit && fin.startsWith "end" then
      answerLine id (decide schema da ta xa db tb xb)
    else
      answerLine "0" (.unconfirmed "malformed request")
  | _ => answerLine "0" (.unconfirmed "malformed request")

partial def serve (inp out : IO.FS.Stream) : IO Unit := do
  let line ← inp.getLine
  if line.isEmpty then
    return
  out.putStrLn (handle line)
  out.flush
  serve inp out

def main (args : List String) : IO UInt32 := do
  if args == ["--version"] then
    IO.println s!"sqldiff-check 0.2.0 ({protocol})"
    return 0
  serve (← IO.getStdin) (← IO.getStdout)
  return 0
