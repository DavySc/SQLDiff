# Pull request review

`sqldiff changes` reviews the SQL a branch changed, the way a pull request
shows it, and the GitHub Action in [`action.yml`](../action.yml) runs it on
every pull request. Reviewers rely on it, so it never drops a changed file
silently: every changed SQL file is either compared or listed with the reason
it was not.

```sh
sqldiff changes --base origin/main                 # HEAD against its merge base with main
sqldiff changes --base main --head feature -f json
sqldiff changes --base main --sarif out.sarif --markdown comment.md -- db/procs
```

## What is compared

The change set runs from the **merge base** of `--base` and `--head` to
`--head` (`git diff BASE...HEAD`): what the branch changed, not what the
target branch did since. Only changed files are read; the rest of the tree is
never opened, so a review takes as long as the change, not the repository.

A file is SQL when it ends in `.sql` or `dialect_map` names it. Its dialect
comes from `--dialect`, else `dialect_map`, else the path convention
(`tsql/x.sql`, `x.pg.sql`). Text is UTF-8 (a BOM is kept) or UTF-16 with a
BOM, as SQL Server tools save it.

Base and head files are paired, in order:

| Pairing | When |
|---|---|
| Path | The file has the same path at both revisions. Two `Cities.sql` in different schema folders are two pairs. |
| Rename | Git's rename detection (50% similar or more) matched a removed and an added file. |
| Same object | A removed and an added file each define exactly one object, the same one: `relation:schema.name` (tables and views) or `routine:schema.name` (procedures and functions), case folded. Only when exactly one file on each side defines it; otherwise all of them are listed as ambiguous. |

Each pair is diffed base (A) against head (B) with the same engine, packs,
review store and markers as `sqldiff diff`. An added file has nothing to be
compared with: it is checked on its own (lints, binding) and counts as
`unknown`.

## Verdict and exit code

The overall verdict is the worst over the pairs. It is at least `unknown`
when anything was not compared:

| Listed as | Meaning |
|---|---|
| added | A new file; checked, not compared |
| removed | No head file defines its object |
| ambiguous | Several removed or added files define the same object |
| no dialect | Map it with `dialect_map` or pass `--dialect` |
| unreadable | Not UTF-8 or UTF-16 text |
| config changed | The change edits `sqldiff.toml`, a `pack.toml` or a loaded config or pack file. Results use the config in the working directory, which is the head's in CI, so a config change could hide findings. A reviewer has to read it. |

Files the config's `ignore` excludes are listed as ignored and do not count.

`--fail-on` sets the exit code: `unknown` (default: exit 1 unless every pair
is equal or equal under assumptions and nothing was left uncompared),
`different` (exit 1 only for proven differences) or `never`. Exit 2 is an
error. The default is the strict one on purpose: a passing check means
SQLDiff showed every change keeps behaviour, never that it did not look.

## SARIF

`--sarif FILE` writes SARIF 2.1.0 where **every result has exactly one
location, in a file and on lines that exist at the head commit**. Code
scanning drops results without one, and a pull request can only show
annotations on the head side.

- A finding with a head-side location keeps it.
- A finding located only at the base (a removed statement) is placed with a
  line diff of the two texts: at the head line now in its place ("Base line 47
  was removed just above this line."), or at the replacement ("was
  replaced here"). The base location stays as a related location.
- A finding about the whole object points at its first line of code.
- An added file's findings keep their own locations (it is side A of its
  check).
- A removed file has no head location; it is in the Markdown, the JSON and
  the exit code, not in the SARIF.

The other commands (`diff`, `tree`, `check`) also give every result a
location now: the file's first line when the finding has none.

## The pull request comment

`--markdown FILE` writes the summary comment: the overall verdict, a table of
every changed file with its verdict and change, the findings in the phrase
grammar ([phrase.rs](../crates/sqldiff-core/src/phrase.rs); A is the base, B
the head), why each unproven file is unproven, and everything not compared.
It starts with `<!-- sqldiff:changes -->`, so the Action updates its comment
instead of adding one per push. A comment that would exceed GitHub's size
limit says how many lines it leaves out; the SARIF and the job log have all
of them.

`--json FILE` writes the full reports.

## GitHub Action

```yaml
name: SQL review
on: pull_request
permissions:
  contents: read
  security-events: write   # SARIF upload
  pull-requests: write     # summary comment
jobs:
  sqldiff:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0       # base, head and their merge base
      - uses: DavySc/SQLDiff@main
        with:
          args: --pack packs/ours
          # dialect: tsql
          # paths: |
          #   db/
          # fail-on: different
```

The Action builds `sqldiff` from its own source (cached by source hash; pass
`sqldiff:` to use a prebuilt binary), runs `sqldiff changes` from the pull
request's base commit to its head commit, writes the summary to the job
summary, uploads the SARIF for `refs/pull/N/head` at the head commit (the
lines it was computed on), and creates or updates the comment. On a pull
request from a fork the token is read-only: the comment step warns and the
job summary still has it. Inputs and outputs are listed in `action.yml`.
[`.github/workflows/action-selftest.yml`](../.github/workflows/action-selftest.yml)
runs the Action on a scratch repository in CI.

Code scanning needs GitHub Advanced Security on private repositories; set
`upload-sarif: false` without it.

## Pairing in `sqldiff tree`

`tree` pairs two directories by file name (pack pairing rules). When several
files on a side share a key (a table and a view both named `Cities.sql` in
different schema folders), it now pairs them by the one schema-qualified
object each defines, where that is unique on both sides. The rest stay
ambiguous, or unpaired when one is left.
