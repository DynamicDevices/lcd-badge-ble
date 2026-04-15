#!/usr/bin/env bash
# Passive LE observation from this laptop's hci0:
# - Starts an LE scan (hears public advertisements from nearby devices).
# - Records HCI to a btsnoop file via btmon.
#
# This is NOT "promiscuous mode" for other phones' BLE connections — the radio
# only reports what the stack receives while scanning (mostly ADV packets).
# If "Find my device" makes the badge advertise or change RSSI, you may see it here.
set -euo pipefail
SEC="${1:-45}"
OUT="${2:-./le_passive_$(date +%Y%m%d_%H%M%S).btsnoop}"
LOG="${OUT%.btsnoop}.txt"

if ! sudo -n true 2>/dev/null; then
  echo "Run this first in your terminal so sudo can start btmon (or enter password once):"
  echo "  sudo -v"
  echo "Then re-run this script."
  exit 1
fi

echo "Duration: ${SEC}s"
echo "btsnoop:  $OUT"
echo "btmon log: $LOG"
echo "Now: disconnect the badge from the phone OR keep phone away — then trigger Find My Device in the app."
echo ""

sudo btmon -w "$OUT" 2>"$LOG" &
BTMON_PID=$!
cleanup() {
  kill "$BTMON_PID" 2>/dev/null || true
  wait "$BTMON_PID" 2>/dev/null || true
}
trap cleanup EXIT

sleep 1
bluetoothctl power on >/dev/null 2>&1 || true
bluetoothctl pairable on >/dev/null 2>&1 || true
# --timeout: scan duration (non-interactive)
bluetoothctl --timeout "$SEC" scan on || true

echo ""
echo "Done. Open $OUT in Wireshark; skim $LOG for 'Advertising' / your MAC / DG01."
