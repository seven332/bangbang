#!/bin/bash
set -euo pipefail
LC_ALL=C
export LC_ALL
umask 077

usage() {
  /bin/cat <<'EOF'
Usage: scripts/run-elevated-vmnet-handoff.sh --prepared ABSOLUTE_PATH
       --target-uid NONZERO_U32 --target-gid NONZERO_U32

Run the fixed one-shot elevated vmnet handoff against one immutable prepared
package. The caller must arrange exact-root execution externally. This entry
does not accept executable, environment, fixture, result, profile, account,
credential, interface, or socket parameters. Root actors receive only the
numeric target identity; the dropped controller resolves only its own fixed
production-session root for residue validation.
EOF
}

prepared=""
target_uid=""
target_gid=""
prepared_set=false
uid_set=false
gid_set=false
while [[ "$#" -gt 0 ]]; do
  case "$1" in
    --prepared)
      if [[ "$prepared_set" == true ]]; then
        echo "duplicate option" >&2
        exit 2
      fi
      shift
      if [[ "$#" -eq 0 || -z "$1" ]]; then
        echo "--prepared requires a path" >&2
        exit 2
      fi
      prepared="$1"
      prepared_set=true
      ;;
    --target-uid)
      if [[ "$uid_set" == true ]]; then
        echo "duplicate option" >&2
        exit 2
      fi
      shift
      if [[ "$#" -eq 0 || -z "$1" ]]; then
        echo "--target-uid requires a value" >&2
        exit 2
      fi
      target_uid="$1"
      uid_set=true
      ;;
    --target-gid)
      if [[ "$gid_set" == true ]]; then
        echo "duplicate option" >&2
        exit 2
      fi
      shift
      if [[ "$#" -eq 0 || -z "$1" ]]; then
        echo "--target-gid requires a value" >&2
        exit 2
      fi
      target_gid="$1"
      gid_set=true
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

if [[ "$prepared_set" != true || "$uid_set" != true || "$gid_set" != true \
  || "$prepared" != /* \
  || "$(/usr/bin/basename "$prepared")" != "bangbang-elevated-vmnet-handoff" \
  || ! "$target_uid" =~ ^[1-9][0-9]{0,9}$ \
  || ! "$target_gid" =~ ^[1-9][0-9]{0,9}$ ]]; then
  echo "one prepared package and canonical nonzero target ids are required" >&2
  exit 2
fi

repo_root="$(cd "$(/usr/bin/dirname "${BASH_SOURCE[0]}")/.." && pwd)"
exec /usr/bin/python3 "$repo_root/scripts/elevated_vmnet_handoff.py" \
  run-root --prepared "$prepared" \
  --target-uid "$target_uid" --target-gid "$target_gid" </dev/null
