#!/usr/bin/env bash
set -uo pipefail

if [ "$#" -eq 0 ]; then
  echo "Usage: $0 <command> [args...]" >&2
  exit 2
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export CARGO_TARGET_DIR="$("${script_dir}/cargo-target-dir.sh")"

"${script_dir}/check-build-cache.sh" before
"$@"
status=$?
"${script_dir}/check-build-cache.sh" after
exit "${status}"
