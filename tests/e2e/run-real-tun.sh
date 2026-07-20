#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0 OR MIT

set -euo pipefail

platform="${1:-}"
scenario="${2:-standard}"
case "$platform" in
  linux)
    test "$(id -u)" -eq 0 || {
      echo "error: Linux real-TUN scenarios must run as root on a disposable runner" >&2
      exit 1
    }
    test -c /dev/net/tun || {
      echo "error: /dev/net/tun is not available on this runner" >&2
      exit 1
    }
    test -r /dev/net/tun && test -w /dev/net/tun || {
      echo "error: /dev/net/tun is not readable and writable" >&2
      exit 1
    }
    command -v ip >/dev/null || {
      echo "error: iproute2 is required" >&2
      exit 1
    }
    command -v ping >/dev/null || {
      echo "error: iputils ping is required" >&2
      exit 1
    }
    if [[ "$scenario" == "fault" || "$scenario" == "all" ]]; then
      command -v tc >/dev/null || {
        echo "error: iproute2 tc with netem and u32 support is required" >&2
        exit 1
      }
    fi
    ;;
  macos)
    test "$(id -u)" -eq 0 || {
      echo "error: macOS real-TUN scenarios must run as root on a disposable runner" >&2
      exit 1
    }
    command -v ifconfig >/dev/null
    command -v route >/dev/null
    command -v ping >/dev/null
    ;;
  *)
    echo "usage: $0 <linux|macos>" >&2
    exit 2
    ;;
esac

test -f Cargo.lock || {
  echo "error: Cargo.lock must be committed" >&2
  exit 1
}

case "$scenario" in
  standard|fault|soak|all) ;;
  *)
    echo "usage: $0 <linux|macos> [standard|fault|soak|all]" >&2
    exit 2
    ;;
esac

run_test() {
  local test_name="$1"
  cargo test \
    --package fusen-net \
    --test real_tun \
    --all-features \
    --locked \
    -- \
    --exact "$test_name" \
    --ignored \
    --nocapture \
    --test-threads=1
}

run_standard() {
  run_test native_tun_protocol_and_route_lifecycle
  if [[ "$platform" == "linux" ]]; then
    local backends="${FUSEN_REAL_TUN_BACKENDS:-quinn s2n gm-quic}"
    local backend
    for backend in $backends; do
      echo "running Linux namespace E2E with backend $backend"
      FUSEN_REAL_TUN_BACKEND="$backend" run_test linux_namespace_overlay_e2e
      FUSEN_REAL_TUN_BACKEND="$backend" run_test linux_namespace_relay_restart
    done
  fi
}

run_fault() {
  local backends="${FUSEN_REAL_TUN_FAULT_BACKENDS:-${FUSEN_REAL_TUN_FAULT_BACKEND:-quinn s2n gm-quic}}"
  local backend
  for backend in $backends; do
    echo "running Linux fault injection with backend $backend"
    FUSEN_REAL_TUN_BACKEND="$backend" run_test linux_namespace_fault_injection
  done
}

run_soak() {
  local backends="${FUSEN_REAL_TUN_SOAK_BACKENDS:-${FUSEN_REAL_TUN_SOAK_BACKEND:-quinn s2n gm-quic}}"
  local backend
  for backend in $backends; do
    echo "running Linux soak with backend $backend"
    FUSEN_REAL_TUN_BACKEND="$backend" run_test linux_namespace_soak
  done
}

case "$scenario" in
  standard)
    run_standard
    ;;
  fault)
    test "$platform" = linux || {
      echo "error: fault injection is implemented only for Linux namespaces" >&2
      exit 2
    }
    run_fault
    ;;
  soak)
    test "$platform" = linux || {
      echo "error: soak is implemented only for Linux namespaces" >&2
      exit 2
    }
    run_soak
    ;;
  all)
    run_standard
    if [[ "$platform" == "linux" ]]; then
      run_fault
      run_soak
    fi
    ;;
esac

if [[ "$platform" == "macos" && "${FUSEN_REAL_TUN_REQUIRE_FULL:-0}" == "1" ]]; then
  echo "error: macOS cross-host Relay/Edge orchestration is not implemented; stable qualification remains blocked" >&2
  exit 1
fi
