#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/build-production-bundle.sh --output PATH [--signing-identity IDENTITY]
       [--worker-profile networkless|vmnet] [--provisioning-profile PATH]

Build the Apple Silicon production launcher, entitlement-free vmnet provider,
and sandbox worker, then publish the fixed Bangbang.app bundle without
replacing an existing destination.

Options:
  --output PATH                 Absent destination named Bangbang.app.
  --signing-identity IDENTITY   One identity for both code objects (default: -).
  --worker-profile PROFILE      Closed worker profile (default: networkless).
  --provisioning-profile PATH   Apple profile required only for vmnet.
  -h, --help                    Show this help.
EOF
}

output=""
output_set=false
signing_identity="-"
signing_identity_set=false
worker_profile="networkless"
worker_profile_set=false
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
      if [[ "$#" -eq 0 || -z "$1" ]]; then
        echo "--signing-identity requires a non-empty value" >&2
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
      if [[ -z "$signing_identity" ]]; then
        echo "--signing-identity requires a non-empty value" >&2
        usage >&2
        exit 2
      fi
      signing_identity_set=true
      ;;
    --worker-profile)
      if [[ "$worker_profile_set" == true ]]; then
        echo "duplicate option" >&2
        usage >&2
        exit 2
      fi
      shift
      if [[ "$#" -eq 0 || -z "$1" ]]; then
        echo "--worker-profile requires a value" >&2
        usage >&2
        exit 2
      fi
      worker_profile="$1"
      worker_profile_set=true
      ;;
    --worker-profile=*)
      if [[ "$worker_profile_set" == true ]]; then
        echo "duplicate option" >&2
        usage >&2
        exit 2
      fi
      worker_profile="${1#--worker-profile=}"
      worker_profile_set=true
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

if [[ -z "$output" ]]; then
  echo "--output is required" >&2
  usage >&2
  exit 2
fi

case "$worker_profile" in
  networkless)
    if [[ "$provisioning_profile_set" == true ]]; then
      echo "networkless profile rejects provisioning input" >&2
      usage >&2
      exit 2
    fi
    ;;
  vmnet)
    if [[ "$signing_identity_set" != true || "$signing_identity" == "-" || "$provisioning_profile_set" != true || -z "$provisioning_profile" ]]; then
      echo "vmnet profile requires named signing and provisioning input" >&2
      usage >&2
      exit 2
    fi
    ;;
  *)
    echo "invalid worker profile" >&2
    usage >&2
    exit 2
    ;;
esac

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

target_triple="aarch64-apple-darwin"
cargo build \
  -p bangbang \
  -p bangbang-launcher \
  -p bangbang-vmnet-provider \
  --bin bangbang \
  --bin bangbang-launcher \
  --bin bangbang-vmnet-provider \
  --release \
  --no-default-features \
  --locked \
  --target "$target_triple"

cargo build \
  -p bangbang-launcher \
  --bin bangbang-bundle \
  --release \
  --locked

bundle_args=(
  build
  --launcher "$repo_root/target/$target_triple/release/bangbang-launcher"
  --worker "$repo_root/target/$target_triple/release/bangbang"
  --vmnet-provider "$repo_root/target/$target_triple/release/bangbang-vmnet-provider"
  --output "$output"
  --signing-identity "$signing_identity"
  --worker-profile "$worker_profile"
)
if [[ "$provisioning_profile_set" == true ]]; then
  bundle_args+=(--provisioning-profile "$provisioning_profile")
fi

"$repo_root/target/release/bangbang-bundle" "${bundle_args[@]}"
