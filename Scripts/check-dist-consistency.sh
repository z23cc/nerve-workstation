#!/usr/bin/env bash
#
# check-dist-consistency.sh — zero-toolchain SRI consistency gate for the
# committed crates/nerve-gui/dist/ bundle that the `nerve` binary
# include_bytes!'s and serves at /app.
#
# WHY THIS EXISTS (and why it is NOT a byte-diff or a trunk rebuild):
# A debug/release wasm-bindgen build is not byte-reproducible, so we cannot
# diff the committed bundle against a fresh `trunk build` in CI. But the bundle
# is internally self-describing: dist/index.html pins a Subresource Integrity
# hash (integrity="sha384-<base64>") for each served asset. A browser refuses
# to load an asset whose bytes don't match that hash. This script performs the
# EXACT same check at commit/CI time, with no wasm toolchain:
#
#   for each asset: sha384 = "sha384-" + base64( sha384_raw(asset_bytes) )
#   and assert it equals the integrity="..." value index.html declares for it.
#
# This catches a half-stale committed bundle — index.html regenerated but one
# of the asset files not re-committed (or vice versa) — which would otherwise
# ship a /app that silently fails SRI in the user's browser at load time.
#
# Only `openssl` (dgst + base64) is required; no Rust, no trunk, no wasm-opt.
#
set -euo pipefail

DIST="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/crates/nerve-gui/dist"
INDEX="$DIST/index.html"

if [[ ! -f "$INDEX" ]]; then
  echo "check-dist-consistency: missing $INDEX" >&2
  exit 1
fi

ASSETS=(styles.css nerve-gui.js nerve-gui_bg.wasm)

# Extract the integrity value for a given asset filename.
# Robust to attribute ordering: we isolate the single <link> element whose
# href ends in /<filename> (or is exactly the filename), then pull the
# sha384-... token out of that element's integrity="..." attribute.
extract_integrity() {
  local filename="$1"
  # Put each <link ...> element on its own line, then keep the one referencing
  # this asset, then extract its sha384 integrity token.
  tr '\n' ' ' < "$INDEX" \
    | grep -o '<link[^>]*>' \
    | grep -F "$filename" \
    | grep -o 'integrity="sha384-[A-Za-z0-9+/=]*"' \
    | head -n1 \
    | sed -e 's/^integrity="//' -e 's/"$//'
}

fail=0
ok=0
for asset in "${ASSETS[@]}"; do
  file="$DIST/$asset"
  if [[ ! -f "$file" ]]; then
    echo "check-dist-consistency: FAIL $asset — asset file missing at $file" >&2
    fail=1
    continue
  fi

  expected="$(extract_integrity "$asset")"
  if [[ -z "$expected" ]]; then
    echo "check-dist-consistency: FAIL $asset — no integrity=\"sha384-...\" found in index.html for this asset" >&2
    fail=1
    continue
  fi

  actual="sha384-$(openssl dgst -sha384 -binary "$file" | openssl base64 -A)"

  if [[ "$expected" != "$actual" ]]; then
    echo "check-dist-consistency: FAIL $asset — SRI mismatch (half-stale bundle)" >&2
    echo "  expected (index.html): $expected" >&2
    echo "  actual   (bytes):      $actual" >&2
    fail=1
    continue
  fi

  ok=$((ok + 1))
done

if [[ "$fail" -ne 0 ]]; then
  echo "check-dist-consistency: committed dist/ is INCONSISTENT — rebuild and re-commit the whole bundle (trunk build) so index.html SRI matches every asset." >&2
  exit 1
fi

echo "dist SRI consistent ($ok/${#ASSETS[@]})"
exit 0
