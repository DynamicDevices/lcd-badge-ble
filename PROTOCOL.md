# DG01 e-badge — reverse-engineering notes

Working notes for the Temu-style **DG01** LCD pin (SuperBand app on iPhone). There is **no** published protocol; everything here comes from local BLE observation unless stated otherwise.

## Device summary

| Field | Value |
|--------|--------|
| GAP name | `DG01` |
| Public address | `0A:93:79:0C:DD:20` |
| App | **SuperBand** (e.g. [App Store](https://apps.apple.com/us/app/super-band/id1514892138)), vendor Shenzhen Well Fitness — generic OEM companion |
| In-app hardware string (screenshot) | `V32399` (internal build / project id; marketing / app UI — **not** the same string as GATT below) |
| GATT string (Device Information; see § Device information) | `LJ733_MB_V1.1` — likely PCB / internal hardware id (read as ASCII from a DIS characteristic) |

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
- **`dg01-ble connect` / `disconnect`:** use **`org.bluez.Device1.Connect`** / **`Disconnect`** (same as the Ubuntu **Bluetooth** settings switch). The tool sets **`Trusted=true`** before connect when allowed (useful when the UI shows **Paired: No**). No LE scan by default; **`--warm-scan-secs N`** with **`N`** > 0 runs discovery only if the device is not yet in BlueZ’s cache and you need to create the object before **`Connect`**.
- **`dg01-ble find`:** calls **`Connect()`** then performs GATT writes. **`ServicesResolved`** is **not** a prerequisite for calling **`Connect`**; GATT discovery follows the ACL. Use **`--connect-timeout-secs`**, **`--nus-profile-connect`**, or **`--reconnect`** if the link is flaky.

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

## Device information — GATT (SIG) vs FitPro UART

This is **not** a second protocol — it is **two ways** to learn about the same class of device: **standard Bluetooth** characteristics (always worth reading first) and **vendor commands** over NUS (same `SendData` / `getSetInfoByKey` as the SuperBand APK).

### Methodology reference (similar hardware, not DG01 spec)

[Hacking a FocusFit Pro-Y68 / LT716](https://xor.co.za/post/2022-11-30-hacking-smartwatch/) (xor.co.za, 2022) walks through the same **FitPro**-style stack: **Device Information Service** reads (`0x2A26` firmware string, `0x2A19` battery), **NUS** `6e400002` writes for find-device, and **jadx** on the APK. The **SoC and PCB** there (e.g. Telink TLSR8232, `LT716`) **do not** match DG01; use it as **methodology**, not as wire-level truth for this pin.

### Standard GATT reads (Linux `gatttool` or nRF Connect)

After the peripheral is connected, read DIS / battery by **16-bit UUID** (same as the article’s `gatttool --char-read --uuid=…`):

| UUID | Typical characteristic |
|------|-------------------------|
| `0x2A29` | Manufacturer Name |
| `0x2A24` | Model Number |
| `0x2A26` | Software Revision String |
| `0x2A27` | Hardware Revision String |
| `0x2A19` | Battery Level (SIG: one byte 0–100; DG01 may return **2+ octets** — first octet often still %, rest OEM/padding) |

Example (adjust MAC; use `7e400002…` NUS on DG01 for **vendor** writes, not for these reads):

```bash
gatttool -b 0A:93:79:0C:DD:20 --char-read --uuid=0x2a29
gatttool -b 0A:93:79:0C:DD:20 --char-read --uuid=0x2a24
gatttool -b 0A:93:79:0C:DD:20 --char-read --uuid=0x2a26
gatttool -b 0A:93:79:0C:DD:20 --char-read --uuid=0x2a27
gatttool -b 0A:93:79:0C:DD:20 --char-read --uuid=0x2a19
```

Decode hex payloads as **ASCII** for string characteristics (e.g. `56 30 33 37 36 37` → `V03767` on the article’s watch). **`bluetoothctl info <MAC>`** lists **Name** and **UUIDs** (DIS, Battery, NUS, `3802`, etc.) without parsing characteristics.

### Observed on DG01 (this repo)

- **`bluetoothctl info`:** name **`DG01`**, **Device Information**, **Battery**, **NUS** `7e400001…`, vendor **`3802`**, manufacturer data company id **`0xAA01`** (payload begins with BD_ADDR bytes).
- **GATT read:** at least one DIS-style read returned ASCII **`LJ733_MB_V1.1`** (internal hardware / board string). **`V32399`** in the app UI and **`LJ733_MB_V1.1`** over GATT are **different identifiers** (UI / product vs BLE-exposed revision string).
- **`dg01-ble device-info`:** reads DIS + battery service; example battery level read **`1a 00`** → **26%** with trailing **`00`** (non-SIG multi-octet pattern).
- **Vendor UART (`dg01-ble query`):** `getSetInfoByKey` (**cmd 26**) with key **20** returns a notify whose payload includes the **public address** `0a 93 79 0c dd 20`. Other keys (`1`, `10`, `12`, …) return **short `0xDC` frames**, **assembled `0xCD`**, or **sport / noise-shaped** packets — the **`query`** command only waits for the **first** notify per write; full multi-part **`0xCD`** replies may need the same **reassembly** logic as `upload-dial` (`CdNotifyAssembler`).

```bash
cd dg01-ble && cargo run --release -- query --addr 0A:93:79:0C:DD:20 \
  --info-keys 1,10,12,15,16,17,20 --dial-keys 1,2 --disconnect
```

## Hypotheses for image / video upload

1. **Chunked writes** to one of: **`4A02`**, **`AA01`**, **`AE01`**, and/or **NUS `7e400002` / `7e400003`** (with notifications on the paired notify characteristics).
2. **App** likely **precompresses** (e.g. RGB565 / raw frames / vendor container), not raw video over BLE.
3. **Next evidence:** HCI capture (Android **btsnoop** or **nRF Sniffer**) while the official app performs one **image send** — identify which **handle** receives the bulk writes.

## Tools in this repo

| Item | Purpose |
|------|--------|
| `ebadge_inspect.py` | Scan / detect `DG01`, optional GATT dump via Bleak (`--pair`, `--connect-timeout`, etc.) |
| `dg01-ble/` | Rust CLI on **Linux BlueZ** (`bluer`): `find`, **`sync-time`**, **`query`**, **`device-info`** (SIG **0x180A** + **0x180F** GATT reads, scanner-style decode), **`dial-dims`** (cmd **32** sub **2** — read firmware **width×height** like APK `getDialClockInfo`), **`upload-dial`** (cmd **31** watchface transfer; use **`--use-device-dial-dims`** to match APK `ClockDialInfoBody`), `scan` — same DBus path as **`bluetoothctl`**. Build: `cd dg01-ble && cargo build --release` |
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
| **FitPro-class tear-down (Y68 / LT716)** | [Hacking a FocusFit Pro-Y68](https://xor.co.za/post/2022-11-30-hacking-smartwatch/) — DIS + battery over GATT, NUS commands, jadx; **different** SoC than DG01 | **Methodology** and **GATT read** pattern; not a DG01 protocol spec. |
| **Gadgetbridge FitPro** | [FitPro protocol](https://gadgetbridge.org/internals/specifics/fitpro-protocol/) — `0xCD` framing; watchface often **not** implemented | Confirms OEM framing family; **no** full dial upload spec. |

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

**Linux (`dg01-ble`):** same bytes via `find` (default: write to NUS TX `7e400002…`):

```bash
cd dg01-ble && cargo run --release -- find --addr 0A:93:79:0C:DD:20
```

### Watchface / “dial” upload (command **31**)

Defined in `Profile.PBSmartBandCommandId` / `SendData` / `WatchThemeTools`:

| Sub-key | Name in code | Purpose |
|---------|----------------|--------|
| `2` | start | `getDialUpdateStartValue` — begins transfer (font slot, custom flag, RGB, optional replace position) |
| `1` | file | `getDialUpdateFileValue` — **chunked file data** |
| `3` | finish | `getDialUpdateFinishValue` — 8-byte trailer: **BE 32-bit file length** + **BE 32-bit sum of all file bytes** (`NumberUtils.intToBytes` in `calculateFinishCheckcode`; same as `dg01-ble` `dial_finish_payload`) |

**Chunking:** `WatchThemeTools` sends **200 bytes** per chunk by default, or **120** if the device config bit says so (`WRITE_MAX_SIZE`). Each **file** frame is:

- `getDialUpdateFileValue` payload = **`[seq_u16_be]`** + **chunk** + **u16 checksum** = sum of **(seq bytes + chunk bytes)** as unsigned 16-bit (see `calculateCheckcode`).

Responses are correlated by sequence in `WatchThemeTools.response` (expects status codes `1000+n` for ACK of chunk `n`, etc.).

**Firmware error statuses (`< 1000`, first payload i32 BE — `parseDialUpCode`):** the stock app maps these to user-visible failures (same as iOS behaviour when the device refuses the transfer). `dg01-ble upload-dial` aborts with an explanatory error when it sees them.

| Status | APK constant | Typical meaning |
|--------|----------------|-----------------|
| `1` | ERROR_CHECK 1003 | Check / verify failed |
| `2` | (success) | Finish success (not an error) |
| `3` | ERROR_BATTERY_LOW 1008 | Battery too low |
| `4` | ERROR_CHARGE_BATTERY 1009 | Charging — upgrade refused |
| `5` | ERROR_OUT_OF_MEMORY 1010 | OOM (cmd **31** path in `WatchThemeTools`) |
| `7` | ERROR_UNKNOWN 1007 | Not ready / fallback in APK |

**APK behaviours not fully replicated in `dg01-ble` (review):**

- **OTA firmware** (not dial **31**): the app blocks **`Constant.mCurBatteryNum <= 30`** before starting firmware OTA (`DeviceBaseFragment` → `battery_low_not_update_ota`). `dg01-ble` has no OTA command; use **`device-info`** / **`battery-watch`** for GATT % and judge manually.
- **Client-side timeouts:** `WatchThemeTools` uses **1001** (wait timeout) and **1002** (resend timeout) from timers; **`upload-dial --step-timeout-ms`** is the closest knob.
- **Resend / retry:** the APK calls `readStatus()` and resends chunks on mismatch; **`--reconnect`**, **`--blind-chunks`**, **`--chunk-gap-ms`** cover partial parity.
- **Paid watch themes:** `TabBaseFragment.getWatchChargeStatus()` gates the **H5** store — unrelated to device battery.
- **cmd 32 sub 3** “enter watch theme” UI flow (`deviceControlEnterWatchTheme`) is interactive; **`--dial32-sub3-control`** only sends the control byte if you need it for preflight parity.

**Correct width × height:** the APK does **not** hardcode a single resolution. It stores **`ClockDialInfoBody.width` / `.height`** from the device response to **`getDialClockInfo`** (**cmd 32** sub **2**), parsed in `BaseReceiveData.parseDialInfo` (same layout as `dg01-ble`’s `parse_dial_clock_info_cd`). **`dg01-ble dial-dims`** sends that read and prints raw notifies plus parsed **width** / **height** (and **RGB565** byte count). Prefer **`upload-dial --use-device-dial-dims`** so the tool reads those values before `--solid` / `--strip-bmp`; only use **`--width` / `--height`** when you already know them or are testing.

**Linux try — read dimensions only:** `cargo run --release -- dial-dims --addr <MAC> --disconnect` sends **`getDialClockInfo`** (cmd **32** sub **2**), reassembles split **`0xCD`** notifies, and prints **screenType**, **grade**, **width**, **height**, and **width×height×2** (RGB565 body size). Example on DG01: **360×360** → **259200** bytes.

**Linux try — upload:** `cargo run --release -- upload-dial --addr <MAC> --solid --use-device-dial-dims` sends a solid RGB565 test pattern at the firmware-reported size. Without that flag, defaults are 360×360 (common for this class of panel, not guaranteed). Use `--file watchface.bin` for a raw blob, or `--file export.bmp --strip-bmp` to mimic APK `getNotHeaderBmp`. If the device uses 120-byte chunks, add `--chunk 120`. Image bytes must match what the firmware expects (often RGB565 from server-exported “dial” assets — a plain PNG will not work without conversion).

**“Uploading” on the badge vs phone app:** the APK waits for **status 1000** after **cmd 31 sub 2** (start) before sending file data (`WatchThemeTools.response`). That handshake is what typically arms the on-device upload UI. **`dg01-ble`** must parse start ACKs that use **cmd 31 sub 2** in the notify (not only sub **1** chunk ACKs). If you use **`--skip-start-ack`**, you may push bytes without that handshake — transfer can still work, but the **splash / progress screen may not appear** because the firmware never saw the same sequence as the app.

**Listing “installed” watch faces:** the decompiled SuperBand / FitPro app loads **watch theme catalogues from the vendor HTTP API** (`HttpHelper` / `queryWatchThemeDetails`, etc.), not from a documented UART enumerator. Over the UART pipe, **`cmd 32`** exposes **sub-keys** such as **1** (dial transfer / status), **2** (clock/dial **info** — dimensions and model strings), **3** (device control / enter watch-theme flow) — see `SendData.getReadDialValue` / `getDialDeviceContrlReponse`. There is **no** known **`dg01-ble`** command that prints a multi-item “library of installed faces” like the app UI; inferring what is stored on the badge would require firmware-specific behaviour or a capture of any extra **`cmd 26`** keys / vendor frames not yet mapped here.

### iPhone app, watchface, and “is this firmware?”

The **SuperBand** iOS app and the decompiled **Android** APK are the same **FitPro-class** OEM stack: high-level features call into **`xfkj.fitpro`**; custom payloads are **`0xCD` frames** written to the **main UART TX** characteristic (APK **`6e400002…`**; DG01 exposes **`7e400002…`** — same layout, different first nibble). There is no separate public spec for DG01.

| What you mean | In the APK | Practical first step on Linux |
|---------------|------------|-------------------------------|
| **Library watchface / on-device dial skin** | **`WatchThemeTools`** → command **31** (sub 2 start, sub 1 file chunks, sub 3 finish) | **`dg01-ble upload-dial --protocol dial31`** (add `--preflight` / `--preflight-upload2` / `--chunk 120` if traces require it) |
| **Some other “file” or asset the app labels differently** | **`BleFileSendTools`** → command **34** (parallel chunk/start/finish framing) | **`upload-dial --protocol file34`** |
| **Actual MCU / radio firmware update** | Separate **OTA / DFU** stacks (JieLi, Realtek, Beken, Telink, … in blobs) + settings like **enter OTA mode** (`getEnterOtaMode` / cmd **18** sub **25**) | **Not** the dial **31** image bytes — needs the vendor’s DFU transport (often USB, UART, or a different BLE service/characteristic). |
| **Unknown: bulk on custom GATT (`3802` / `4A02`)** | Hypothesis only for this badge | **HCI capture** while the official app sends an image — see whether **all** bytes go to **NUS TX** or split to **`3802`** (still **open** in the table below). |

**Ground truth:** one **btsnoop** (Android SuperBand) or **Sniffer** capture during a single successful phone upload beats guessing. Align handles with `gatt_dump_dg01.txt` / `device-info` so you know which ATT destination matches **`7e400002`**.

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
