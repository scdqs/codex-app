#!/usr/bin/env bash
set -euo pipefail

run_bundle=true

usage() {
  cat <<'USAGE'
Usage: scripts/dogfood-smoke.sh [--skip-bundle]

Runs the local dogfood smoke checks before sharing an internal build:
  - dev release gate
  - Rust workspace tests
  - mobile PWA tests and production build
  - desktop debug .app bundle build, unless --skip-bundle is passed
USAGE
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --skip-bundle)
      run_bundle=false
      shift
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "${script_dir}/.." && pwd)"
cargo_target_dir="$("${script_dir}/cargo-target-dir.sh")"

cd "${repo_root}"

echo "==> release gate: dev"
scripts/check-release-gate.sh --channel dev

echo "==> cargo test --workspace"
scripts/cargo.sh test --workspace

echo "==> mobile PWA tests"
(
  cd apps/mobile-pwa
  npm test -- --run
  npm run build
)

if [ "${run_bundle}" = "true" ]; then
  echo "==> desktop debug app bundle"
  (
    cd apps/desktop-shell
    npm run tauri:build -- --debug --bundles app
  )
  bundled_cloudflared="${cargo_target_dir}/debug/bundle/macos/Codex Mobile Bridge.app/Contents/Resources/bin/cloudflared"
  if [ ! -x "${bundled_cloudflared}" ]; then
    echo "Bundled cloudflared is missing or not executable: ${bundled_cloudflared}" >&2
    exit 1
  fi
else
  echo "==> desktop debug app bundle skipped"
fi

echo "dogfood smoke passed"
