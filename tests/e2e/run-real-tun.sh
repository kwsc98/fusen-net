#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0 OR MIT

set -euo pipefail

platform="${1:-}"
scenario="${2:-native}"

if [[ "$platform" != linux ]]; then
  echo "error: Stellaris v2 real-TUN gates currently support Linux only" >&2
  exit 2
fi
if [[ "$scenario" != native && "$scenario" != all ]]; then
  echo "usage: $0 linux <native|all>" >&2
  exit 2
fi
if [[ "$(id -u)" -ne 0 ]]; then
  echo "error: Linux real-TUN scenarios require root on a disposable runner" >&2
  exit 1
fi
if [[ ! -c /dev/net/tun || ! -r /dev/net/tun || ! -w /dev/net/tun ]]; then
  echo "error: /dev/net/tun must be an accessible character device" >&2
  exit 1
fi
for command in cargo ip ping rg; do
  if ! command -v "$command" >/dev/null; then
    echo "error: $command is required" >&2
    exit 1
  fi
done

test_list="$(cargo test \
  --package stellaris \
  --test real_tun \
  --no-default-features \
  --features backend-quinn \
  --locked \
  -- \
  --list)"

run_test() {
  local test_name="$1"
  if ! rg -q "^${test_name}: test$" <<<"$test_list"; then
    echo "error: required v2 release gate '$test_name' is not implemented" >&2
    exit 1
  fi
  cargo test \
    --package stellaris \
    --test real_tun \
    --no-default-features \
    --features backend-quinn \
    --locked \
    -- \
    --exact "$test_name" \
    --ignored \
    --nocapture \
    --test-threads=1
}

run_test native_tun_protocol_and_route_lifecycle

if [[ "$scenario" == all ]]; then
  command -v tc >/dev/null || {
    echo "error: iproute2 tc is required for the full v2 gate" >&2
    exit 1
  }
  run_test linux_v2_overlay_e2e
  run_test linux_v2_server_restart
  run_test linux_v2_agent_restart
  run_test linux_v2_fault_injection
  run_test linux_v2_soak
fi
