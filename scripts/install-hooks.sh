#!/usr/bin/env bash
# Point this clone's git hooks at scripts/hooks (commit and push leak scans).
#   --strict   refuse commits and pushes when no deny-list can be found; use it
#              on every machine that holds private packs.
#   --check    verify the protection is actually in place; exit 1 if not.
#              Git silently ignores a core.hooksPath that points nowhere, so
#              run this after cloning, switching tools or moving the folder.
set -euo pipefail
root="$(git rev-parse --show-toplevel)"
cd "$root"

check() {
  local ok=0
  local path
  path="$(git config --local --get core.hooksPath || true)"
  if [[ "$path" != "scripts/hooks" ]]; then
    echo "check: core.hooksPath is '${path:-unset}', expected scripts/hooks" >&2; ok=1
  fi
  for h in pre-commit commit-msg pre-push denylist.sh; do
    if [[ ! -x "scripts/hooks/$h" ]]; then
      echo "check: scripts/hooks/$h is missing or not executable" >&2; ok=1
    fi
  done
  if [[ "$(git config --local --bool sqldiff.requireDenylist 2>/dev/null || echo false)" != "true" ]]; then
    echo "check: strict mode is off (an empty deny-list would skip the scan)" >&2; ok=1
  fi
  email="$(git config --local --get user.email || true)"
  if [[ -z "$email" ]]; then
    echo "check: no clone-local user.email; the global identity would be used" >&2; ok=1
  fi
  local t; t="$(mktemp)"
  if bash scripts/hooks/denylist.sh "$t" 2>/dev/null && [[ -s "$t" ]]; then
    echo "check: deny-list found ($(wc -l < "$t") terms)"
  else
    echo "check: no deny-list found" >&2; ok=1
  fi
  rm -f "$t"
  [[ $ok -eq 0 ]] && echo "check: protection in place" || echo "check: PROTECTION NOT IN PLACE" >&2
  return $ok
}

case "${1:-}" in
  --check) check; exit $? ;;
  --strict)
    git config core.hooksPath scripts/hooks
    chmod +x scripts/hooks/* scripts/leak-scan.sh 2>/dev/null || true
    git config sqldiff.requireDenylist true
    echo "hooks installed (strict: an empty deny-list blocks commits and pushes)"
    ;;
  "")
    git config core.hooksPath scripts/hooks
    chmod +x scripts/hooks/* scripts/leak-scan.sh 2>/dev/null || true
    echo "hooks installed"
    ;;
  *) echo "install-hooks: unknown option $1" >&2; exit 2 ;;
esac
