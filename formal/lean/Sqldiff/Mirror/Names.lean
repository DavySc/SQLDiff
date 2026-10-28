/-
Small executable checks on names, shared by the lowerings and the check.
-/

namespace Sqldiff.Mirror

/-- No name occurs twice. -/
def distinct : List String → Bool
  | [] => true
  | x :: xs => !xs.contains x && distinct xs

/-- Every name is ASCII, and two names that differ are not equal up to
ASCII case. -/
def caseSafe (xs : List String) : Bool :=
  xs.all (fun x => x.toList.all fun (c : Char) => c.toNat < 128) &&
    xs.all fun x => xs.all fun y => x == y || x.toLower != y.toLower

/-- The value of key `x` in an association list, first entry winning. -/
def lookup {α : Type} (m : List (String × α)) (x : String) : Option α :=
  (m.find? (·.1 == x)).map (·.2)

end Sqldiff.Mirror
