#!/usr/bin/env python3
"""Scan for DG01 (or a given MAC), connect, and print GATT services/characteristics."""
from __future__ import annotations

import argparse
import asyncio
import sys

from bleak import BleakClient, BleakScanner
from bleak.backends.device import BLEDevice

DEFAULT_ADDR = "0A:93:79:0C:DD:20"
DEFAULT_NAME = "DG01"


def _mac_key(s: str) -> str:
    """Normalize to 12 hex chars for comparison (BLE may use : or -)."""
    return s.replace("-", ":").replace(":", "").lower()


def _norm_mac(s: str) -> str:
    s = s.strip().upper().replace("-", ":")
    parts = s.split(":")
    if len(parts) != 6:
        return s
    return ":".join(f"{int(p, 16):02X}" for p in parts)


async def find_by_mac(target_mac: str, timeout: float) -> BLEDevice | None:
    """Wait until advertisements from this BD_ADDR are seen (exact match on normalized MAC)."""
    want = _mac_key(target_mac)
    if len(want) != 12:
        return None

    def match(d: BLEDevice, ad) -> bool:
        return _mac_key(d.address) == want

    return await BleakScanner.find_device_by_filter(match, timeout=timeout)


async def find_by_name(name_substr: str, timeout: float) -> BLEDevice | None:
    ns = name_substr.lower()

    def match(d: BLEDevice, ad) -> bool:
        ln = (ad.local_name or "") + " " + (d.name or "")
        return ns in ln.lower()

    return await BleakScanner.find_device_by_filter(match, timeout=timeout)


async def find_device(target_mac: str | None, name_substr: str, scan_s: float) -> str | None:
    print(f"Listening up to {scan_s:.0f}s for MAC {_norm_mac(target_mac)} (and name {name_substr!r})…", flush=True)
    if target_mac:
        d = await find_by_mac(target_mac, scan_s)
        if d:
            print(f"  Detected by address: {d.address.upper()}  name={d.name!r}", flush=True)
            return d.address
    d = await find_by_name(name_substr, scan_s)
    if d:
        print(f"  Detected by name: {d.address.upper()}  name={d.name!r}", flush=True)
        return d.address
    return None


async def dump_gatt(address: str, *, pair: bool, timeout: float, warm_scan_s: float) -> None:
    if warm_scan_s > 0:
        print(f"\nWarm-up scan {warm_scan_s:.0f}s (BlueZ cache)…", flush=True)
        await BleakScanner.discover(timeout=warm_scan_s)
    print(f"\nConnecting to {address} (pair={pair}, timeout={timeout:.0f}s)…", flush=True)
    async with BleakClient(address, timeout=timeout, pair=pair) as client:
        print(f"Connected: {client.is_connected}  MTU: {client.mtu_size}")
        await asyncio.sleep(0.1)
        for svc in client.services:
            print(f"\nService {svc.uuid}")
            for ch in svc.characteristics:
                props = ",".join(ch.properties)
                extra = ""
                if ch.descriptors:
                    extra = f" desc={len(ch.descriptors)}"
                print(f"  {ch.uuid}  [{props}]{extra}")


async def main() -> int:
    p = argparse.ArgumentParser(
        description="Inspect BLE GATT on DG01 / e-badge",
        epilog=(
            "MAC matching uses the address seen in advertisements. If the badge uses a "
            "random/private address, Linux may show a different address than the iPhone until paired."
        ),
    )
    p.add_argument(
        "address",
        nargs="?",
        default=None,
        help="BLE address: skip scan and connect directly (use when you already see this MAC)",
    )
    p.add_argument(
        "--mac",
        default=DEFAULT_ADDR,
        metavar="ADDR",
        help=f"MAC to wait for when scanning (default {DEFAULT_ADDR})",
    )
    p.add_argument("--scan", type=float, default=25.0, metavar="SEC", help="How long to scan (default 25)")
    p.add_argument("--name", default=DEFAULT_NAME, help="Name substring if MAC not seen")
    p.add_argument(
        "--detect",
        action="store_true",
        help="Only try to detect the device by --mac (exit 0 if seen, 1 if not)",
    )
    p.add_argument(
        "--pair",
        action="store_true",
        help="Pair with BlueZ before connect (some devices need this)",
    )
    p.add_argument(
        "--connect-timeout",
        type=float,
        default=60.0,
        metavar="SEC",
        help="GATT connect timeout (default 60)",
    )
    p.add_argument(
        "--warm-scan",
        type=float,
        default=8.0,
        metavar="SEC",
        help="Discovery seconds before connect when using a direct address (default 8; 0 to disable)",
    )
    args = p.parse_args()

    if args.address:
        addr = _norm_mac(args.address)
        if args.detect:
            print("Use --detect without a positional address, or use --mac.", file=sys.stderr)
            return 2
    elif args.detect:
        d = await find_by_mac(args.mac, args.scan)
        if d:
            print(f"FOUND {d.address.upper()}  name={d.name!r}")
            return 0
        print(
            f"NOT FOUND: no advertisements from {_norm_mac(args.mac)} in {args.scan:.0f}s.\n"
            "If the pin is connected to the iPhone it may not advertise. Disconnect there, "
            "power-cycle the badge, and retry. If it still fails, the on-air address may differ "
            "(BLE privacy); scan with:  ./.venv/bin/python ebadge_inspect.py --scan 30  (no --detect) "
            "and look for DG01 or a new random address.",
            file=sys.stderr,
        )
        return 1
    else:
        addr = await find_device(args.mac, args.name, args.scan)

    if not addr:
        print(
            "\nNo matching device. Check: badge on/charged, within ~2 m, "
            "disconnected from iPhone (many devices stop advertising when connected).",
            file=sys.stderr,
        )
        return 1

    try:
        # Direct address: optional warm scan fills BlueZ cache. Scan-based path already discovered.
        warm = args.warm_scan if args.address is not None else 0.0
        await dump_gatt(
            addr,
            pair=args.pair,
            timeout=args.connect_timeout,
            warm_scan_s=warm,
        )
    except Exception as e:
        print(f"\nError: {e!r}", file=sys.stderr)
        print(
            "Hint: disconnect the badge from the iPhone (or turn off phone Bluetooth), "
            "keep the pin within ~1 m, then retry. "
            "Try --pair if the device requires bonding.",
            file=sys.stderr,
        )
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(asyncio.run(main()))
