# lcd-badge-ble

BLE tooling and protocol / reverse-engineering notes for **DG01**-class LCD pins (SuperBand / FitPro-style OEM apps). Primary reference: **[PROTOCOL.md](PROTOCOL.md)**.

## Repository layout

| Path | Purpose |
|------|---------|
| `dg01-ble/` | Rust CLI on Linux (BlueZ via **bluer**): `scan`, `find`, `sync-time`, `query`, `upload-dial` (cmd 31 watchface) — see **Find device** below |
| `PROTOCOL.md` | GATT map, framing, command IDs from APK analysis and local captures |
| `ebadge_inspect.py`, `superband_find_device.py` | Python helpers (Bleak path; flaky vs BlueZ in practice) |
| `capture_le_passive.sh`, `apk-get` | Shell helpers |

**Not tracked in git:** APKs, JADX output tree (`superband_jadx_src/`), and the whole `tools/` tree (local JADX install / zip) — download or regenerate locally.

## Requirements

- **dg01-ble:** Rust toolchain, Bluetooth adapter managed by BlueZ (typical Linux desktop / Pi).

## BlueZ connect / disconnect (Linux)

The **`connect`** and **`disconnect`** subcommands use the same D-Bus methods as the Ubuntu **Bluetooth** settings toggle: **`org.bluez.Device1.Connect`** and **`Disconnect`** (via [bluer](https://github.com/bluez/bluer), same path as `bluetoothctl`). Before connect, the tool sets **`Trusted=true`** when BlueZ allows it (helps **unpaired** LE peripherals — the panel shows **Paired: No** for some devices).

- **Default:** no extra LE scan before connect (`--warm-scan-secs` defaults to **0**). Use **`--warm-scan-secs N`** only if the device has never been seen by BlueZ and the D-Bus device object is missing.
- If generic **`Connect`** hangs on LE-only gear, try **`--nus-profile-connect`** ( **`ConnectProfile`** on the NUS service UUID).

```bash
cd dg01-ble && cargo run --release -- connect --addr 0A:93:79:0C:DD:20
cd dg01-ble && cargo run --release -- disconnect --addr 0A:93:79:0C:DD:20
```

## Find device (Linux)

`dg01-ble find` opens the link with **`Device1.Connect`**, then writes the find payload to the NUS TX characteristic. BlueZ returns quickly if the ACL is already up. **`--connect-timeout-secs`**, **`--nus-profile-connect`**, and **`--reconnect`** cover flaky links. See **[PROTOCOL.md](PROTOCOL.md)**.

```bash
cd dg01-ble && cargo run --release -- find --addr 0A:93:79:0C:DD:20
```

Optional **`--warm-scan-secs N`** ( **`N` > 0** ): run LE discovery only when the address is not yet in BlueZ’s cache and you need to populate the device before **`Connect`**.

If the **phone app** holds the only LE link, disconnect it or turn phone Bluetooth off so Linux can connect.

Quick BlueZ check (no `Connect`): `cargo run --release -- is-connected --addr 0A:93:79:0C:DD:20`. Exit status **1** if **`Connected`** is false (prints **`ServicesResolved`** for info only).

## License

Tooling and documentation in this repository are provided as-is for interoperability research. Third-party apps and firmware remain under their respective terms.
