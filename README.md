# lcd-badge-ble

BLE tooling and protocol / reverse-engineering notes for **DG01**-class LCD pins (SuperBand / FitPro-style OEM apps). Primary reference: **[PROTOCOL.md](PROTOCOL.md)**.

## Repository layout

| Path | Purpose |
|------|---------|
| `dg01-ble/` | Rust CLI on Linux (BlueZ via **bluer**): `scan`, `find`, `sync-time`, `query`, … |
| `PROTOCOL.md` | GATT map, framing, command IDs from APK analysis and local captures |
| `ebadge_inspect.py`, `superband_find_device.py` | Python helpers (Bleak path; flaky vs BlueZ in practice) |
| `capture_le_passive.sh`, `apk-get` | Shell helpers |

**Not tracked in git:** APKs, JADX output tree (`superband_jadx_src/`), and the whole `tools/` tree (local JADX install / zip) — download or regenerate locally.

## Requirements

- **dg01-ble:** Rust toolchain, Bluetooth adapter managed by BlueZ (typical Linux desktop / Pi).

## License

Tooling and documentation in this repository are provided as-is for interoperability research. Third-party apps and firmware remain under their respective terms.
