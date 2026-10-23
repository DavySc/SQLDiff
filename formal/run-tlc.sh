#!/usr/bin/env bash
# Check the error/transaction model: regenerate-check the cases, then run TLC
# on both configurations.  TLA2TOOLS points at tla2tools.jar; if unset, the
# pinned release is downloaded into formal/.tools/.  Either way the jar must
# match the pinned checksum.
set -euo pipefail
cd "$(dirname "$0")"

# 1.7.4 is the latest stable release; 1.8.0 is a pre-release whose asset is
# replaced over time.
TLA_VERSION=1.7.4
TLA_SHA256=936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88

python3 gen_cases.py --check

jar="${TLA2TOOLS:-}"
if [[ -z "$jar" ]]; then
  jar=.tools/tla2tools-$TLA_VERSION.jar
  if [[ ! -f "$jar" ]]; then
    mkdir -p .tools
    curl -fsSL -o "$jar.tmp" \
      "https://github.com/tlaplus/tlaplus/releases/download/v$TLA_VERSION/tla2tools.jar"
    mv "$jar.tmp" "$jar"
  fi
fi
if ! echo "$TLA_SHA256  $jar" | sha256sum -c - >/dev/null 2>&1; then
  echo "run-tlc: $jar does not match the pinned tla2tools $TLA_VERSION checksum" >&2
  exit 1
fi

meta=$(mktemp -d)
trap 'rm -rf "$meta"' EXIT
for cfg in Cases Explore; do
  echo "== TLC $cfg.cfg"
  java -XX:+UseParallelGC -cp "$jar" tlc2.TLC -config "$cfg.cfg" -workers auto \
    -metadir "$meta/$cfg" ErrorTxn.tla 2>&1 \
    | grep -v '^Picked up JAVA_TOOL_OPTIONS' > "$meta/$cfg.log" || true
  if ! grep -q '^Model checking completed. No error has been found.' "$meta/$cfg.log"; then
    cat "$meta/$cfg.log"
    exit 1
  fi
  grep -E 'states generated|^Finished in' "$meta/$cfg.log" | tail -2
done
