/-
SQLDiff's syntax tree as the checker receives it, decoded into Lean types
(the AST boundary in docs/verification-roadmap.md). The kinds come from
`Sqldiff.Syntax`, which is generated from the Rust kinds; a name the schema
lacks, a token where a node belongs or the reverse, and anything that is not
the exact form `sqldiff_core::proof::tree_line` writes, fail to decode.

What is trusted here: that `tree_line` writes a tree injectively, and that
this decoder reads the form back (one tree per text, nothing else accepted).
-/
import Sqldiff.Syntax

namespace Sqldiff.Tree

open Sqldiff.Syntax

/-- A node with its children, or a token with its text in lower-case hex.
Whitespace and comments are left out. -/
inductive Tree where
  | node (k : Kind) (kids : List Tree)
  | tok (k : Kind) (hex : String)
deriving Repr

-- `deriving DecidableEq` does not handle the nested `List Tree`.
mutual
  def Tree.decEq : (a b : Tree) → Decidable (a = b)
    | .node k xs, .node k' ys =>
      if hk : k = k' then
        match Tree.decEqList xs ys with
        | isTrue h => isTrue (by subst hk; subst h; rfl)
        | isFalse h => isFalse (by intro e; cases e; exact h rfl)
      else isFalse (by intro e; cases e; exact hk rfl)
    | .tok k x, .tok k' y =>
      if hk : k = k' then
        if hx : x = y then isTrue (by subst hk; subst hx; rfl)
        else isFalse (by intro e; cases e; exact hx rfl)
      else isFalse (by intro e; cases e; exact hk rfl)
    | .node .., .tok .. => isFalse (by intro e; cases e)
    | .tok .., .node .. => isFalse (by intro e; cases e)

  def Tree.decEqList : (xs ys : List Tree) → Decidable (xs = ys)
    | [], [] => isTrue rfl
    | x :: xs, y :: ys =>
      match Tree.decEq x y, Tree.decEqList xs ys with
      | isTrue h1, isTrue h2 => isTrue (by subst h1; subst h2; rfl)
      | isFalse h, _ => isFalse (by intro e; cases e; exact h rfl)
      | _, isFalse h => isFalse (by intro e; cases e; exact h rfl)
    | [], _ :: _ => isFalse (by intro e; cases e)
    | _ :: _, [] => isFalse (by intro e; cases e)
end

instance : DecidableEq Tree := Tree.decEq

private def isNameChar (c : Char) : Bool := c.isUpper || c.isDigit || c == '_'

private def isHex (c : Char) : Bool := c.isDigit || ('a' ≤ c && c ≤ 'f')

private def name (cs : List Char) : String × List Char :=
  let n := cs.takeWhile isNameChar
  (String.ofList n, cs.drop n.length)

mutual
  /-- `(KIND item ...)` for a node kind. -/
  private partial def node : List Char → Option (Tree × List Char)
    | '(' :: rest =>
      let (n, rest) := name rest
      match Kind.ofName? n with
      | some k =>
        if k.isToken then none
        else match items rest with
          | some (kids, rest) => some (.node k kids, rest)
          | none => none
      | none => none
    | _ => none

  /-- ` item` repeated, then `)`. -/
  private partial def items : List Char → Option (List Tree × List Char)
    | ')' :: rest => some ([], rest)
    | ' ' :: rest =>
      match item rest with
      | some (t, rest) =>
        match items rest with
        | some (ts, rest) => some (t :: ts, rest)
        | none => none
      | none => none
    | _ => none

  /-- A node, or `KIND:hex` for a token kind that is not trivia. -/
  private partial def item : List Char → Option (Tree × List Char)
    | cs@('(' :: _) => node cs
    | cs =>
      let (n, rest) := name cs
      match Kind.ofName? n, rest with
      | some k, ':' :: rest =>
        let h := rest.takeWhile isHex
        if k.isToken && !k.isTrivia && h.length % 2 == 0 then
          some (.tok k (String.ofList h), rest.drop h.length)
        else none
      | _, _ => none
end

/-- The tree a whole text holds, if it is exactly one tree. -/
def decode (s : String) : Option Tree :=
  match node s.toList with
  | some (t, []) => some t
  | _ => none

end Sqldiff.Tree
