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

# A commit whose patch the scan's pathspec filters out still has its message and identities scanned.
git commit -q --no-verify --allow-empty -m "names zq-secret-term-1" >/dev/null 2>&1
bash scripts/leak-scan.sh --commits HEAD~1..HEAD >/dev/null 2>&1 && bad "empty commit message not scanned" || ok "empty commit message scanned"
git reset -q --hard HEAD~1
git -c user.email=me@zq-corp.example commit -q --no-verify --allow-empty -m plain >/dev/null 2>&1
bash scripts/leak-scan.sh --commits HEAD~1..HEAD >/dev/null 2>&1 && bad "empty commit identity not scanned" || ok "empty commit identity scanned"
git reset -q --hard HEAD~1

unset SQLDIFF_DENYLIST_FILE
git config sqldiff.requireDenylist true
echo q >> ok.txt; git add ok.txt
APPDATA=/nonexistent SQLDIFF_CONFIG=/nonexistent HOME=/nonexistent git commit -q -m nolist >/dev/null 2>&1 \
  && bad "strict mode allowed an unscanned commit" || ok "strict mode refuses without a deny-list"

git reset -q ok.txt; git checkout -q ok.txt
git config sqldiff.requireDenylist false

# Hashed deny-list (what CI holds): needs the sqldiff binary.
bin="${SQLDIFF_BIN:-}"
for c in "$root/target/debug/sqldiff" "$root/target/release/sqldiff"; do
  [[ -z "$bin" ]] && command -v "$c" >/dev/null 2>&1 && bin="$c"
done
if [[ -z "$bin" ]]; then
  if [[ "${CI:-}" == "true" ]]; then bad "hashed scan: no sqldiff binary to test with"
  else echo "  skip hashed scan tests (cargo build -p sqldiff-cli first)"; fi
else
  export SQLDIFF_BIN="$bin"
  printf 'deny_list = ["zq-secret-term-1", "zq-corp.example"]\n[pack]\nname = "t"\nversion = "1"\n' > "$tmp/pack.toml"
  "$bin" pack deny-hashes --isolated --pack "$tmp/pack.toml" > "$tmp/deny.hashes"
  grep -q -i zq- "$tmp/deny.hashes" && bad "hash list holds a plain term" || ok "hash list holds no plain term"
  export SQLDIFF_DENYHASHES_FILE="$tmp/deny.hashes"
  scan() { bash scripts/leak-scan.sh "$@" > "$tmp/scan.out" 2>&1; }

  scan && ok "hashed: clean tree passes" || bad "hashed: clean tree blocked"
  echo "x ZQ-Secret-Term-1" > leak.txt; git add leak.txt
  scan && bad "hashed: tree term not found" || ok "hashed: tree term found (case-insensitive)"
  grep -q -i zq- "$tmp/scan.out" && bad "hashed: report names the term" || ok "hashed: report names the place, not the term"
  scan --cached && bad "hashed: staged term not found" || ok "hashed: staged term found"
  git reset -q leak.txt; rm -f leak.txt

  base="$(git rev-parse HEAD)"
  echo "y zq-corp.example" > deep.txt; git add deep.txt; git commit -q --no-verify -m deep
  git rm -q deep.txt; git commit -q --no-verify -m removed
  scan --commits "$base..HEAD" && bad "hashed: leak buried in history not found" || ok "hashed: leak buried in history found"
  git commit -q --no-verify --allow-empty -m "names zq-secret-term-1"
  scan --commits HEAD~1..HEAD && bad "hashed: message term not found" || ok "hashed: message term found"
  git reset -q --hard "$base"
  git -c user.email=me@zq-corp.example commit -q --no-verify --allow-empty -m plain
  scan --commits HEAD~1..HEAD && bad "hashed: identity term not found" || ok "hashed: identity term found"
  git reset -q --hard "$base"
  mkdir -p target; echo "t zq-secret-term-1" > target/x.txt; git add -f target/x.txt
  git commit -q --no-verify -m "names zq-secret-term-1"
  scan --commits HEAD~1..HEAD && bad "hashed: message of a target-only commit not found" || ok "hashed: message of a target-only commit found"
  git reset -q --hard "$base"
  scan --commits "$base" && ok "hashed: clean history passes" || bad "hashed: clean history blocked"

  printf '#!/bin/sh\nexit 3\n' > "$tmp/broken"; chmod +x "$tmp/broken"
  SQLDIFF_BIN="$tmp/broken" scan; rc=$?
  [[ $rc -eq 2 ]] && ! grep -q 'leak-scan: clean' "$tmp/scan.out" && ok "hashed: a failing scanner is an error, not clean" \
    || bad "hashed: a failing scanner was reported as status $rc"
  unset SQLDIFF_DENYHASHES_FILE
fi

SQLDIFF_REQUIRE_DENYLIST=true bash scripts/leak-scan.sh >/dev/null 2>&1 \
  && bad "required deny-list missing, scan passed" || ok "required deny-list missing fails the scan"

[[ $fail -eq 0 ]] && echo "hook self-test: all passed" || { echo "hook self-test: FAILED" >&2; exit 1; }
