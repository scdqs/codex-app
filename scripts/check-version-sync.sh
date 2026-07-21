#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "${script_dir}/.." && pwd)"
expected="$(tr -d '[:space:]' < "${repo_root}/VERSION")"

check_toml() {
  local path="$1"
  local actual
  actual="$(sed -n 's/^version = "\([^"]*\)"$/\1/p' "${repo_root}/${path}" | head -n 1)"
  if [ "${actual}" != "${expected}" ]; then
    echo "Version mismatch: ${path} is ${actual:-missing}, expected ${expected}" >&2
    return 1
  fi
}

check_json() {
  local path="$1"
  local actual
  actual="$(node -e 'const fs=require("fs"); const value=JSON.parse(fs.readFileSync(process.argv[1], "utf8")); process.stdout.write(String(value.version || ""));' "${repo_root}/${path}")"
  if [ "${actual}" != "${expected}" ]; then
    echo "Version mismatch: ${path} is ${actual:-missing}, expected ${expected}" >&2
    return 1
  fi
}

for manifest in \
  crates/bridge-core/Cargo.toml \
  crates/desktop-core/Cargo.toml \
  apps/bridge-sidecar/Cargo.toml \
  apps/desktop-shell/src-tauri/Cargo.toml; do
  check_toml "${manifest}"
done

for manifest in \
  apps/desktop-shell/package.json \
  apps/mobile-pwa/package.json \
  apps/desktop-shell/src-tauri/tauri.conf.json; do
  check_json "${manifest}"
done

echo "Version ${expected} is synchronized across desktop, sidecar, and PWA manifests."
