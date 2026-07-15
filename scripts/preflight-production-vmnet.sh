#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/preflight-production-vmnet.sh --output PATH
       --signing-identity IDENTITY --provisioning-profile PATH

Assemble, inspect, and test current-host authorization for the exact production
vmnet profile without publishing Bangbang.app. Readiness prints one fixed line;
credential, profile, signing, inspection, or authorization failure prints
"bangbang vmnet preflight: blocked" and exits 3.

Options:
  --output PATH                 Intended absent destination named Bangbang.app.
  --signing-identity IDENTITY   Non-ad-hoc identity approved by the profile.
  --provisioning-profile PATH   Caller-owned Apple vmnet provisioning profile.
  -h, --help                    Show this help.
EOF
}

output=""
output_set=false
signing_identity=""
signing_identity_set=false
provisioning_profile=""
provisioning_profile_set=false

while [[ "$#" -gt 0 ]]; do
  case "$1" in
    --output)
      if [[ "$output_set" == true ]]; then
        echo "duplicate option" >&2
        usage >&2
        exit 2
      fi
      shift
      if [[ "$#" -eq 0 || -z "$1" ]]; then
        echo "--output requires a path" >&2
        usage >&2
        exit 2
      fi
      output="$1"
      output_set=true
      ;;
    --output=*)
      if [[ "$output_set" == true ]]; then
        echo "duplicate option" >&2
        usage >&2
        exit 2
      fi
      output="${1#--output=}"
      output_set=true
      ;;
    --signing-identity)
      if [[ "$signing_identity_set" == true ]]; then
        echo "duplicate option" >&2
        usage >&2
        exit 2
      fi
      shift
      if [[ "$#" -eq 0 || -z "$1" || "$1" == "-" ]]; then
        echo "--signing-identity requires a non-ad-hoc value" >&2
        usage >&2
        exit 2
      fi
      signing_identity="$1"
      signing_identity_set=true
      ;;
    --signing-identity=*)
      if [[ "$signing_identity_set" == true ]]; then
        echo "duplicate option" >&2
        usage >&2
        exit 2
      fi
      signing_identity="${1#--signing-identity=}"
      if [[ -z "$signing_identity" || "$signing_identity" == "-" ]]; then
        echo "--signing-identity requires a non-ad-hoc value" >&2
        usage >&2
        exit 2
      fi
      signing_identity_set=true
      ;;
    --provisioning-profile)
      if [[ "$provisioning_profile_set" == true ]]; then
        echo "duplicate option" >&2
        usage >&2
        exit 2
      fi
      shift
      if [[ "$#" -eq 0 || -z "$1" ]]; then
        echo "--provisioning-profile requires a path" >&2
        usage >&2
        exit 2
      fi
      provisioning_profile="$1"
      provisioning_profile_set=true
      ;;
    --provisioning-profile=*)
      if [[ "$provisioning_profile_set" == true ]]; then
        echo "duplicate option" >&2
        usage >&2
        exit 2
      fi
      provisioning_profile="${1#--provisioning-profile=}"
      if [[ -z "$provisioning_profile" ]]; then
        echo "--provisioning-profile requires a path" >&2
        usage >&2
        exit 2
      fi
      provisioning_profile_set=true
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument" >&2
      usage >&2
      exit 2
      ;;
  esac
  shift
done

if [[ "$output_set" != true || "$signing_identity_set" != true || "$provisioning_profile_set" != true ]]; then
  echo "output, signing identity, and provisioning profile are required" >&2
  usage >&2
  exit 2
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

target_triple="aarch64-apple-darwin"
cargo build \
  --quiet \
  -p bangbang \
  -p bangbang-launcher \
  --bin bangbang \
  --bin bangbang-launcher \
  --release \
  --no-default-features \
  --locked \
  --target "$target_triple"

cargo build \
  --quiet \
  -p bangbang-launcher \
  --bin bangbang-bundle \
  --release \
  --locked

"$repo_root/target/release/bangbang-bundle" \
  preflight \
  --launcher "$repo_root/target/$target_triple/release/bangbang-launcher" \
  --worker "$repo_root/target/$target_triple/release/bangbang" \
  --output "$output" \
  --signing-identity "$signing_identity" \
  --worker-profile vmnet \
  --provisioning-profile "$provisioning_profile"
