#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${script_dir}/macos-signing-env.sh"

assert_adhoc_build_unsets_empty_notarization_environment() {
  export APPLE_SIGNING_IDENTITY=""
  export MACOS_CERTIFICATE_P12=""
  export APPLE_API_KEY=""
  export APPLE_API_KEY_ID=""
  export APPLE_API_ISSUER_ID=""
  export APPLE_ID=""
  export APPLE_PASSWORD=""
  export APPLE_TEAM_ID=""

  configure_macos_signing_environment Darwin

  [ "${APPLE_SIGNING_IDENTITY}" = "-" ]

  for name in \
    APPLE_API_KEY APPLE_API_KEY_ID APPLE_API_ISSUER_ID \
    APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID; do
    if [ -n "${!name+x}" ]; then
      echo "${name} must be absent before an ad-hoc build" >&2
      return 1
    fi
  done
}

assert_formal_signing_preserves_notarization_environment() {
  export APPLE_SIGNING_IDENTITY="Developer ID Application: Example"
  export APPLE_TEAM_ID="TEAM123"

  configure_macos_signing_environment Darwin

  [ "${APPLE_SIGNING_IDENTITY}" = "Developer ID Application: Example" ]
  [ "${APPLE_TEAM_ID}" = "TEAM123" ]
}

assert_adhoc_build_unsets_empty_notarization_environment
assert_formal_signing_preserves_notarization_environment
echo "tauri-build tests passed"
