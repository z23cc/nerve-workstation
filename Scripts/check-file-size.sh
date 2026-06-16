#!/usr/bin/env bash
#
# check-file-size.sh — soft file-size convention check (see docs/CONVENTIONS.md).
#
# Reports Rust source files whose NON-TEST line count exceeds the soft cap.
# "Non-test" = everything before the first `#[cfg(test)]` (tests live at the
# bottom by convention). Advisory by default (exit 0). Pass --strict to fail.
#
set -euo pipefail

CAP="${FILE_SIZE_CAP:-600}"
STRICT=0
[[ "${1:-}" == "--strict" ]] && STRICT=1

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

over=0
while IFS= read -r f; do
  [[ -f "$f" ]] || continue
  # Lines before the first `#[cfg(test)]` (or whole file if none).
  n="$(awk '/^[[:space:]]*#\[cfg\(test\)\]/{print NR-1; exit} END{print NR}' "$f" | head -1)"
  if (( n > CAP )); then
    printf '  %5d  %s\n' "$n" "$f"
    over=$((over + 1))
  fi
done < <({ git ls-files '*.rs'; git ls-files --others --exclude-standard '*.rs'; } | sort -u | grep -vE '/(tests|benches|examples)/')

if (( over == 0 )); then
  echo "file-size: all source files within ${CAP} non-test lines"
  exit 0
fi
echo "file-size: ${over} file(s) over the ${CAP}-line soft cap (split by responsibility)"
(( STRICT == 1 )) && exit 1 || exit 0
