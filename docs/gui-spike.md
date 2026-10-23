# M1 GUI spike: pair view

Design §14 M1 asks for a two-pane view with per-token styling and synchronised scroll that re-renders on edit, and for the GUI stack to be chosen and written down (§15 item 5). This records what the spike built and what it decided.

## What it does

`sqldiff-gui` (crate `crates/sqldiff-gui`) opens one pair in a browser:

```sh
cargo build --release
G=target/release/sqldiff-gui

# Live: local server on 127.0.0.1:7878, edits in the panes re-diff as you type
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
- **Keyboard:** `j`/`k` next/previous finding, `]`/`[` next/previous changed row, `c` collapse unchanged, `y` copy finding as text, `?` help.
- **Deep links:** `#f=<fingerprint>` opens that finding (for `open_in_gui`).
- **Inline editing:** every code cell in the pair view is `contenteditable="plaintext-only"` in live mode (not in a static export). An edit splices the cell text into that side's source (by the block's line range) and, 250 ms after the last keystroke, posts both texts to `/api/diff`; the server re-runs the engine and returns a new payload. Sources on disk are never written. Fingerprints are stable, so the focused finding survives edits. A browser refresh re-reads the files.

The page consumes exactly the `sqldiff diff -f json` report, plus a `view` (rows, blocks, token segments) and the two source texts. All analysis and alignment runs in Rust; the page only renders.

## Decision

**Rust core + embedded local HTTP server + a single dependency-free HTML/JS page, embedded in the binary.**

- No JS toolchain, no npm, no new Rust dependencies (the server is ~100 lines of `std::net`), so it builds wherever the CLI builds and CI stays one `cargo test`.
- The same page works as a static export, which covers "send this review to someone".
- For desktop packaging later, the page drops into a Tauri or similar webview shell unchanged, since it talks to the core only through one JSON payload and one endpoint.

What the spike did *not* settle, and the trigger to revisit:

- **Large files.** Rendering is plain DOM, one element per token. Fine for routines of a few thousand lines; for very large files, swap the code cells for a virtualised code view (CodeMirror 6 is the likely choice) behind the same payload.
- **Inline editing limits.** Editing is per code cell on plain DOM. Cells with no counterpart on the other side (hatched) and rows folded by "Collapse unchanged" cannot be typed into; add a statement by pressing Enter at the end of a neighbouring line. A real editor component (CodeMirror, same trigger as above) would remove these limits.
- **Incremental re-diff.** Each edit re-runs the full diff; it is fast enough on routine-sized inputs, and the design's incremental re-diff can come with the IR (M4).
- **Review queue, lens switch, transaction overlay, "what if", assumption flips, review store.** Out of scope for M1; the payload has room for them.

## Notes for agents picking this up

What changed after the M1 spike, and why (all in `crates/sqldiff-gui/assets/index.html` unless noted):

- **Live editing was broken.** On startup the page set `state.data` directly, so `openPair` skipped `loadPair`, the only place that filled the editor textareas. Both stayed empty. `syncEditors()` now runs for every freshly loaded pair, including the first.
- **The scratchpad was replaced by inline editing.** Users expected to type in the diff panes, not in a hidden box. The `#scratch` textareas, the Edit button and the `e` key are gone. The source texts live in `state.src` (normalised to LF); `onCodeInput` edits them, `scheduleDiff` re-diffs, and `captureCaret`/`restoreCaret` keep the caret in place across the re-render by tracking an absolute character offset in the side's source. Pane scroll position is kept too. **Reset edits** (toolbar, palette) calls `loadPair(cur, false)`.
- **Decluttered.** Filter chips, Hide accepted and Collapse unchanged moved to a toolbar row. Config and hash details sit behind **Details** (`body.show-info`). Finding cards are compact: only the focused card shows meta and the Accept/Reject/Comment buttons, and others clamp the witness to three lines.
- **Clean redesign** (after Davy found the GUI cluttered). Principles borrowed from Linear, Zed/VS Code and GitHub's PR diff: calm defaults, one line of chrome at the top, progressive disclosure, keyboard first.
  - Header is one line: breadcrumb (Queue / pair name with ‹ ›), verdict as a coloured dot plus text, a "Search or run a command" bar (Ctrl K) and an ⓘ button. The ⓘ button (or `i`) opens the provenance popover (`#provbar`, `body.show-info`); the old Details button had no handler.
  - Secondary toggles (Collapse unchanged, Hide accepted, Compact density, live-editing note and Reset) live in a status bar (`footer`) with per-view key hints. Density (`d`) is remembered in localStorage.
  - Findings are a list, not cards. The class filter chips sit at the top of the list and only show with two or more classes. Only the focused finding shows meta and actions (and a finding being commented on keeps its draft visible); others clamp to two lines. Accept is the one outlined button.
  - Findings panel works as a triage list: open findings first, a progress bar ("1/3 reviewed"), and accepted or rejected ones folded into a **Reviewed** section (`orderedFindings()`, `isDone()`; a comment alone keeps a finding open). Accept or reject on the focused finding moves focus to the next open one, Linear-style. Each item shows its short location (`B 19`) on the right. The focused item spells out its assumptions as plain sentences ("Holds only if"). Rule, fingerprint and Copy sit on one faint line. Hovering an item outlines its rows in the diff. When everything is reviewed, a "Next pair" button appears.
  - Assumptions, Why not stronger, Normalizations and Parse diagnostics are `<details>` sections (`section()`/`wireSections()`); open state lives in `state.open`, assumptions start open.
  - Diff pane: tinted line-number gutters with a lighter code tint, no bold keywords, no ligatures in code. A block whose tokens are more than half marked drops the word highlights (`.cell.dense`), as GitHub does, because the row tint already says it all.
  - Queue: a search box (`/`) and one segmented control (All / Needs review / Reviewed / Clean / Errors); class, dialect, folder and sort sit behind Filters. Rows show the pair's shared file stem; the folder line only shows when pairs come from more than one folder pair. Review progress is a small bar.
  - `convert.html` takes the same header, verdict and quiet-button styling and the updated palette values.
- **Test fix** (`crates/sqldiff-review/tests/review.rs`): `redacted_export_passes_leak_check` ran a bare `bash`, which on Windows resolves to the WSL relay in `System32` and fails without a distro. The `bash()` helper uses `SQLDIFF_BASH`, then Git for Windows' bash, then `bash`.

Gotchas:

- Editing the CSS block by regex is risky: one earlier edit deleted the tail of a line and left an unclosed `@media`, which silently dropped all later rules. Check that braces balance after CSS edits.
- To verify GUI behaviour for real, drive headless Edge over the DevTools protocol (`--remote-debugging-port`, `Input.insertText` into a focused `.code` cell) rather than posting to `/api/diff`; the API worked all along while the page did not.
- A running `sqldiff-gui.exe` locks `target/release/sqldiff-gui.exe` on Windows; stop it before `cargo build --release`.

Not done yet: terminal output is still plain (no colour, long witnesses are not wrapped) and the `movestock` demo fixture is only about 10 lines.
