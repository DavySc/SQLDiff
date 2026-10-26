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
