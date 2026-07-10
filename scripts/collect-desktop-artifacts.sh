#!/usr/bin/env bash
set -euo pipefail

channel="${RELEASE_CHANNEL:-dev}"
profile="${TAURI_BUNDLE_PROFILE:-release}"
output_dir="${1:-artifacts/desktop-shell}"
bundle_dir="target/${profile}/bundle"
app_path="${bundle_dir}/macos/Codex Mobile Bridge.app"
dmg_dir="${bundle_dir}/dmg"
manifest="${output_dir}/artifact-paths.txt"

mkdir -p "${output_dir}"
rm -f "${manifest}"

record_artifact() {
  local path="$1"
  printf '%s\n' "${path}" >> "${manifest}"
}

if [ -d "${app_path}" ]; then
  app_zip="${output_dir}/Codex-Mobile-Bridge-${channel}.app.zip"
  rm -f "${app_zip}"
  ditto -c -k --sequesterRsrc --keepParent "${app_path}" "${app_zip}"
  record_artifact "${app_zip}"
fi

if [ -d "${dmg_dir}" ]; then
  while IFS= read -r dmg; do
    [ -n "${dmg}" ] || continue
    target="${output_dir}/Codex-Mobile-Bridge-${channel}.dmg"
    cp "${dmg}" "${target}"
    record_artifact "${target}"
  done < <(find "${dmg_dir}" -maxdepth 1 -type f -name "*.dmg" | sort)
fi

if [ ! -s "${manifest}" ]; then
  echo "No desktop artifacts found under ${bundle_dir}" >&2
  exit 1
fi

echo "Collected desktop artifacts:"
cat "${manifest}"
