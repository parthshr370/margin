#!/usr/bin/env bash
# Margin's checks, run on this machine before a merge to main or a release (Margin has no hosted
# CI): formatting, the generated attribution, clippy over the workspace with warnings as errors,
# then tests. Builds with the same craft-i18n and craft-fonts inputs as a release.
#
# Usage: packaging/margin/check.sh [--full]
#   default  the tests of the crates Margin lives in: pdfcraft-ui-egui (Margin's chrome is
#            crates/ui-egui/src/margin), margin-config and margin
#   --full   every workspace test (about 10,000; run it after an upstream sync)
#
# Cargo uses half the cores unless CARGO_BUILD_JOBS is set, so the machine stays usable.
set -euo pipefail
# shellcheck source=env.sh
. "$(dirname "${BASH_SOURCE[0]}")/env.sh"

FULL=0
while [ $# -gt 0 ]; do
  case "$1" in
    --full) FULL=1; shift ;;
    -h | --help) sed -n '2,12p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-$(( ($(nproc) + 1) / 2 ))}"
margin_build_inputs
cd "$ROOT"

echo "==> fmt"
cargo fmt --all -- --check
echo "==> attribution"
cargo xtask assets
echo "==> OCR models (the OCR tests read them from assets/models)"
cargo xtask models
echo "==> clippy"
cargo clippy --workspace --all-targets --locked -- -D warnings
if [ "$FULL" = 1 ]; then
  echo "==> tests (workspace)"
  cargo test --workspace --locked --no-fail-fast
else
  echo "==> tests (pdfcraft-ui-egui, margin-config, margin)"
  cargo test --locked --no-fail-fast -p pdfcraft-ui-egui -p margin-config -p margin
fi
echo "==> checks passed"
