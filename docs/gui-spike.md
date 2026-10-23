# M1 GUI spike: pair view

Design §14 M1 asks for a two-pane view with per-token styling and synchronised scroll that re-renders on edit, and for the GUI stack to be chosen and written down (§15 item 5). This records what the spike built and what it decided.

## What it does

`sqldiff-gui` (crate `crates/sqldiff-gui`) opens one pair in a browser:

```sh
cargo build --release
G=target/release/sqldiff-gui

# Live: local server on 127.0.0.1:7878, scratchpad edits re-diff as you type
$G serve tests/fixtures/movestock.tsql.sql tests/fixtures/movestock.db2i.sql --dialect-a tsql --dialect-b db2i

# From a CLI report: paths and dialects are taken from `sqldiff diff -f json`
sqldiff diff a.sql b.sql --dialect-a tsql --dialect-b postgres -f json > report.json
$G serve --report report.json

# Static, read-only HTML file to attach or share (no server needed)
$G export --report report.json -o pair.html
```

- **Aligned by statement, not line.** Each side is cut into blocks of whole lines around statements (short compound statements such as a one-line `IF … BEGIN … END` stay one block; lines outside statements, such as routine headers, form their own blocks). Blocks are aligned globally by token similarity with a cross-dialect key (case folded, quotes, `@` sigils, `p_`/`v_` prefixes and underscores ignored), so `UPDATE … @Qty` pairs with `UPDATE … p_Qty`. A pure reformat is all "same" rows and folds away.
- **Synchronised scroll** for free: both sides are cells of the same rows in one scroll container.
- **Per-token styling** from the lossless CST tokens (keyword, identifier, variable, literal, comment, error), plus change marks: changed tokens inside a paired row, rows present on one side only, and a dotted underline where tokens pair only through the cross-dialect key.
- **Findings panel** with class, severity, verdict, witness, assumption ids (hover for text) and fingerprint. Rows carrying a finding get a coloured gutter; clicking either jumps to the other.
- **Keyboard:** `j`/`k` next/previous finding, `]`/`[` next/previous changed row, `c` collapse unchanged, `e` scratchpad, `y` copy finding as text, `?` help.
- **Deep links:** `#f=<fingerprint>` opens that finding (for `open_in_gui`).
- **Re-render on edit:** the scratchpad posts both texts to `/api/diff`; the server re-runs the engine and returns a new payload. Sources on disk are never written. Fingerprints are stable, so the focused finding survives edits. A browser refresh re-reads the files.

The page consumes exactly the `sqldiff diff -f json` report, plus a `view` (rows, blocks, token segments) and the two source texts. All analysis and alignment runs in Rust; the page only renders.

## Decision

**Rust core + embedded local HTTP server + a single dependency-free HTML/JS page, embedded in the binary.**

- No JS toolchain, no npm, no new Rust dependencies (the server is ~100 lines of `std::net`), so it builds wherever the CLI builds and CI stays one `cargo test`.
- The same page works as a static export, which covers "send this review to someone".
- For desktop packaging later, the page drops into a Tauri or similar webview shell unchanged, since it talks to the core only through one JSON payload and one endpoint.

What the spike did *not* settle, and the trigger to revisit:

- **Large files.** Rendering is plain DOM, one element per token. Fine for routines of a few thousand lines; for very large files, swap the code cells for a virtualised code view (CodeMirror 6 is the likely choice) behind the same payload.
- **In-place editing.** The scratchpad is a pair of textareas above the view. Editing inside the panes would need a real editor component (same trigger as above).
- **Incremental re-diff.** Each edit re-runs the full diff; it is fast enough on routine-sized inputs, and the design's incremental re-diff can come with the IR (M4).
- **Review queue, lens switch, transaction overlay, "what if", assumption flips, review store.** Out of scope for M1; the payload has room for them.
