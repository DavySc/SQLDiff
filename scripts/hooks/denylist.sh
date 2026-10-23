#!/usr/bin/env bash
# Shared by the git hooks: write the deny-list to the file named in $1.
#
# Sources, first hit wins: SQLDIFF_DENYLIST_FILE, SQLDIFF_DENYLIST, then
# `sqldiff pack deny-list`, which reads the private packs named in the user
# config (outside this clone; docs/private-overlay.md).
#
# With `git config sqldiff.requireDenylist true` (set by
# `scripts/install-hooks.sh --strict`) an empty deny-list is an error, so a
# machine that holds private packs never commits or pushes unscanned.
set -euo pipefail

out="$1"
: > "$out"

if [[ -n "${SQLDIFF_DENYLIST_FILE:-}" && -f "${SQLDIFF_DENYLIST_FILE}" ]]; then
  cat "${SQLDIFF_DENYLIST_FILE}" >> "$out"
elif [[ -n "${SQLDIFF_DENYLIST:-}" ]]; then
  printf '%s\n' "${SQLDIFF_DENYLIST}" >> "$out"
else
  root="$(git rev-parse --show-toplevel)"
  bin=""
  for c in sqldiff "$root/target/release/sqldiff" "$root/target/debug/sqldiff"; do
    if command -v "$c" >/dev/null 2>&1; then bin="$c"; break; fi
  done
  if [[ -n "${SQLDIFF_CONFIG:-}" ]]; then
    cfg="${SQLDIFF_CONFIG}"
  elif [[ -n "${APPDATA:-}" ]]; then
    cfg="${APPDATA}/sqldiff/config.toml"
  else
    cfg="${HOME}/.config/sqldiff/config.toml"
  fi
  if [[ -n "$bin" && -f "$cfg" ]]; then
    # --isolated with the user config named explicitly: never pick up a
    # project sqldiff.toml from inside the clone.
    "$bin" pack deny-list --isolated --user-config "$cfg" >> "$out"
  fi
fi

# Normalize: drop CR, blank lines and comments.
tmp="$out.tmp"
tr -d '\r' < "$out" | sed '/^[[:space:]]*$/d; /^#/d' > "$tmp" && mv "$tmp" "$out"

if [[ ! -s "$out" ]]; then
  if [[ "$(git config --bool sqldiff.requireDenylist 2>/dev/null || echo false)" == "true" ]]; then
    echo "sqldiff hooks: sqldiff.requireDenylist is set but no deny-list was found." >&2
    echo "  Build the CLI (cargo build -p sqldiff-cli) and check the private pack in your user config," >&2
    echo "  or set SQLDIFF_DENYLIST_FILE. Refusing to continue." >&2
    exit 1
  fi
  echo "sqldiff hooks: no deny-list; skipping scan" >&2
fi
