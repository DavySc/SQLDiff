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
- A `[claim]` draft is not ready for review: reviewers leave it alone until
  it is marked ready.

## Accuracy rules every change keeps

- **Never loosen the gate.** A statement the engine never compared does not
  count as converted, and a wrong "equal" is worse than "not proven". Do not
  trade correctness for speed or coverage without the owner's explicit OK.
- **No new assumption without the owner's OK.** A new `ASM-*` id, or a wider
  reading of an existing one, needs the owner's explicit approval before it
  merges. Register it in `crates/sqldiff-core/profiles/assumptions.toml` and
  regenerate `docs/assumptions.md` with
  `SQLDIFF_BLESS=1 cargo test -p sqldiff-core --test assumptions`. An
  assumption the owner withdrew is not proposed again without new evidence.
- **Rule ids are registered.** Adding or renaming a converter rule id means
  updating `crates/sqldiff-core/profiles/rule-proofs.toml` and regenerating
  `docs/proof-coverage.md` with
  `SQLDIFF_BLESS=1 cargo test -p sqldiff-core --test rule_proofs`.
- **Db2 means Db2 for i.** Nothing for Db2 for z/OS or Db2 LUW enters the
  code, and their documentation is not evidence for Db2 for i behaviour.
  Vendor facts get the next free `V<n>` in docs/vendor-facts-verification.md;
  name it in your claim so two sessions do not take the same number.
- **Private configuration stays out.** Packs and configs marked private never
  go into the repository, CI or GitHub (docs/private-overlay.md).
- **A test must observe what it proves.** An engine test whose assigned
  variable is never read proves nothing; add an effect (a write, a SELECT)
  after the statement under test.

## Tests and builds

- CI also runs on Windows with `core.autocrlf=true`. A test that reads a
  file from the repository (fixtures, corpus, assets) must pass with CRLF
  line endings: normalize them, or pin byte-compared files to LF in
  `.gitattributes`.
- The disk is small. Run tests with `TMPDIR` pointing at a directory in your
  scratchpad (e.g. `TMPDIR=<scratchpad>/tt`), and delete `target/debug` when
  the disk fills up. Rebuild after switching branches before trusting a
  result.

## Merging

A PR merges once a session that did not write it has reviewed it and CI is
green; nothing waits on the owner. Authors do not merge their own PRs.
