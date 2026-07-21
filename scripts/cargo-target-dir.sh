#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "${script_dir}/.." && pwd)"

configured_target="${CARGO_TARGET_DIR:-${CODEX_SHARED_CARGO_TARGET_DIR:-}}"
if [ -n "${configured_target}" ]; then
  if [[ "${configured_target}" = /* ]]; then
    printf '%s\n' "${configured_target}"
  else
    printf '%s/%s\n' "${repo_root}" "${configured_target}"
  fi
  exit 0
fi

git_common_dir="$(git -C "${repo_root}" rev-parse --path-format=absolute --git-common-dir)"
main_worktree_root="$(dirname "${git_common_dir}")"
printf '%s-shared-target\n' "${main_worktree_root}"
