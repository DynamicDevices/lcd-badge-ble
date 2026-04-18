#!/usr/bin/env bash
# Extra BLE / protocol probes when you cannot capture HCI — uses **DG01 default NUS (7e40…)** only.
#
# **Dimension hint:** preflight **`cmd32/2`** on this badge reports **360×360**. Sending a **64×64** image may
# never get a valid **start ACK (1000)** — firmware can ignore or stall. Prefer **`--use-device-dial-dims`**
# for real uploads (larger transfer). Probes **P4–P8** below use 64×64 as *negative / stress* cases; expect
# **`timeout waiting for notify`** on start if the device enforces matching size.
#
# Usage (from dg01-ble after `cargo build --release`):
#   ./scripts/run_extra_ble_probes.sh
#   DG01_ADDR=0A:93:79:0C:DD:20 ./scripts/run_extra_ble_probes.sh
#   SKIP_PROBE_UPLOAD=1 ./scripts/run_extra_ble_probes.sh   # skip the long built-in matrix
#   RUN_DIM_MATCH_UPLOAD=1 ./scripts/run_extra_ble_probes.sh  # P10: full dial dims + skip-start-ack (long)
#
set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BLE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$BLE_DIR" || exit 1

DG01_ADDR="${DG01_ADDR:-0A:93:79:0C:DD:20}"
SKIP_PROBE_UPLOAD="${SKIP_PROBE_UPLOAD:-0}"
RUN_DIM_MATCH_UPLOAD="${RUN_DIM_MATCH_UPLOAD:-0}"

if [[ -x ./target/release/dg01-ble ]]; then
  BLE=(./target/release/dg01-ble)
else
  BLE=(cargo run --release --quiet --)
fi

COMMON=(--addr "$DG01_ADDR")

PASS=0
FAIL=0

banner() {
  echo ""
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo " $1"
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
}

run_probe() {
  local name="$1"
  shift
  banner "$name"
  "${BLE[@]}" "$@"
  local ret=$?
  if [[ "$ret" -eq 0 ]]; then
    echo ">>> OK: $name"
    PASS=$((PASS + 1))
  else
    echo ">>> FAIL: $name (exit $ret)"
    FAIL=$((FAIL + 1))
  fi
}

banner "Extra BLE probes (no Wireshark) — ${DG01_ADDR}"
echo "  BLE=${BLE[*]}"

# Light UART / GATT checks
run_probe "P1 — Sync time (cmd 18/1)" \
  sync-time "${COMMON[@]}" --disconnect

run_probe "P2 — Query cmd26 key20 + cmd32/2" \
  query "${COMMON[@]}" --info-keys 20 --dial-keys 2 \
  --response-timeout-ms 4000 --gap-ms 120 --disconnect

run_probe "P3 — cmd32/1 dial-status (readStatus)" \
  dial-status "${COMMON[@]}" --response-timeout-ms 4000 --notify-settle-ms 200 --disconnect

# Small **64×64** solid uploads — faster than 360×360; may still exercise start + a few chunks.
run_probe "P4 — dial31 64×64 solid, preflight-upload2, reconnect, apk-parity" \
  upload-dial "${COMMON[@]}" --solid --width 64 --height 64 \
  --preflight-upload2 --reconnect --apk-parity \
  --notify-settle-ms 450 --disconnect

run_probe "P5 — same as P4 + loose ACK parsing" \
  upload-dial "${COMMON[@]}" --solid --width 64 --height 64 \
  --preflight-upload2 --reconnect --apk-parity --loose-ack \
  --notify-settle-ms 600 --disconnect

run_probe "P6 — dial31 64×64 + extended start (mid4 from dims)" \
  upload-dial "${COMMON[@]}" --solid --width 64 --height 64 \
  --extended-dial-start --dial-start-mid-from-dims \
  --preflight-upload2 --reconnect --apk-parity \
  --notify-settle-ms 450 --disconnect

# Optional: some firmware expects cmd32/3 **control** before start — try a harmless non-zero byte.
run_probe "P7 — 64×64 + dial32-sub3-control 1 (experimental)" \
  upload-dial "${COMMON[@]}" --solid --width 64 --height 64 \
  --preflight-upload2 --reconnect --apk-parity \
  --dial32-sub3-control 1 \
  --notify-settle-ms 450 --disconnect

# file34 on small payload (different state machine)
run_probe "P8 — file34 protocol, 64×64 solid, preflight-upload2" \
  upload-dial "${COMMON[@]}" --solid --width 64 --height 64 \
  --protocol file34 \
  --preflight-upload2 --reconnect --apk-parity \
  --notify-settle-ms 450 --disconnect

if [[ "$SKIP_PROBE_UPLOAD" != "1" ]]; then
  run_probe "P9 — Built-in probe-upload matrix (6 attempts, temp file)" \
    probe-upload "${COMMON[@]}" --test-bytes 400
else
  echo ""
  echo "SKIP_PROBE_UPLOAD=1 — skipping P9 (probe-upload)."
fi

if [[ "$RUN_DIM_MATCH_UPLOAD" == "1" ]]; then
  run_probe "P10 — dial31 solid with **device dial dims** + skip-start-ack (debug: may show progress without ACK 1000; LONG)" \
    upload-dial "${COMMON[@]}" --solid --use-device-dial-dims \
    --preflight-upload2 --reconnect --apk-parity \
    --skip-start-ack \
    --notify-settle-ms 450 --disconnect
else
  echo ""
  echo "RUN_DIM_MATCH_UPLOAD=1 — not set; skipping P10 (large 360×360 transfer)."
fi

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo " Summary: PASS=$PASS  FAIL=$FAIL"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
[[ "$FAIL" -gt 0 ]] && exit 1
exit 0
