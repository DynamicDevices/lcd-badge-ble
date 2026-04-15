# DG01 e-badge — reverse-engineering notes

Working notes for the Temu-style **DG01** LCD pin (SuperBand app on iPhone). There is **no** published protocol; everything here comes from local BLE observation unless stated otherwise.

## Device summary

| Field | Value |
|--------|--------|
| GAP name | `DG01` |
| Public address | `0A:93:79:0C:DD:20` |
| App | **SuperBand** (e.g. [App Store](https://apps.apple.com/us/app/super-band/id1514892138)), vendor Shenzhen Well Fitness — generic OEM companion |
| In-app hardware string (screenshot) | `V32399` (internal build / project id; not decoded over BLE yet) |

## Advertising

- **Local name:** `DG01`
- **Manufacturer Specific Data**
  - **Company ID:** `0xAA01` (see [Bluetooth Assigned Numbers](https://www.bluetooth.com/specifications/assigned-numbers/company-Identifiers/) — resolve the vendor by current SIG list; do not confuse with `0x00AA`.)
  - **Payload pattern:** first **6 bytes** repeat the **BD_ADDR** (`0A:93:79:0C:DD:20`); remaining bytes look **dynamic** (status / battery / build — **unknown** encoding).

## Connection behaviour (important)

- While **connected to the iPhone**, the peripheral often **does not appear** in scans from a second central (Linux). **Disconnect** the badge in the app and/or turn **phone Bluetooth off** when developing from a laptop.
- **Bleak** (Python) connect has been **unreliable** here (`TimeoutError`, `Page Timeout`) even when the device is advertising.
- **BlueZ `bluetoothctl connect <MAC>`** has succeeded when the phone is not holding the link; use **`menu gatt`** / **`list-attributes`** to enumerate GATT when Bleak fails.
- A captured session: **`gatt_dump_dg01.txt`** (raw `bluetoothctl` output).

## GATT — services and characteristics

Full UUIDs use the Bluetooth base UUID `0000xxxx-0000-1000-8000-00805f9b34fb` unless noted.

### Likely relevant to custom protocol (image / commands)

| Service UUID | Characteristics | Notes |
|--------------|-----------------|--------|
| `00003802-…` | `00004a02-…` | User Description + **CCCD** on `4A02` → strong candidate for **chunked data / notifications** (e.g. image or animation). |
| `7e400001-b5a3-f393-e0a9-e50e24dcca9d` | `7e400002…`, `7e400003…`, `7e400004…` | Same **128-bit base** as **Nordic UART Service (NUS)**. Typical use: **binary command stream** (TX/RX naming is peripheral-defined). `…04` has CCCD. |
| `0000aa00-…` | `0000aa01-…`, `0000aa02-…` | `AA02` has CCCD → **notify** path; `AA01` likely **write/command**. |
| `0000ae00-…` | `0000ae01-…`, `0000ae02-…` | `AE02` has CCCD; `AE01` **write**. |
| `0000fee7-…` | `0000fec7…`, `0000fec8…`, `0000fec9…` | SIG labels mention Tencent / Apple; on white-label devices these are often **vendor placeholders**, not literal Tencent/Apple services. Treat as **opaque** until traced. |

### Standard / boilerplate

| Service | UUID | Notes |
|---------|------|--------|
| Device Information | `0x180A` | `2A29` Mfg name, `2A24` Model, `2A25` Serial, `2A26` FW, `2A27` HW, `2A28` SW, `2A23` System ID, `2A2A` regulatory, `2A50` PnP ID |
| Battery | `0x180F` | `2A19` Battery Level |
| Heart Rate | `0x180D` | `2A37`, `2A38` — likely **SDK template**, not a real HR sensor |
| Generic Access | `0x1800` | Seen in `bluetoothctl info` cache |

**Heart Rate service:** assume **not used** for real HR unless proven; common on cheap BLE stacks copied from wearables examples.

## Hypotheses for image / video upload

1. **Chunked writes** to one of: **`4A02`**, **`AA01`**, **`AE01`**, and/or **NUS `7e400002` / `7e400003`** (with notifications on the paired notify characteristics).
2. **App** likely **precompresses** (e.g. RGB565 / raw frames / vendor container), not raw video over BLE.
3. **Next evidence:** HCI capture (Android **btsnoop** or **nRF Sniffer**) while the official app performs one **image send** — identify which **handle** receives the bulk writes.

## Tools in this repo

| Item | Purpose |
|------|--------|
| `ebadge_inspect.py` | Scan / detect `DG01`, optional GATT dump via Bleak (`--pair`, `--connect-timeout`, etc.) |
| `dg01-ble/` | Rust CLI on **Linux BlueZ** (`bluer`): `find`, **`sync-time`**, **`query`** (`getSetInfoByKey` cmd **26** / dial read cmd **32** + NUS notify), `scan` — same DBus path as **`bluetoothctl`**. Build: `cd dg01-ble && cargo build --release` |
| `capture_le_passive.sh` | `btmon` + scan (needs `sudo`); HCI to `.btsnoop` for Wireshark |
| `gatt_dump_dg01.txt` | One successful **`bluetoothctl`** `list-attributes` capture |

## Online / public sources (literature search)

Nothing on the public web documents **DG01**, **`V32399`**, or **SuperBand**-specific **image-transfer framing** for this pin. Treat the following as **analogies** and **methodology**, not a match to our wire format.

| Topic | What exists | Relevance |
|--------|-------------|-----------|
| **SuperBand app** | Marketing site ([superband.app](https://superband.app/)), Play listing under Shenzhen Well Fitness — generic **fitness band** features, **no** GATT/API docs | Confirms OEM “white label” companion; **no protocol**. |
| **16-bit service `0x3802`** | Security analysis of **COROS PACE 3** lists a custom service **`00003802-0000-1000-8000-00805f9b34fb`** alongside **Nordic UART–style** `6e400001-…` ([SySS blog](https://blog.syss.com/posts/bluetooth-analysis-coros-pace-3/)) | Shows **`3802` + UART-like service** appearing together on **unrelated** wearables — **not** DG01’s protocol, but supports “**`3802` = vendor blob**” as a pattern. |
| **Nordic UART (NUS)–like service** | Official Nordic NUS is **`6E400001-B5A3-F393-E0A9-E50E24DCCA9E`** ([Nordic documentation](https://docs.nordicsemi.com/)). Our device exposes the **same** 128-bit pattern with **`7E`** instead of **`6E`** in the first octet — common for OEM forks; treat as **UART-style pipe** until proven. |
| **Similar LCD badge hardware** | Alibaba listings for **~1.85" 360×360 IPS** “electronic badge”, e.g. **JL7014F5**, BLE **5.4**, Dongguan suppliers ([example product page](https://www.alibaba.com/product-detail/1-85-inch-Electronic-Display-Electronic_1601606442776.html)) | **Hardware class** match (screen size, BLE); **no** open protocol; chip **JL701x** is common in Chinese wearables — **does not prove** our device uses JL7014. |
| **LED matrix badges** | `FEE0`/`FEE1` and AES examples ([Stack Overflow / Bleak](https://stackoverflow.com/questions/77984711/problem-sending-data-to-a-led-name-badge-through-ble-using-bleak), [ble-led-badge](https://github.com/timhodson/ble-led-badge)) | **Different product class** (scroll text / LED), not IPS LCD — **do not** reuse packets. |
| **Reverse-engineering methodology** | Android **HCI snoop** + Wireshark; APK decompilation ([general BLE RE guide](https://reverse-engineering-ble-devices.readthedocs.io/en/latest/)) | How to get **ground truth** when the official app sends an image. |

**Bluetooth SIG company ID `0xAA01`:** resolve the current assignee in the official [Company Identifiers](https://www.bluetooth.com/specifications/assigned-numbers/company-Identifiers/) list — third-party summaries often confuse similar hex values.

## SuperBand Android APK — static analysis (jadx)

APK: `SuperBand_1.5.3_uptodown.apk` (SHA-256 `b6b71ef328215c6bc84910e501c726f27b661bbaf84d8362a41eefa3f3cbd43e`). Decompiled tree: `superband_jadx_src/` (run `jadx -d superband_jadx_src SuperBand_1.5.3_uptodown.apk` to regenerate).

### What the app actually is

- Package id **`com.legend.superband.watch`** is a thin shell; almost all BLE/UI logic lives under **`xfkj.fitpro`** (generic OEM “FitPro” white-label stack), plus vendor OTA/SDK blobs (JieLi, Realtek DFU, Beken, Telink, etc.).
- Main UART service UUID is **not** hard-coded as a Java string for `6e400001…`; it comes from resources: `res/values/strings.xml` → **`ble_main_service_uuid` = `6e400001-b5a3-f393-e0a9-e50e24dcca9d`** (standard **Nordic UART** service UUID).
- TX/RX characteristics are fixed in code as **`6e400002…`** (write) and **`6e400003…`** (notify) — see `superband_jadx_src/sources/xfkj/fitpro/bluetooth/Profile.java`.

**UUID note vs DG01 capture:** Your `bluetoothctl` dump showed a **NUS-shaped** service with **`7e400001` / `7e400002` / `7e400003`** (first octet `7E` vs Nordic `6E`). The Android build is wired for **`6E…`**. Treat this as either (a) firmware exposing a one-nibble fork of the same 128-bit pattern, or (b) a different BLE build than this APK — **try both** when implementing a central.

### Binary framing (`getProtocol` in `SendData.java`)

All “smart band” commands use a shared header. For a payload `P` of length `L`:

| Offset | Size | Content |
|--------|------|---------|
| 0 | 1 | **0xCD** (`DataPackageHead`; Java stores as `-51`) |
| 1 | 2 | Big-endian **u16**: value **`5 + L`** (i.e. **total frame length minus 3**; implementation uses the **low 16 bits** of `intToBytes(5+L)`) |
| 3 | 1 | **Command ID** (e.g. `18` = settings, `31` = dial/watchface transfer, `32` = dial read, `34` = generic file) |
| 4 | 1 | **0x01** (fixed key-length field in this implementation) |
| 5 | 1 | **Sub-key** (per command) |
| 6 | 2 | Big-endian **u16**: **`L`** (payload length) |
| 8 | `L` | Payload bytes |

Short “no payload” frames use `getNoValueProtocol` (8-byte total, same `0xCD` prefix).

Some toggles (including **Find device**) use **`SwitchProtocol`** instead — a **fixed 9-byte** frame with **no** variable-length tail:

```java
// SendData.java — values shown for find-device ON
return new byte[]{ (byte)0xCD, 0, 6, command, 1, subKey, 0, 1, valueByte };
```

### Find device / 寻找手环 (flash colours)

| Item | Source |
|------|--------|
| UI | `DeviceBaseFragment.onMCardFindClicked()` — requires `Constant.BleState == 1` (connected). Toast + `commandPoolWrite(...)`. |
| API | `SDKCmdMannager.findWatch()` — same write. |
| Builder | `SendData.getSetFindMeValue(true)` → `SwitchProtocol((byte)18, (byte)11, (byte)1)`. |

**Command IDs:** `18` = `PBSmartBandCommandIdSetting`; sub-key `11` = `PBSmartBandCommandIdSettingKeyFindMe` (`Profile.java`).

**Exact bytes (find ON):**

`cd 00 06 12 01 0b 00 01 01`

(`0x12` = 18, `0x0b` = 11, last `01` = enable.) For **off** the app would send `… 00` as the last byte (`getSetFindMeValue(false)`); the stock UI only sends **on**.

**Where to write:** the main UART **TX** characteristic — in the APK **`6e400002-b5a3-f393-e0a9-e50e24dcca9d`**. If DG01 only exposes **`7e400002-…`**, use that UUID instead (same payload).

**Repo helper:** `superband_find_device.py` connects and sends this payload (see `--help`).

### Watchface / “dial” upload (command **31**)

Defined in `Profile.PBSmartBandCommandId` / `SendData` / `WatchThemeTools`:

| Sub-key | Name in code | Purpose |
|---------|----------------|--------|
| `2` | start | `getDialUpdateStartValue` — begins transfer (font slot, custom flag, RGB, optional replace position) |
| `1` | file | `getDialUpdateFileValue` — **chunked file data** |
| `3` | finish | `getDialUpdateFinishValue` — 8-byte trailer: **LE 32-bit file length** + **LE 32-bit sum of all file bytes** (see `calculateFinishCheckcode` in `WatchThemeTools`) |

**Chunking:** `WatchThemeTools` sends **200 bytes** per chunk by default, or **120** if the device config bit says so (`WRITE_MAX_SIZE`). Each **file** frame is:

- `getDialUpdateFileValue` payload = **`[seq_u16_be]`** + **chunk** + **u16 checksum** = sum of **(seq bytes + chunk bytes)** as unsigned 16-bit (see `calculateCheckcode`).

Responses are correlated by sequence in `WatchThemeTools.response` (expects status codes `1000+n` for ACK of chunk `n`, etc.).

### Other commands worth tracing

- **`SendData.getFileDataValue` / `getFileStartValue` / `getFileFinishValue`:** command **34** — alternate “file” path used elsewhere in the app.
- **`BaseLeService`:** subscribes to notify on **`Profile.uartNotifyCharacteristicUUID`** and routes writes to **`Profile.uartWriteCharacteristicUUID`**.

### Confirmed vs still open

| Item | Status |
|------|--------|
| App-side **frame layout** for UART commands | **Confirmed** from `SendData.getProtocol` |
| **Dial** command **31** / sub-keys **1–3** / chunk **200** (or **120**) | **Confirmed** from `WatchThemeTools` + `SendData` |
| Which **GATT handle** on DG01 matches **`6e400002` vs `7e400002`** | **Open** — resolve against your `gatt_dump_dg01.txt` |
| Whether **image/video** for this badge uses cmd **31** dial path or cmd **34** or raw writes to **`3802`/`4A02`** | **Open** — needs HCI capture while uploading |

## Revision checklist

When you learn something definitive (e.g. “upload uses `4A02` only”), add a short **“Confirmed”** subsection with date and capture method; avoid duplicating UUID tables—edit the tables above instead.
