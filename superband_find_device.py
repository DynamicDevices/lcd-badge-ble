#!/usr/bin/env python3
"""Send SuperBand / FitPro 'find device' (寻找手环) — same bytes as SendData.getSetFindMeValue(true)."""
from __future__ import annotations

import argparse
import asyncio
import sys

from bleak import BleakClient, BleakScanner

# SendData.SwitchProtocol((byte)18, (byte)11, (byte)1)
FIND_DEVICE_ON = bytes([0xCD, 0x00, 0x06, 0x12, 0x01, 0x0B, 0x00, 0x01, 0x01])

# Nordic NUS TX in APK (Profile.uartWriteCharacteristicUUID)
UART_WRITE_APK = "6e400002-b5a3-f393-e0a9-e50e24dcca9d"
# Observed on some OEM firmware (DG01 GATT dump)
UART_WRITE_ALT = "7e400002-b5a3-f393-e0a9-e50e24dcca9d"


def _norm_mac(s: str) -> str:
    s = s.strip().upper().replace("-", ":")
    parts = s.split(":")
    if len(parts) != 6:
        return s
    return ":".join(f"{int(p, 16):02X}" for p in parts)


async def _run(addr: str, write_uuid: str, pair: bool, timeout: float, warm: float) -> None:
    if warm > 0:
        await BleakScanner.discover(timeout=warm)
    async with BleakClient(addr, timeout=timeout, pair=pair) as client:
        print(f"Connected MTU={client.mtu_size}  writing {FIND_DEVICE_ON.hex()} → {write_uuid}")
        await client.write_gatt_char(write_uuid, FIND_DEVICE_ON, response=False)
        print("Write submitted (no-response). Check the badge for colours / alert.")


async def main() -> int:
    p = argparse.ArgumentParser(
        description="Send find-device BLE command (SuperBand APK: getSetFindMeValue / SwitchProtocol)."
    )
    p.add_argument("address", help="BLE address e.g. 0A:93:79:0C:DD:20")
    p.add_argument(
        "--write-uuid",
        default=UART_WRITE_ALT,
        help=f"GATT characteristic to write (default {UART_WRITE_ALT}; APK uses {UART_WRITE_APK})",
    )
    p.add_argument("--pair", action="store_true", help="Bond/pair before connect if required")
    p.add_argument("--connect-timeout", type=float, default=60.0, metavar="SEC")
    p.add_argument("--warm-scan", type=float, default=6.0, help="Seconds discovery before connect (default 6)")
    args = p.parse_args()
    addr = _norm_mac(args.address)
    try:
        await _run(addr, args.write_uuid.lower(), args.pair, args.connect_timeout, args.warm_scan)
    except Exception as e:
        print(f"Error: {e!r}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(asyncio.run(main()))
