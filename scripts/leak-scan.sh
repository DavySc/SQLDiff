#!/usr/bin/env bash
# Leak scan (design section 5.6): fail if any deny-listed term appears in the
# repository. The deny-list is never committed. It arrives at run time:
#
#   SQLDIFF_DENYLIST_FILE / SQLDIFF_DENYLIST      plain terms (local hooks)
#   SQLDIFF_DENYHASHES_FILE / SQLDIFF_DENYHASHES  salted hashes from
#       `sqldiff pack deny-hashes` (CI), so the terms never leave the machine
#       that holds the private packs. Needs the sqldiff binary: $SQLDIFF_BIN,
#       sqldiff on PATH, or a build in target/.
#
# With SQLDIFF_REQUIRE_DENYLIST=true a missing list fails instead of skipping.
#
# Usage: leak-scan.sh                  scan tracked files in the working tree
#        leak-scan.sh --cached         scan the index (what the next commit holds)
#        leak-scan.sh --file F         scan one file (for example a commit message)
#        leak-scan.sh --commits RANGE  scan the patch, message and identities of
#                                      every commit in RANGE (git rev-list syntax)
set -euo pipefail

mode="tree"
arg=""
case "${1:-}" in
  --cached) mode="cached" ;;
  --file) mode="file"; arg="${2:?--file needs a path}" ;;
  --commits) mode="commits"; arg="${2:?--commits needs a range}" ;;
  "") ;;
  *) echo "leak-scan: unknown option $1" >&2; exit 2 ;;
esac

list=""
if [[ -n "${SQLDIFF_DENYLIST_FILE:-}" && -f "${SQLDIFF_DENYLIST_FILE}" ]]; then
  list="$(cat "${SQLDIFF_DENYLIST_FILE}")"
elif [[ -n "${SQLDIFF_DENYLIST:-}" ]]; then
  list="${SQLDIFF_DENYLIST}"
fi
hashes=""
if [[ -z "${list//[[:space:]]/}" ]]; then
  if [[ -n "${SQLDIFF_DENYHASHES_FILE:-}" && -f "${SQLDIFF_DENYHASHES_FILE}" ]]; then
    hashes="$(cat "${SQLDIFF_DENYHASHES_FILE}")"
  elif [[ -n "${SQLDIFF_DENYHASHES:-}" ]]; then
    hashes="${SQLDIFF_DENYHASHES}"
  fi
fi

if [[ -z "${list//[[:space:]]/}" && -z "${hashes//[[:space:]]/}" ]]; then
  if [[ "${SQLDIFF_REQUIRE_DENYLIST:-false}" == "true" ]]; then
    echo "leak-scan: SQLDIFF_REQUIRE_DENYLIST is set but no deny-list was supplied" >&2
    exit 1
  fi
  echo "leak-scan: no deny-list supplied; skipping"
  exit 0
fi

terms="$(mktemp)"
out="$(mktemp)"
trap 'rm -f "$terms" "$out"' EXIT

if [[ -n "$hashes" ]]; then
  printf '%s\n' "$hashes" | tr -d '\r' > "$terms"
  bin="${SQLDIFF_BIN:-}"
  if [[ -z "$bin" ]]; then
    bin="$(command -v sqldiff 2>/dev/null)" || bin=""
  fi
  if [[ -z "$bin" ]]; then
    # The newer of the two builds: an old one may predate `pack scan`.
    root="$(git rev-parse --show-toplevel)"
    for c in "$root/target/release/sqldiff" "$root/target/debug/sqldiff"; do
      for f in "$c" "$c.exe"; do
        if [[ -f "$f" && ( -z "$bin" || "$f" -nt "$bin" ) ]]; then bin="$f"; fi
      done
    done
  fi
  if [[ -z "$bin" ]]; then
    echo "leak-scan: a hashed deny-list needs the sqldiff binary (cargo build -p sqldiff-cli, or set SQLDIFF_BIN)" >&2
    exit 2
  fi
  count="$(($(grep -c . "$terms") - 2)) hashed"
else
  printf '%s\n' "$list" | tr -d '\r' | sed '/^[[:space:]]*$/d; /^#/d' > "$terms"
  count="$(wc -l < "$terms" | tr -d ' ')"
fi

# Run a matcher that exits 0 for clean and 1 for a match, writing locations
# (never terms) to $out. Any other status is an error, never "clean".
run() {
  local rc=0
  "$@" >> "$out" 2>/dev/null || rc=$?
  if [[ $rc -gt 1 ]]; then
    echo "leak-scan: the scan itself failed (status $rc); refusing to report clean" >&2
    [[ -z "${bin:-}" ]] || echo "  scanner: $bin (an old build may lack \`pack scan\`; rebuild it)" >&2
    exit 2
  fi
  return $rc
}

# grep -q exits 0 on a match: invert it to the 0 = clean convention of run.
plain_stdin() { ! grep -q -i -F -f "$terms" || { echo "$1"; return 1; }; }

found=0
case "$mode:${hashes:+hashed}" in
  tree:)
    if git grep -q -i -F -f "$terms" -- . ':!target' >/dev/null 2>&1; then
      git grep -l -i -F -f "$terms" -- . ':!target' >> "$out"; found=1
    fi ;;
  cached:)
    if git grep --cached -q -i -F -f "$terms" -- . ':!target' >/dev/null 2>&1; then
      git grep --cached -l -i -F -f "$terms" -- . ':!target' >> "$out"; found=1
    fi ;;
  file:)
    if grep -q -i -F -f "$terms" -- "$arg"; then
      echo "$arg (lines: $(grep -n -i -F -f "$terms" -- "$arg" | cut -d: -f1 | paste -sd, -))" >> "$out"; found=1
    fi ;;
  tree:hashed)
    # As git grep does, skip tracked files deleted from the working tree.
    while IFS= read -r -d '' p; do
      [[ -e "$p" ]] && printf '%s\0' "$p"
    done < <(git ls-files -z -- . ':!target') > "$out.list"
    run "$bin" pack scan --hashes "$terms" --files-from "$out.list" || found=1
    rm -f "$out.list" ;;
  cached:hashed)
    while IFS= read -r -d '' p; do
      git show ":$p" > "$out.blob"
      run "$bin" pack scan --hashes "$terms" --label "$p" < "$out.blob" || found=1
    done < <(git ls-files -z --cached -- . ':!target')
    rm -f "$out.blob" ;;
  file:hashed)
    run "$bin" pack scan --hashes "$terms" "$arg" || found=1 ;;
  commits:*)
    for c in $(git rev-list "$arg"); do
      short="$(git rev-parse --short "$c")"
      # The patch includes file names; the format adds message and identities.
      git show --no-color --format='%an%n%ae%n%cn%n%ce%n%B' "$c" -- . ':!target' > "$out.commit"
      if [[ -n "$hashes" ]]; then
        run "$bin" pack scan --hashes "$terms" --label "commit $short" < "$out.commit" || found=1
      else
        run plain_stdin "commit $short" < "$out.commit" || found=1
      fi
    done
    rm -f "$out.commit" ;;
esac

if [[ $found -ne 0 ]]; then
  # Report where, never what: printing the matched term would leak it into logs.
  echo "leak-scan: deny-listed terms found in:" >&2
  cat "$out" >&2
  exit 1
fi
echo "leak-scan: clean ($count terms)"
