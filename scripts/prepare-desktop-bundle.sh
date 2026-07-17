#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "${script_dir}/.." && pwd)"
desktop_resources="${repo_root}/apps/desktop-shell/src-tauri/resources"
sidecar_target="${desktop_resources}/bin/bridge-sidecar"
cloudflared_target="${desktop_resources}/bin/cloudflared"
mobile_target="${desktop_resources}/mobile-pwa"

cloudflared_version="${CLOUDFLARED_VERSION:-2026.7.1}"

cloudflared_arch() {
  case "$(uname -m)" in
    arm64|aarch64)
      printf '%s\n' "arm64"
      ;;
    x86_64|amd64)
      printf '%s\n' "amd64"
      ;;
    *)
      echo "Unsupported macOS architecture for cloudflared: $(uname -m)" >&2
      return 1
      ;;
  esac
}

system_cloudflared() {
  for candidate in "${CODEX_MOBILE_BRIDGE_TUNNEL_BIN:-}" /opt/homebrew/bin/cloudflared /usr/local/bin/cloudflared; do
    if [ -n "${candidate}" ] && [ -x "${candidate}" ]; then
      printf '%s\n' "${candidate}"
      return 0
    fi
  done

  if command -v cloudflared >/dev/null 2>&1; then
    command -v cloudflared
    return 0
  fi

  return 1
}

prepare_cloudflared() {
  local arch
  arch="$(cloudflared_arch)"
  local cache_dir="${repo_root}/target/cloudflared/${cloudflared_version}-${arch}"
  local archive="${cache_dir}/cloudflared-darwin-${arch}.tgz"
  local extracted="${cache_dir}/cloudflared"
  local url="${CLOUDFLARED_URL:-https://github.com/cloudflare/cloudflared/releases/download/${cloudflared_version}/cloudflared-darwin-${arch}.tgz}"

  mkdir -p "${cache_dir}"
  if [ ! -x "${extracted}" ]; then
    echo "Downloading cloudflared ${cloudflared_version} for darwin-${arch}"
    if curl -fL --retry 3 --connect-timeout 15 "${url}" -o "${archive}"; then
      local extract_dir="${cache_dir}/extract"
      rm -rf "${extract_dir}"
      mkdir -p "${extract_dir}"
      tar -xzf "${archive}" -C "${extract_dir}"
      local found
      found="$(find "${extract_dir}" -type f -name cloudflared -print -quit)"
      if [ -z "${found}" ]; then
        echo "cloudflared archive did not contain a cloudflared binary" >&2
        return 1
      fi
      cp "${found}" "${extracted}"
      chmod 755 "${extracted}"
      rm -rf "${extract_dir}"
    else
      local fallback
      fallback="$(system_cloudflared || true)"
      if [ -z "${fallback}" ]; then
        echo "Failed to download cloudflared and no local cloudflared binary was found" >&2
        return 1
      fi
      echo "Using local cloudflared fallback: ${fallback}"
      cp "${fallback}" "${extracted}"
      chmod 755 "${extracted}"
    fi
  fi

  cp "${extracted}" "${cloudflared_target}"
  chmod 755 "${cloudflared_target}"
}

cd "${repo_root}"

"${script_dir}/check-version-sync.sh"

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
prepare_cloudflared
cp -R "${repo_root}/apps/mobile-pwa/dist/." "${mobile_target}/"

echo "Prepared desktop bundle resources:"
echo "  ${sidecar_target}"
echo "  ${cloudflared_target}"
echo "  ${mobile_target}"
