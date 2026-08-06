#!/usr/bin/env bash

set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
default_target="${repository_root}/target/tracing-overhead/default"
enabled_target="${repository_root}/target/tracing-overhead/enabled"
default_binary="${default_target}/release/bangbang"
enabled_binary="${enabled_target}/release/bangbang"
trace_marker="trace module="

cargo build \
  --manifest-path "${repository_root}/Cargo.toml" \
  --package bangbang \
  --release \
  --locked \
  --no-default-features \
  --target-dir "${default_target}"

cargo build \
  --manifest-path "${repository_root}/Cargo.toml" \
  --package bangbang \
  --release \
  --locked \
  --features tracing \
  --target-dir "${enabled_target}"

if LC_ALL=C grep -a -F -q "${trace_marker}" "${default_binary}"; then
  printf 'default release binary unexpectedly contains the trace marker\n' >&2
  exit 1
fi
if ! LC_ALL=C grep -a -F -q "${trace_marker}" "${enabled_binary}"; then
  printf 'tracing release binary does not contain the trace marker\n' >&2
  exit 1
fi

default_bytes="$(wc -c < "${default_binary}" | tr -d ' ')"
enabled_bytes="$(wc -c < "${enabled_binary}" | tr -d ' ')"
printf 'release binary bytes: default=%s tracing=%s delta=%s\n' \
  "${default_bytes}" \
  "${enabled_bytes}" \
  "$((enabled_bytes - default_bytes))"

CARGO_TARGET_DIR="${enabled_target}" cargo test \
  --manifest-path "${repository_root}/Cargo.toml" \
  --package bangbang-runtime \
  --lib \
  --release \
  --locked \
  --features tracing \
  logger::tracing::tests::reports_trace_scope_overhead \
  -- \
  --ignored \
  --nocapture
