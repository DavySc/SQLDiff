# Editor integration

Both commands read a buffer from standard input when the file is `-`;
`--stdin-filepath PATH` names the file the buffer is (dialect, config lookup,
path shown). Input must be UTF-8; CRLF passes through unchanged.

```sh
sqldiff fmt   - --stdin-filepath db/proc.tsql.sql < buffer      # formatted text on stdout
sqldiff check - --stdin-filepath db/proc.tsql.sql -f json < buffer
```

`check -f json` prints one report. For diagnostics read:

- `findings[]`: `rule`, `severity`, `witness` (the message) and `a` with
  `start_line`, `start_col`, `end_line`, `end_col` (1-based; columns count
  **bytes**; the end is exclusive) and `byte_start`, `byte_end`.
- `diagnostics[]`: parse problems with `line`, `col`, `message`.

Exit code: 0 equal, 1 findings, 2 error. For Neovim subtract 1 from the
line and column numbers (its columns are 0-based bytes too).
