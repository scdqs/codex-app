#!/usr/bin/env bash
set -euo pipefail

phase="${1:-check}"
case "${phase}" in
  before|after|check) ;;
  *)
    echo "Usage: $0 [before|after|check]" >&2
    exit 2
    ;;
esac

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
target_dir="${CODEX_BUILD_CACHE_TARGET_DIR:-$("${script_dir}/cargo-target-dir.sh")}"
warn_bytes="${CODEX_BUILD_CACHE_WARN_BYTES:-20000000000}"
state_file="${CODEX_BUILD_CACHE_STATE_FILE:-${TMPDIR:-/tmp}/codex-mobile-bridge-build-cache-warning-${UID}}"

if ! [[ "${warn_bytes}" =~ ^[0-9]+$ ]]; then
  echo "CODEX_BUILD_CACHE_WARN_BYTES must be a non-negative integer" >&2
  exit 2
fi

if [ ! -d "${target_dir}" ]; then
  rm -f "${state_file}"
  exit 0
fi

size_kib="$(du -sk "${target_dir}" | awk '{print $1}')"
size_bytes="$((size_kib * 1024))"

if [ "${size_bytes}" -lt "${warn_bytes}" ]; then
  rm -f "${state_file}"
  exit 0
fi

size_gb="$(awk -v bytes="${size_bytes}" 'BEGIN { printf "%.1f", bytes / 1000 / 1000 / 1000 }')"
limit_gb="$(awk -v bytes="${warn_bytes}" 'BEGIN { printf "%.1f", bytes / 1000 / 1000 / 1000 }')"
message="Codex Mobile Bridge shared Cargo cache is ${size_gb} GB (warning threshold ${limit_gb} GB): ${target_dir}"

echo "WARNING [build cache ${phase}]: ${message}" >&2
echo "Run './scripts/cargo.sh clean' from the repository root to reclaim it." >&2

if [ ! -e "${state_file}" ]; then
  mkdir -p "$(dirname "${state_file}")"
  : > "${state_file}"
  if [ "${CODEX_BUILD_CACHE_NOTIFY:-1}" != "0" ] && command -v osascript >/dev/null 2>&1; then
    osascript - "${message}" <<'APPLESCRIPT' >/dev/null 2>&1 || true
on run argv
  display notification (item 1 of argv) with title "Codex build cache warning"
end run
APPLESCRIPT
  fi
fi
