#!/usr/bin/env bash
# Leak scan (design section 5.6): fail if any deny-listed term appears in the
# repository. The deny-list is never committed; CI and the pre-commit hook
# receive it at run time, either as a file path in SQLDIFF_DENYLIST_FILE or as
# newline-separated terms in SQLDIFF_DENYLIST.
set -euo pipefail

list=""
if [[ -n "${SQLDIFF_DENYLIST_FILE:-}" && -f "${SQLDIFF_DENYLIST_FILE}" ]]; then
  list="$(cat "${SQLDIFF_DENYLIST_FILE}")"
elif [[ -n "${SQLDIFF_DENYLIST:-}" ]]; then
  list="${SQLDIFF_DENYLIST}"
fi

if [[ -z "${list//[[:space:]]/}" ]]; then
  echo "leak-scan: no deny-list supplied; skipping"
  exit 0
fi

terms="$(mktemp)"
trap 'rm -f "$terms"' EXIT
printf '%s\n' "$list" | sed '/^[[:space:]]*$/d; /^#/d' > "$terms"

if git grep -n -i -F -f "$terms" -- . ':!target' >/dev/null 2>&1; then
  echo "leak-scan: deny-listed terms found in:" >&2
  git grep -l -i -F -f "$terms" -- . ':!target' >&2
  exit 1
fi
echo "leak-scan: clean ($(wc -l < "$terms") terms)"
