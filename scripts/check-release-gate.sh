#!/usr/bin/env bash
set -euo pipefail

channel="${RELEASE_CHANNEL:-dev}"
artifact="${RELEASE_ARTIFACT:-}"

usage() {
  cat <<'USAGE'
Usage: scripts/check-release-gate.sh [--channel dev|beta|stable] [--artifact path]

Environment:
  RELEASE_CHANNEL                          Fallback channel when --channel is omitted.
  RELEASE_ARTIFACT                         Optional artifact path to validate.
  ENABLE_QUICK_TUNNEL_BY_DEFAULT=true      Fails stable, requires beta acknowledgement.
  QUICK_TUNNEL_BETA_ACK=true               Required for beta when quick tunnel is default-on.
USAGE
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --channel)
      channel="${2:-}"
      shift 2
      ;;
    --artifact)
      artifact="${2:-}"
      shift 2
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

fail() {
  echo "release gate failed: $*" >&2
  exit 1
}

require_env() {
  local name="$1"
  if [ -z "${!name:-}" ]; then
    fail "missing required environment variable: ${name}"
  fi
}

require_one_of() {
  local label="$1"
  shift
  for name in "$@"; do
    if [ -n "${!name:-}" ]; then
      return 0
    fi
  done
  fail "missing ${label}; set one of: $*"
}

require_notarization_credentials() {
  if [ -n "${APPLE_API_KEY_ID:-}" ] || [ -n "${APPLE_API_ISSUER_ID:-}" ] || [ -n "${APPLE_API_KEY:-}" ]; then
    require_env APPLE_API_KEY_ID
    require_env APPLE_API_ISSUER_ID
    require_env APPLE_API_KEY
    return 0
  fi

  require_env APPLE_ID
  require_env APPLE_PASSWORD
  require_env APPLE_TEAM_ID
}

case "${channel}" in
  dev|beta|stable)
    ;;
  *)
    fail "unknown RELEASE_CHANNEL '${channel}', expected dev, beta, or stable"
    ;;
esac

if [ -n "${artifact}" ] && [ ! -f "${artifact}" ]; then
  fail "RELEASE_ARTIFACT does not exist: ${artifact}"
fi

if [ "${channel}" = "beta" ]; then
  if [ "${ENABLE_QUICK_TUNNEL_BY_DEFAULT:-false}" = "true" ] && [ "${QUICK_TUNNEL_BETA_ACK:-false}" != "true" ]; then
    fail "beta quick tunnel default-on requires QUICK_TUNNEL_BETA_ACK=true"
  fi
fi

if [ "${channel}" = "stable" ]; then
  if [ "${ENABLE_QUICK_TUNNEL_BY_DEFAULT:-false}" = "true" ]; then
    fail "stable releases cannot enable Quick Tunnel by default"
  fi
  if [ "${STABLE_REMOTE_ACCESS_PROVIDER:-}" = "quick_tunnel" ]; then
    fail "stable releases cannot treat Quick Tunnel as the stable remote access provider"
  fi

  require_one_of "macOS signing identity or imported certificate" APPLE_SIGNING_IDENTITY MACOS_CERTIFICATE_P12
  if [ -n "${MACOS_CERTIFICATE_P12:-}" ]; then
    require_env MACOS_CERTIFICATE_PASSWORD
  fi
  require_notarization_credentials
  require_one_of "updater signing private key" TAURI_SIGNING_PRIVATE_KEY TAURI_PRIVATE_KEY
  require_one_of "updater signing password" TAURI_SIGNING_PRIVATE_KEY_PASSWORD TAURI_KEY_PASSWORD
  require_env UPDATE_MANIFEST_URL

  if [ -n "${artifact}" ]; then
    artifact_name="$(basename "${artifact}")"
    case "${artifact_name}" in
      *unsigned*.dmg|*Unsigned*.dmg)
        fail "stable DMG artifact name must not contain 'unsigned': ${artifact_name}"
        ;;
    esac
  fi
fi

echo "release gate passed for channel: ${channel}"

