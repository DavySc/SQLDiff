/-
Packaging and performance spike for `sqldiff-check` (decision 2 in
docs/verification-roadmap.md). Reads one request per line on stdin and
answers one line per request. Every answer is "unconfirmed": the spike
proves nothing, and nothing it can print reads as confirmed.
-/

partial def serve (inp out : IO.FS.Stream) (n : Nat) : IO Nat := do
  let line ← inp.getLine
  if line.isEmpty then
    return n
  out.putStrLn s!"\{\"proof\":\"unconfirmed\",\"reason\":\"spike\",\"request\":{n}}"
  out.flush
  serve inp out (n + 1)

def main (args : List String) : IO UInt32 := do
  if args == ["--version"] then
    IO.println "sqldiff-check spike 0.0.1"
    return 0
  let n ← serve (← IO.getStdin) (← IO.getStdout) 0
  IO.eprintln s!"{n} requests"
  return 0
