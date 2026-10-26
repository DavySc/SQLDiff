# Distributing SQLDiff to colleagues

Colleagues get a folder they can unpack and run, with nothing to install
and nothing to configure. The programs are built in public and stay generic.
Your organisation's config is added on your own machine and goes straight to
colleagues. It never passes through this repository, CI or a release.

```
sqldiff-v0.2.0-windows-x64/      what a colleague receives
    sqldiff-gui.exe              double-click: opens the app in the browser
    sqldiff.exe                  command line
    sqldiff-mcp.exe              MCP server for AI assistants
    README.md                    docs/dist/README.md
    config/                      only in a private bundle (added by `sqldiff dist`)
        config.toml              your user config, paths rewritten
        packs/<name>/pack.toml   every pack it names
        PRIVATE.txt              "do not commit or publish"
```

## 1. Release builds (public, generic)

Push a tag `v*`, or run the **Release** workflow by hand. It builds
`sqldiff`, `sqldiff-gui` and `sqldiff-mcp` for Windows x64 (static C runtime),
macOS arm64 and x64, and Linux x64 (static musl, any distribution). A tag
also creates a **draft** GitHub release with the archives and `SHA256SUMS.txt`.
Check it, then publish it.

Before anything is built, the workflow runs the hashed leak scan over the
tree with the deny-list required ([private-overlay.md](private-overlay.md)).
Each archive is staged by `sqldiff dist --no-config`, and a check then fails
the build unless the archive holds exactly the three programs and the
README. A config file, a `config/` folder or anything else fails the build.

## 2. Add your private config (on your machine)

Download the archive for each platform your colleagues use, unpack it
**outside any git checkout**, and run its own `sqldiff`:

```sh
./sqldiff dist --user-config /path/to/your/config.toml -o ~/Desktop/sqldiff-for-team
```

To build a Windows bundle on macOS or Linux, pass the unpacked Windows
folder as `--from`, the folder that holds the executables to copy:

```sh
./sqldiff dist --from ~/Downloads/sqldiff-v0.2.0-windows-x64 \
  --user-config /path/to/your/config.toml -o ~/Desktop/sqldiff-for-team-windows
```

Without `--user-config`, `dist` bundles the user config it would load itself:
`$SQLDIFF_CONFIG`, then a bundled `config/`, then the platform config file.
`--pack PATH` adds further packs as private packs.

What `dist` does:

- **Loads and validates** the config and every pack first, with the same
  checks as every other command (secret-looking keys, credential references,
  unknown keys). Nothing is written when they fail.
- **Copies each pack** in `packs` and `private_packs` to
  `config/packs/<name>/pack.toml` and rewrites the paths to point there.
  Only `pack.toml` files are copied, never the folders around them, so notes,
  corpora and snapshots in your private overlay stay behind.
- **Leaves out what only fits your machine:** `data_dir`, a `[source]` that
  names a folder, repository or bundle (an adapter source is kept), and a
  `data_key` of the form `file:PATH`. It prints a note for each one, and for
  every adapter whose command is a path that must also exist on the
  colleague's machine.
- **Checks the result:** the bundled `config.toml` must load to the same
  packs (by content hash) and the same settings as the source. Otherwise
  `dist` fails.
- **Refuses** an output folder inside any git work tree, and one that is not
  empty.

TOML comments in your config are not carried over, because the file is
rewritten. Zip the output folder and hand it over directly, for example by
mail or on a team share. Do not upload it anywhere public.

## How the programs find the config

User config, first match wins (see [packs-and-config.md](packs-and-config.md)):

1. `--user-config FILE`
2. `$SQLDIFF_CONFIG`
3. `config/config.toml` beside the executable (symlinks are followed to the real install)
4. `%APPDATA%\sqldiff\config.toml`, else `~/.config/sqldiff/config.toml`

Only one user config is loaded. With a bundle present, a colleague's own
`~/.config/sqldiff/config.toml` (or `%APPDATA%` file) is read only when
`$SQLDIFF_CONFIG` points at it, and then the bundle is not read.
`--isolated` skips all of these. `sqldiff config` lists the loaded file and
packs under `layers`. Review decisions and the cache stay in each user's own
data directory, never in the distribution folder.

## Keeping the config out of the repository

- The bundle is only ever written outside git work trees, and release
  archives are built without one and checked.
- `.gitignore` lists `/config/` and `/dist/`, so a bundle copied into the
  clone by mistake does not show up as untracked work.
- The commit and push hooks and the CI leak scan keep checking for the
  deny-listed terms of your packs
  ([private-overlay.md](private-overlay.md)).
- Credentials never go in a config or a pack. Name a reference such as
  `env:IBMI_PASSWORD`. Each colleague sets the variable themselves.
