# Keeping private information out of the repository

This repository is public-safe: it contains the generic core and nothing
specific to any organisation or platform. This guide is for anyone developing
the application, and explains how to do that work without private information
ever reaching a commit, a pull request, an issue, a log or the remote.

The design reason is section 5 of [design.md](design.md): organisation-specific
knowledge is **configuration supplied at run time**, never code, fixtures or
documentation in this repository.

## The rule

> If it would not be safe to publish, it does not go in this repository, its
> history, its issues, its pull requests, its CI logs or its commit messages.

"Private" includes anything that identifies an organisation, a product or a
person, or reveals how an internal system works: names, schemas, object and
table names, ticket and work-item numbers, internal URLs, hostnames, account
names, e-mail addresses and domains, credentials, counts and statistics from a
real corpus, and real code or data, even slightly edited.

## Where private things live

Keep a separate **private overlay** next to the clone, never inside it:

```
<parent>/SQLDiff/            this repository (pushed)
<parent>/SQLDiff-private/    the overlay (never pushed, or pushed only to its own private remote)
    packs/<name>/pack.toml   private packs: conventions, markers, helpers, limits, deny-list
    adapters/                private adapter programs (they talk to private systems)
    notes/                   write-ups, findings, meeting notes
    snapshots/               captured bundles of private sources
    corpus/                  real inputs used for local testing
    data/                    cache and review store (data_dir)
<user config dir>/sqldiff/config.toml
                             names the private packs; also outside the clone
                             (Windows: %APPDATA%\sqldiff, else ~/.config/sqldiff)
```

Outside the work tree is deliberate. A folder that is merely git-ignored inside
the clone can still be staged by `git add -f`, copied by a tool, or picked up by
a glob. A folder elsewhere cannot.

To hand the app with your private config to colleagues, build the bundle
with `sqldiff dist` outside the clone; see [distribution.md](distribution.md).

## One-time setup

```sh
cargo build -p sqldiff-cli
scripts/install-hooks.sh --strict
git config --local user.name  "Your Name"
git config --local user.email "<public or noreply address>"

mkdir -p ../SQLDiff-private/packs/org
# write ../SQLDiff-private/packs/org/pack.toml  (start from docs/examples/pack/pack.toml)
# add to the user config:  private_packs = ["<absolute path>/SQLDiff-private/packs/org"]

sqldiff pack validate ../SQLDiff-private/packs/org
scripts/install-hooks.sh --check      # must end with "protection in place"
```

What each step does:

- **Hooks** scan every commit, commit message and push against the deny-list.
  `--strict` makes a missing deny-list an error instead of a silent skip.
- **Local identity** stops a work address from being written into commits. The
  global git identity is easy to forget about.
- **The pack's `deny_list`** names the terms that must never appear in this
  repository: product and environment names, internal entity and object names,
  people and account names, company domains, ticket numbers. The list lives only
  in the private pack.

Run `scripts/install-hooks.sh --check` again after cloning, moving the folder,
or changing tools. Git ignores a hooks folder that does not exist, so protection
can disappear without any warning.

## Day-to-day rules

### Code
- Write the core generically. If a feature needs an organisation's convention
  (a naming rule, a marker comment, a placeholder syntax, a helper function, a
  limit), implement the **mechanism** in the core and put the **values** in a
  pack. Rule of thumb: the core asks "what is the placeholder syntax?", the pack
  answers.
- Never hard-code an identifier, URL, path, schema or credential. Put it in
  config and read it at run time.
- Never put real names in code comments, test names, variable names or error
  messages.
- Credentials never go in a pack or config file. Name a credential *reference*
  (`env:NAME`, `keychain:SERVICE/ACCOUNT`, `vault:PATH`); the adapter resolves it.

### Tests and fixtures
- Fixtures are **synthetic**: written for this repository, or public-domain
  with a recorded origin and licence. Record each one in
  `tests/fixtures/MANIFEST.md`.
- Never paste real code into a fixture, even trimmed, renamed or "anonymised".
  Structure, comments and unusual constructs identify the origin.
- When a real input exposes a bug, **reduce it to a synthetic reproduction**:
  invent names, keep only the construct that triggers the bug, and check that
  the reproduction still fails the same way. Commit that, not the original.
- Keep real inputs in `../SQLDiff-private/corpus/` and run the tool over them
  locally. Only abstract findings come back (next section).

### Learning from private corpora
What may come back into the repository: an **abstract bug class** ("a column
removed on one side is still referenced on the other"), a **synthetic
example**, a new **lint or equivalence rule** written generically, a design
improvement. What may not: counts, rankings or statistics from the corpus,
names, quotes from change descriptions, dates and ticket numbers, or anything
that tells a reader which organisation or system it came from.

### Commits, branches, pull requests and issues
- Commit messages describe the generic change. No ticket numbers, no real
  names, no "fixes the thing in <internal system>".
- The same applies to branch names, pull request titles and descriptions, review
  comments, issue text, release notes and CI configuration.
- Never attach or paste logs, screenshots, stack traces or SARIF/JSON output
  from private inputs. Run the tool with the redacted export, or reproduce on
  synthetic input first.
- If you need a private reference to remember why something was done, put it in
  `../SQLDiff-private/notes/`.

### Logs, output and tooling
- Source text is not logged unless a debug flag is set. Do not enable it on
  private inputs and then share the output.
- Redacted export replaces pack-derived identifiers with stable placeholders.
  Use it for anything that leaves your machine.
- Be careful with editor and IDE history, terminal scrollback, crash reports and
  AI assistant transcripts: they can hold private text even when the repository
  does not. Keep private sessions and public sessions separate, and use the
  private overlay (not the repository) as the working directory for private work.
- Do not copy private documents into `docs/`. Design documents in this
  repository are generic; internal write-ups stay in the overlay.

## Before you commit and push

1. `git status` shows only files you meant to change, none from the overlay.
2. The hooks ran. A commit that prints nothing from the hooks may mean they are
   not installed: run `scripts/install-hooks.sh --check`.
3. Skim the diff for things the deny-list cannot know: unusual identifiers,
   comments that explain "why" with a reference to a specific system, numbers
   that look like real statistics.
4. Read the commit message and the pull request description as if a stranger
   were reading them.
5. Push only to the intended remote. A new branch is scanned against everything
   that remote does not have yet.

## Keeping the deny-list useful

The hooks match only the terms they are given. Whenever a new private name
appears in your work (a system, a person, an environment, a ticket series, a
domain), add it to the pack's `deny_list` the same day. Prefer specific terms;
a term that is also an ordinary word matches innocent text and teaches people to
ignore the hooks (a word that matched the design document's own prose had to be
replaced this way).

Check the list against the repository whenever you change it:

```sh
SQLDIFF_DENYLIST="$(sqldiff pack deny-list --isolated --user-config <config>)" scripts/leak-scan.sh
```

### CI gets hashes, not terms

CI scans the tree and every commit of a push or pull request, but it never sees
the terms. Store salted SHA-256 hashes of them as the repository secret
`SQLDIFF_DENYHASHES`:

```sh
sqldiff pack deny-hashes --isolated --user-config <config> | gh secret set SQLDIFF_DENYHASHES
```

`sqldiff pack scan` hashes every substring of each listed length and reports
the file and line of a match, never the term. Matching is the same as the
hooks': anywhere in the text, ignoring ASCII case. Re-run the command whenever
the deny-list changes; each run uses a fresh salt.

The hashes are not a reason to publish the list. A short term (a ticket number,
a common surname) can be found by hashing guesses, so the hashes stay in the
secret. What they buy is that the plain terms never leave your machine.

On pushes and on pull requests from this repository a missing secret fails the
scan. Pull requests from forks get no secrets, so there the scan is skipped.

## What the protection cannot do

- **It runs locally.** `git commit --no-verify` and `git push --no-verify` skip
  the hooks, and a fresh clone has none until `scripts/install-hooks.sh` runs.
  Do not use the flags; CI is the backstop for the second case.
- **It matches only the listed terms.** A sensitive term that is not on the list
  passes. It is a safety net behind careful habits, not a substitute.
- **It scans text.** It does not understand meaning, so an abstract description
  of a private mechanism needs a human judgement.
- **It cannot unpublish.** Anything already pushed may be cached, forked or
  fetched.

## Checking the protection itself

`scripts/test-hooks.sh` builds a throwaway repository and a local bare "remote"
with synthetic terms and verifies that each protection fires: staged content,
commit messages, identities, a bypassed commit at push time, a leak buried in
history, a clean push, strict mode without a deny-list, and the same cases for
the hashed list CI uses (including that a failing scanner never reports
clean). CI runs it, and you
can run it after changing any script in `scripts/`.

## If something private does get pushed

1. **Stop and tell the owner** of the information. They decide what the impact
   is.
2. **Treat it as exposed.** Deleting the commit or rewriting history does not
   remove copies that were already fetched, forked, cached or captured in CI
   logs.
3. **Rotate anything that is a credential** straight away.
4. **Then clean up:** remove the content from every branch and from history,
   force-push with care, ask the host to purge cached views and forks where
   possible, and re-run the leak scan across all refs.
5. **Add the term to the deny-list**, and ask why the hooks did not catch it
   (not installed, term missing, flag used, different clone) so that gap closes.
