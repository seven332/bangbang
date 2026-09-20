#!/bin/bash
set -euo pipefail
LC_ALL=C
export LC_ALL
umask 077

usage() {
  /bin/cat <<'EOF'
Usage: scripts/prepare-production-vmnet-certification.sh
       --config ABSOLUTE_PATH --result ABSOLUTE_PATH
       --output ABSOLUTE_PATH/bangbang-elevated-vmnet-handoff

Prepare the canonical no-Apple production vmnet certification package as an
ordinary user. This command never elevates and does not use an Apple signing
identity or provisioning profile.
EOF
}

config=""
result=""
output=""
config_set=false
result_set=false
output_set=false
while [[ "$#" -gt 0 ]]; do
  case "$1" in
    --config | --result | --output)
      option="$1"
      shift
      if [[ "$#" -eq 0 || -z "$1" ]]; then
        echo "$option requires a path" >&2
        exit 2
      fi
      case "$option" in
        --config)
          if [[ "$config_set" == true ]]; then
            echo "duplicate option" >&2
            exit 2
          fi
          config="$1"
          config_set=true
          ;;
        --result)
          if [[ "$result_set" == true ]]; then
            echo "duplicate option" >&2
            exit 2
          fi
          result="$1"
          result_set=true
          ;;
        --output)
          if [[ "$output_set" == true ]]; then
            echo "duplicate option" >&2
            exit 2
          fi
          output="$1"
          output_set=true
          ;;
      esac
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

if [[ "$config_set" != true || "$result_set" != true || "$output_set" != true \
  || "$config" != /* || "$result" != /* || "$output" != /* \
  || "$(/usr/bin/basename "$output")" != "bangbang-elevated-vmnet-handoff" ]]; then
  echo "canonical absolute config, result, and package paths are required" >&2
  exit 2
fi

repo_root="$(cd "$(/usr/bin/dirname "${BASH_SOURCE[0]}")/.." && pwd)"
exec /usr/bin/python3 "$repo_root/scripts/production_vmnet_certification.py" \
  prepare-elevated --config "$config" --result "$result" --output "$output" \
  </dev/null
