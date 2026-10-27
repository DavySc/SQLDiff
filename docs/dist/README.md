# SQLDiff

A semantic diff and conversion checker for T-SQL (SQL Server), PostgreSQL and
Db2 for i. It tells you what changed between two SQL artifacts, whether the
meaning changed, and gives a concrete scenario for every behavioural
difference. When equivalence cannot be shown, it says `unknown`; it never
reports a false "equal".

## What is in this folder

| File | What it is |
|---|---|
| `sqldiff-gui` (`.exe` on Windows) | The app: opens in your browser |
| `sqldiff` | The command line: diff, check, convert, review a branch |
| `sqldiff-mcp` | The same checks for AI assistants (MCP server on stdio) |
| `config/` | Only when you were sent a private setup: your team's config and packs |

Nothing is installed. Everything runs on your own machine, and no SQL leaves it.

## Start the app

Double-click `sqldiff-gui` (or run it from a terminal). It starts a local
server on `127.0.0.1` and opens your browser at it. Keep the window that
appears open while you work; closing it stops SQLDiff.

The app opens on Home. To compare two files or two folders, pick them with
Browse (or type their paths) and press Compare; the dialect of each side comes
from the path convention unless you choose one. Comparisons you opened before
are listed under Recent. Convert opens convert mode: type or paste T-SQL on the
left, checked PostgreSQL or Db2 for i appears on the right as you type, with a
badge on each statement.

To convert a whole tree, open Runs (or Convert a tree on Home): pick the source
folder and an empty or new output folder, choose the dialects and press
Convert. The run shows how many files are checked and can be cancelled; a
cancelled run writes nothing into the output folder. When it finishes, the run
lists how many units landed on each status and Review opens what it wrote. A
run writes exactly what `sqldiff convert src/ out/ --from … --to …` writes,
with the project config found from the source folder upward. Each run's Command line shows that
command for PowerShell and for sh, ready to copy into a script or pipeline,
with the config files it loaded by path. Runs are listed until the app is
closed; the output folder and its review ledger stay.

The rail on the left switches between Home, Runs, Compare, Convert and Review
in the same window. From a terminal the app can also start on files, folders or a
converted tree:

```sh
sqldiff-gui serve old.sql new.sql            # two versions of one object
sqldiff-gui serve tsql/ db2i/                # two trees as a review queue
sqldiff-gui review src/ out/ --to postgres   # review what `sqldiff convert` wrote
```

A comparison opened from Home reads the project config (`sqldiff.toml`) found
from side A's folder upward, plus your user config and the private config in
`config/`, as a command run in that folder would. The Recent list keeps paths
only, in SQLDiff's data folder; it is not kept when the data folder is
encrypted.

### First run on macOS

The programs are not notarised, so macOS blocks them the first time. In
Terminal, inside this folder, run once:

```sh
xattr -dr com.apple.quarantine .
```

### First run on Windows

SmartScreen may say "Windows protected your PC". Choose **More info**, then
**Run anyway**. It asks once per program.

## Command line

```sh
sqldiff diff a.sql b.sql --dialect-a tsql --dialect-b db2i   # conversion review
sqldiff diff v1.sql v2.sql -d postgres                       # version review
sqldiff check proc.sql -d tsql                               # lint one file
sqldiff convert src/tsql -o out/pg --to postgres             # convert a whole tree
sqldiff config                                               # which config is loaded
sqldiff --help
```

Exit codes: `0` equal or equal under assumptions, `1` different or unknown,
`2` usage or I/O error.

On macOS and Linux, run the programs as `./sqldiff` from this folder, or add
the folder to your `PATH`.

## Your team's config

If you were sent a `config/` folder, it sits next to the programs and is
loaded automatically: pairing rules, naming conventions, template syntax,
limits and connections that are specific to your organisation.
`sqldiff config` shows it in the `layers` list.

- It is private. Do not commit it to a repository, attach it to a ticket or
  share it outside the team.
- It holds no passwords. Where a connection needs one, the config names an
  environment variable (for example `env:IBMI_PASSWORD`) that you set yourself.
- To update the programs, replace them and keep `config/`. To update the
  config, replace `config/` with the new one.
- `SQLDIFF_CONFIG=/path/to/config.toml` uses another config instead;
  `--isolated` runs without any.

Your review decisions and cache are stored per user, not in this folder
(`%LOCALAPPDATA%\sqldiff` on Windows, `~/.local/share/sqldiff` elsewhere).
