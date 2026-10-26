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

The app opens in convert mode: type or paste T-SQL on the left, checked
PostgreSQL or Db2 for i appears on the right as you type, with a badge on each
statement. Switch to Compare to diff two texts. To compare files or folders
directly:

```sh
sqldiff-gui serve old.sql new.sql            # two versions of one object
sqldiff-gui serve tsql/ db2i/                # two trees as a review queue
```

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
