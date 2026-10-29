# Working on SQLDiff with Claude Code

Accuracy beats speed in this project: a wrong "equal" or a silently wrong
conversion is the worst outcome (README, docs/design.md). Nothing below trades
correctness for a smaller context.

## Delegate searching and gate runs to subagents

Keep the main context small by delegating to the subagents in `.claude/agents/`:

- **search**: where something lives (crate, module, test, fixture, doc).
- **grep**: every call site or reference of a symbol, before changing it.
- **audit**: commit first, then hand it the SHA; it runs the CI checks and a
  hygiene pass in its own worktree and reports raw results.

Their output is a set of leads, not proof. Read the code you change yourself,
and read a test failure's full output yourself before deciding what it means.
An audit PASS is not a review: semantic correctness is judged in the main
session and by the cross-review on the PR.

## Parallel sessions: an open PR is a claim

Several sessions work on this repository at once (roadmap items on `main`,
the verified-conversion phases of docs/verified-conversion.md). They
coordinate through GitHub, which every session can read:

- **Before starting an item**, list the open pull requests, drafts included
  (`gh pr list --state open`, or the GitHub tools' `list_pull_requests`).
  Each open PR claims its item and the areas its body names under
  **Touches**.
- **To claim an item**, branch off `main`, push, and open a **draft** PR at
  once, before writing code. Title it `[claim] <item>` and give the body
  four lines: **Item** (the roadmap or phase entry), **Touches** (crates,
  modules or files you expect to change), **Owner** (the session's link),
  **Status** (one line, kept current). Widen Touches as soon as you know
  you will.
- **Do not start an item or change an area another open PR claims.** If you
  must, comment on that PR first and wait for an answer.
- **When done**, mark the PR ready for review and drop `[claim]` from the
  title. If you give the item up, close the draft with a line saying so.
- Fixes that span claims (a false "equal" found in someone else's area) go
  in their own small PR naming the claim they touch.
