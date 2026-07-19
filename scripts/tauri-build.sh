#!/usr/bin/env bash
set -uo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export CARGO_TARGET_DIR="$("${script_dir}/cargo-target-dir.sh")"

"${script_dir}/check-build-cache.sh" before
status=0

npm run prepare:bundle || status=$?

if [ "${status}" -eq 0 ] \
  && [ "$(uname -s)" = "Darwin" ] \
  && [ -z "${APPLE_SIGNING_IDENTITY:-}" ] \
  && [ -z "${MACOS_CERTIFICATE_P12:-}" ]; then
  export APPLE_SIGNING_IDENTITY="-"
  echo "Using ad-hoc macOS signing for this unsigned build."
fi

if [ "${status}" -eq 0 ]; then
  npx tauri build "$@" || status=$?
fi

"${script_dir}/check-build-cache.sh" after
exit "${status}"
