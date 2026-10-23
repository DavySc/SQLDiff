#!/usr/bin/env bash
# Leak scan (design section 5.6): fail if any deny-listed term appears in the
# repository. The deny-list is never committed; CI and the pre-commit hook
# receive it at run time, either as a file path in SQLDIFF_DENYLIST_FILE or as
# newline-separated terms in SQLDIFF_DENYLIST.
#
# Usage: leak-scan.sh            scan tracked files in the working tree
#        leak-scan.sh --cached   scan the index (what the next commit holds)
#        leak-scan.sh --file F   scan one file (for example a commit message)
set -euo pipefail

mode="tree"
file=""
case "${1:-}" in
  --cached) mode="cached" ;;
  --file) mode="file"; file="${2:?--file needs a path}" ;;
  "") ;;
  *) echo "leak-scan: unknown option $1" >&2; exit 2 ;;
esac

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
printf '%s\n' "$list" | tr -d '\r' | sed '/^[[:space:]]*$/d; /^#/d' > "$terms"

case "$mode" in
  tree)   cmd=(git grep -i -F -f "$terms"); paths=(-- . ':!target') ;;
  cached) cmd=(git grep --cached -i -F -f "$terms"); paths=(-- . ':!target') ;;
  file)   cmd=(grep -i -F -f "$terms"); paths=(-- "$file") ;;
esac

if "${cmd[@]}" -q "${paths[@]}" >/dev/null 2>&1; then
  # Report where, never what: printing the matched term would leak it into logs.
  echo "leak-scan: deny-listed terms found in:" >&2
  if [[ "$mode" == "file" ]]; then
    echo "$file (lines: $("${cmd[@]}" -n "${paths[@]}" | cut -d: -f1 | paste -sd, -))" >&2
  else
    "${cmd[@]}" -l "${paths[@]}" >&2
  fi
  exit 1
fi
echo "leak-scan: clean ($(wc -l < "$terms") terms)"
