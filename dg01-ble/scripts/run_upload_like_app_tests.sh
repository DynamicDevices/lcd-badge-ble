#!/usr/bin/env bash
# Run APK / iPhone–style DG01 watchface scenarios so you can compare behaviour with SuperBand / FitPro.
#
# Prerequisites:
#   • Linux + BlueZ, phone Bluetooth OFF, DG01 paired/connected to this machine.
#   • Real **DG01** uses NUS **7e40…** — this script does **not** pass `--apk-uart` (that is for **6e40…** FitPro UUIDs only).
#
# Usage (from repo root or from dg01-ble):
#   ./scripts/run_upload_like_app_tests.sh
#   DG01_ADDR=XX:XX:XX:XX:XX:XX ./scripts/run_upload_like_app_tests.sh
#   SKIP_FULL_UPLOAD=1 ./scripts/run_upload_like_app_tests.sh    # probes + dial-dims only (no long RGB transfer)
#   STOP_AFTER_FIRST_SUCCESS=1 ./scripts/run_upload_like_app_tests.sh
#
set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BLE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$BLE_DIR" || exit 1

DG01_ADDR="${DG01_ADDR:-0A:93:79:0C:DD:20}"
SKIP_FULL_UPLOAD="${SKIP_FULL_UPLOAD:-0}"
STOP_AFTER_FIRST_SUCCESS="${STOP_AFTER_FIRST_SUCCESS:-0}"
MIN_BATTERY="${MIN_BATTERY:-0}"

if [[ -x ./target/release/dg01-ble ]]; then
  BLE=(./target/release/dg01-ble)
else
  BLE=(cargo run --release --quiet --)
fi

COMMON=(
  --addr "$DG01_ADDR"
)
# DG01 hardware NUS (defaults) — do not add --apk-uart

PASS=0
FAIL=0
LAST_OK=""

banner() {
  echo ""
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo " $1"
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
}

run_ok() {
  local name="$1"
  shift
  banner "$name"
  "${BLE[@]}" "$@"
  local ret=$?
  if [[ "$ret" -eq 0 ]]; then
    echo ">>> OK: $name"
    PASS=$((PASS + 1))
    LAST_OK="$name"
    if [[ "$STOP_AFTER_FIRST_SUCCESS" == "1" ]]; then
      echo "STOP_AFTER_FIRST_SUCCESS=1 — exiting after first success."
      exit 0
    fi
  else
    echo ">>> FAIL: $name (exit $ret)"
    FAIL=$((FAIL + 1))
  fi
}

banner "dg01-ble — APK / iPhone–style upload matrix"
echo "  DG01_ADDR=$DG01_ADDR"
echo "  BLE=${BLE[*]}"
echo "  SKIP_FULL_UPLOAD=$SKIP_FULL_UPLOAD  STOP_AFTER_FIRST_SUCCESS=$STOP_AFTER_FIRST_SUCCESS  MIN_BATTERY=$MIN_BATTERY"
echo ""

# --- Phase A: link + DIS/BAS (no vendor UART) ---
run_ok "A0 — BlueZ link (Connected must be true)" \
  is-connected --addr "$DG01_ADDR"

run_ok "A1 — Device Information + Battery (BAS %)" \
  device-info --addr "$DG01_ADDR" --disconnect

# --- Phase B: vendor UART sanity (same NUS as upload) ---
run_ok "B1 — Dial dimensions (cmd 32/2 — APK getDialClockInfo)" \
  dial-dims --addr "$DG01_ADDR" --disconnect --notify-settle-ms 250 \
  --response-timeout-ms 15000

# --- Phase C: “uploading” UI without moving full image (start frame + 10s notify drain) ---
# Matches iPhone capture order: --preflight-upload2 (see APK_PARITY.md §6).
run_ok "C1 — Dial START only + iPhone preflight (watch badge ~10s for 'uploading' UI)" \
  upload-dial "${COMMON[@]}" --solid \
  --preflight-upload2 --reconnect --apk-parity \
  --notify-settle-ms 400 \
  --dial-start-probe --disconnect

run_ok "C2 — Dial START only + shorter preflight (--preflight, not upload-2 replay)" \
  upload-dial "${COMMON[@]}" --solid \
  --preflight --reconnect --apk-parity \
  --notify-settle-ms 400 \
  --dial-start-probe --disconnect

run_ok "C3 — Dial START only + iPhone preflight + extended cmd31/2 start (capture-shaped, 0-byte file in probe)" \
  upload-dial "${COMMON[@]}" --solid \
  --preflight-upload2 --reconnect --apk-parity \
  --extended-dial-start --dial-start-mid-from-dims \
  --notify-settle-ms 400 \
  --dial-start-probe --disconnect

# --- Phase D: full WatchThemeTools transfer (cmd 31 → chunks → finish) ---
if [[ "$SKIP_FULL_UPLOAD" != "1" ]]; then
  EXTRA=()
  if [[ "$MIN_BATTERY" != "0" ]]; then
    EXTRA+=(--min-battery-percent "$MIN_BATTERY")
  fi

  run_ok "D1 — Full solid upload — iPhone-style preflight + extended start (capture-shaped)" \
    upload-dial "${COMMON[@]}" --solid \
    --use-device-dial-dims \
    --preflight-upload2 --reconnect --apk-parity \
    --extended-dial-start --dial-start-mid-from-dims \
    --notify-settle-ms 400 \
    --dial-read-status-after-start \
    "${EXTRA[@]}" \
    --disconnect

  run_ok "D2 — Full solid upload — iPhone preflight + minimal cmd31/2 start (no extended header)" \
    upload-dial "${COMMON[@]}" --solid \
    --use-device-dial-dims \
    --preflight-upload2 --reconnect --apk-parity \
    --notify-settle-ms 400 \
    "${EXTRA[@]}" \
    --disconnect

  run_ok "D3 — Full solid upload — same as D2 but no reconnect (if D2 was flaky)" \
    upload-dial "${COMMON[@]}" --solid \
    --use-device-dial-dims \
    --preflight-upload2 --apk-parity \
    --notify-settle-ms 450 \
    "${EXTRA[@]}" \
    --disconnect

  run_ok "D4 — Full solid + APK-style cmd26 keys before preflight (hardware/product sweep)" \
    upload-dial "${COMMON[@]}" --solid \
    --use-device-dial-dims \
    --preflight-cmd26-keys 12,15,16,17,20 \
    --preflight-upload2 --reconnect --apk-parity \
    --extended-dial-start --dial-start-mid-from-dims \
    --notify-settle-ms 400 \
    "${EXTRA[@]}" \
    --disconnect
else
  echo ""
  echo "SKIP_FULL_UPLOAD=1 — skipping Phase D (full RGB565 transfer)."
fi

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo " Summary: PASS=$PASS  FAIL=$FAIL"
[[ -n "$LAST_OK" ]] && echo " Last success: $LAST_OK"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

if [[ "$FAIL" -gt 0 ]]; then
  exit 1
fi
exit 0
