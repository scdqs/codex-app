#!/usr/bin/env bash

configure_macos_signing_environment() {
  local platform="${1:-$(uname -s)}"

  if [ "${platform}" != "Darwin" ] \
    || [ -n "${APPLE_SIGNING_IDENTITY:-}" ] \
    || [ -n "${MACOS_CERTIFICATE_P12:-}" ]; then
    return
  fi

  export APPLE_SIGNING_IDENTITY="-"
  unset APPLE_API_KEY APPLE_API_KEY_ID APPLE_API_ISSUER_ID
  unset APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID
  echo "Using ad-hoc macOS signing for this unsigned build."
}
