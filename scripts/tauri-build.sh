#!/usr/bin/env bash
set -uo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${script_dir}/macos-signing-env.sh"
export CARGO_TARGET_DIR="$("${script_dir}/cargo-target-dir.sh")"

"${script_dir}/check-build-cache.sh" before
status=0

npm run prepare:bundle || status=$?

if [ "${status}" -eq 0 ]; then
  configure_macos_signing_environment "$(uname -s)"
fi

if [ "${status}" -eq 0 ]; then
  npx tauri build "$@" || status=$?
fi

"${script_dir}/check-build-cache.sh" after
exit "${status}"
