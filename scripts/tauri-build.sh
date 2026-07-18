#!/usr/bin/env bash
set -euo pipefail

if [ "$(uname -s)" = "Darwin" ] \
  && [ -z "${APPLE_SIGNING_IDENTITY:-}" ] \
  && [ -z "${MACOS_CERTIFICATE_P12:-}" ]; then
  export APPLE_SIGNING_IDENTITY="-"
  echo "Using ad-hoc macOS signing for this unsigned build."
fi

npx tauri build "$@"
