#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/sign-app-sandbox-bundle.sh INPUT OUTPUT_APP INFO_PLIST

Package INPUT as the main executable described by INFO_PLIST, then ad-hoc
sign OUTPUT_APP with the integration-only App Sandbox and Hypervisor
entitlements. OUTPUT_APP must not already exist.
EOF
}

if [[ "$#" -eq 1 ]]; then
  case "$1" in
    -h | --help)
      usage
      exit 0
      ;;
  esac
fi

if [[ "$#" -ne 3 ]]; then
  usage >&2
  exit 2
fi

input="$1"
output_app="$2"
info_plist="$3"

if [[ ! -f "$input" ]]; then
  echo "input binary does not exist or is not a regular file: $input" >&2
  exit 1
fi

if [[ ! -f "$info_plist" ]]; then
  echo "Info.plist does not exist or is not a regular file: $info_plist" >&2
  exit 1
fi

case "$output_app" in
  *.app)
    ;;
  *)
    echo "output bundle path must end in .app: $output_app" >&2
    exit 2
    ;;
esac

if [[ -e "$output_app" ]]; then
  echo "output bundle path already exists: $output_app" >&2
  exit 1
fi

for tool in codesign plutil; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "$tool is required to package App Sandbox integration tests" >&2
    exit 1
  fi
done

bundle_executable="$(plutil -extract CFBundleExecutable raw -o - "$info_plist")"
bundle_identifier="$(plutil -extract CFBundleIdentifier raw -o - "$info_plist")"

if [[ -z "$bundle_executable" \
  || "$bundle_executable" == "." \
  || "$bundle_executable" == ".." \
  || "$bundle_executable" == */* ]]; then
  echo "CFBundleExecutable must be a non-empty file name" >&2
  exit 1
fi

if [[ -z "$bundle_identifier" ]]; then
  echo "CFBundleIdentifier must not be empty" >&2
  exit 1
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
entitlements="$repo_root/scripts/app-sandbox/entitlements.plist"
contents="$output_app/Contents"
executable="$contents/MacOS/$bundle_executable"

mkdir -p -- "$contents/MacOS"
cp -p -- "$input" "$executable"
cp -p -- "$info_plist" "$contents/Info.plist"
codesign --force --sign - --entitlements "$entitlements" "$output_app"
codesign --verify --strict --verbose=4 "$output_app"

printf '%s\n' "$executable"
