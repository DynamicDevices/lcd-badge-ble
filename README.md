# lcd-badge-ble

BLE tooling and protocol / reverse-engineering notes for **DG01**-class LCD pins (SuperBand / FitPro-style OEM apps). Primary reference: **[PROTOCOL.md](PROTOCOL.md)**.

## Repository layout

| Path | Purpose |
|------|---------|
| `dg01-ble/` | Rust CLI on Linux (BlueZ via **bluer**): `scan`, `find`, `sync-time`, `query`, **`device-info`**, **`battery-watch`**, **`dial-dims`**, **`upload-dial`** — see **[PROTOCOL.md](PROTOCOL.md)**; APK ↔ tool parity notes: **[dg01-ble/APK_PARITY.md](dg01-ble/APK_PARITY.md)** |
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

## Standard GATT: Device Information + Battery (`device-info`)

Subcommand **`device-info`** connects and reads:

- **Device Information (0x180A):** manufacturer, model, serial, firmware, hardware, software, system ID, IEEE regulatory, **PnP ID** — decoded like nRF Connect (UTF-8 strings; PnP fields broken out).
- **Battery Service (0x180F):** **Battery Level (0x2A19)** — SIG defines one octet 0–100 as %; some OEMs return **extra octets**; the decoded **Value** uses the **first octet** only; **`device-info`** still prints a **Raw** hex line for the full read.

If **0x180F** is absent, a one-line notice is printed; **0x180A** is still required for the command to succeed.

```bash
cd dg01-ble && cargo run --release -- device-info --addr 0A:93:79:0C:DD:20
cd dg01-ble && cargo run --release -- device-info --addr 0A:93:79:0C:DD:20 --disconnect
```

Vendor UART **`query`** (cmd 26) is separate from SIG GATT — use both if you want APK-style keys **and** standard DIS/battery reads.

## Battery level while charging (`battery-watch`)

Subcommand **`battery-watch`** connects, subscribes to **Battery Level (0x2A19) NOTIFY**, and prints a line **only when the device pushes a new value** (no periodic GATT reads). If NOTIFY is missing or subscribe fails, it falls back to polling with **`--interval-secs`** (default **10**).

Use **`--duration-secs N`** to stop after *N* seconds, or **`0`** to run until Ctrl+C. **`--disconnect`** ends the BLE session when the command exits so the adapter does not leave the link up (same idea as **`device-info --disconnect`**).

```bash
cd dg01-ble && cargo run --release -- battery-watch --addr 0A:93:79:0C:DD:20 --duration-secs 300 --disconnect
cd dg01-ble && cargo run --release -- battery-watch --addr 0A:93:79:0C:DD:20 --duration-secs 0 --disconnect
```

## Dial dimensions (`dial-dims`)

**`dial-dims`** sends the same **`getDialClockInfo`** frame as the Android app (**cmd 32** sub **2**), reassembles **`0xCD`** notifications if split, and prints **width**, **height**, and expected **RGB565** payload size. Use this before **`upload-dial`** or pass **`--use-device-dial-dims`** on upload so image size is not guessed.

```bash
cd dg01-ble && cargo run --release -- dial-dims --addr 0A:93:79:0C:DD:20 --disconnect
```

There is **no** CLI command to list the phone-style **catalogue of installed watch faces** — that UI is driven by the app’s **HTTP API**; see **[PROTOCOL.md](PROTOCOL.md)** (watchface section).

**Uploading splash on the badge:** the stock app waits for a **start ACK** (status **1000**) after **cmd 31/2** before sending chunks; **`--skip-start-ack`** skips that wait and may mean the device **never shows** the uploading screen even if data still transfers — see **[PROTOCOL.md](PROTOCOL.md)**.

## License

Tooling and documentation in this repository are provided as-is for interoperability research. Third-party apps and firmware remain under their respective terms.
