#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "${script_dir}/.." && pwd)"
desktop_resources="${repo_root}/apps/desktop-shell/src-tauri/resources"
sidecar_target="${desktop_resources}/bin/bridge-sidecar"
mobile_target="${desktop_resources}/mobile-pwa"

cd "${repo_root}"

cargo build -p bridge-sidecar --release

(
  cd apps/mobile-pwa
  npm ci
  npm run build
)

mkdir -p "${desktop_resources}/bin" "${mobile_target}"
find "${mobile_target}" -mindepth 1 ! -name ".keep" -exec rm -rf {} +

cp "${repo_root}/target/release/bridge-sidecar" "${sidecar_target}"
chmod 755 "${sidecar_target}"
cp -R "${repo_root}/apps/mobile-pwa/dist/." "${mobile_target}/"

echo "Prepared desktop bundle resources:"
echo "  ${sidecar_target}"
echo "  ${mobile_target}"
