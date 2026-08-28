#!/bin/bash
set -euo pipefail
LC_ALL=C
export LC_ALL
umask 077

usage() {
  /bin/cat <<'EOF'
Usage: scripts/prepare-elevated-vmnet-handoff.sh --output ABSOLUTE_PATH

Build and ad-hoc sign the normal networkless production bundle, bind it to the
clean source and fixed handoff implementation, and exclusively publish an
immutable package named bangbang-elevated-vmnet-handoff. Run as an ordinary
user. No Apple developer identity or provisioning profile is used.
EOF
}

output=""
output_set=false
while [[ "$#" -gt 0 ]]; do
  case "$1" in
    --output)
      if [[ "$output_set" == true ]]; then
        echo "duplicate option" >&2
        exit 2
      fi
      shift
      if [[ "$#" -eq 0 || -z "$1" ]]; then
        echo "--output requires a path" >&2
        exit 2
      fi
      output="$1"
      output_set=true
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

if [[ "$output_set" != true || "$output" != /* \
  || "$(/usr/bin/basename "$output")" != "bangbang-elevated-vmnet-handoff" ]]; then
  echo "an absolute absent bangbang-elevated-vmnet-handoff output is required" >&2
  exit 2
fi

repo_root="$(cd "$(/usr/bin/dirname "${BASH_SOURCE[0]}")/.." && pwd)"
exec /usr/bin/python3 "$repo_root/scripts/elevated_vmnet_handoff.py" \
  prepare --output "$output" </dev/null
