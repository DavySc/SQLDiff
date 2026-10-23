#!/usr/bin/env bash
# Self-test for the leak-scan hooks (docs/private-overlay.md). Builds a throwaway
# repository and a local bare "remote" with synthetic deny-list terms, then
# checks that every protection fires. Touches nothing outside a temp directory.
set -uo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
fail=0
ok()  { echo "  ok   $1"; }
bad() { echo "  FAIL $1"; fail=1; }

git init -q --bare "$tmp/remote.git"
mkdir "$tmp/work" && cd "$tmp/work"
git init -q -b t
git config user.name Test; git config user.email test@users.noreply.example
git remote add origin "$tmp/remote.git"
mkdir -p scripts/hooks
cp "$root"/scripts/hooks/* scripts/hooks/
cp "$root/scripts/leak-scan.sh" scripts/
chmod +x scripts/hooks/* scripts/leak-scan.sh
git config core.hooksPath scripts/hooks
printf 'zq-secret-term-1\nzq-corp.example\n' > "$tmp/deny.txt"
export SQLDIFF_DENYLIST_FILE="$tmp/deny.txt"
remote_branches() { git --git-dir="$tmp/remote.git" branch --list | wc -l; }

echo hello > ok.txt; git add ok.txt
git commit -q -m clean >/dev/null 2>&1 && ok "clean commit passes" || bad "clean commit blocked"

echo "x ZQ-Secret-Term-1" > leak.txt; git add leak.txt
git commit -q -m x >/dev/null 2>&1 && bad "staged term not blocked" || ok "staged term blocked (case-insensitive)"
git reset -q leak.txt; rm -f leak.txt

echo more >> ok.txt; git add ok.txt
git commit -q -m "mentions zq-secret-term-1" >/dev/null 2>&1 && bad "message term not blocked" || ok "message term blocked"
git -c user.email=me@zq-corp.example commit -q -m fine >/dev/null 2>&1 && bad "identity term not blocked" || ok "identity term blocked"
git reset -q ok.txt; git checkout -q ok.txt

echo "z zq-secret-term-1" > bypass.txt; git add bypass.txt
git commit -q --no-verify -m bypassed >/dev/null 2>&1
git push -q origin t >/dev/null 2>&1; [[ "$(remote_branches)" == 0 ]] && ok "bypassed commit refused at push" || bad "bypassed commit reached the remote"

git reset -q --hard HEAD~1
echo "z zq-secret-term-1" > deep.txt; git add deep.txt; git commit -q --no-verify -m deep >/dev/null 2>&1
git rm -q deep.txt; git commit -q --no-verify -m removed >/dev/null 2>&1
git push -q origin t >/dev/null 2>&1; [[ "$(remote_branches)" == 0 ]] && ok "leak buried in history refused at push" || bad "history leak reached the remote"

git reset -q --hard HEAD~2
git push -q origin t >/dev/null 2>&1; [[ "$(remote_branches)" == 1 ]] && ok "clean history pushes" || bad "clean push failed"

unset SQLDIFF_DENYLIST_FILE
git config sqldiff.requireDenylist true
echo q >> ok.txt; git add ok.txt
APPDATA=/nonexistent SQLDIFF_CONFIG=/nonexistent HOME=/nonexistent git commit -q -m nolist >/dev/null 2>&1 \
  && bad "strict mode allowed an unscanned commit" || ok "strict mode refuses without a deny-list"

[[ $fail -eq 0 ]] && echo "hook self-test: all passed" || { echo "hook self-test: FAILED" >&2; exit 1; }
