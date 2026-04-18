# SuperBand / FitPro APK (`xfkj.fitpro`) — parity notes for `dg01-ble`

Decompiled tree: `../superband_jadx_src/sources/` (not always in git). This document summarizes behaviour useful for the Rust tool and gaps worth implementing later.

---

## 1. Frame construction (`SendData`)

- **`getProtocol(cmd, sub, payload)`** — `0xCD` header, length, **`cmd`**, **`0x01`**, **`sub`**, payload length, payload. Matches `dg01-ble` `get_protocol` / `dial_upload::get_protocol`.
- **`getNoValueProtocol(cmd, sub)`** — 8-byte frame, no payload (e.g. **`getReadDialValue(k)`** = cmd **32**, sub **`k`**).
- **`SwitchProtocol(18, sub, byte)`** — fixed 9-byte frames for settings toggles (time sync uses **`getSetTimesValue`**, not `SwitchProtocol` alone).

**Already mirrored in Rust:** dial start/file/finish, cmd **34** file34 helpers, cmd **26** / **18** / **21** preflight pieces, `getDialDeviceContrlReponse` (32/3).

---

## 2. BLE write path (`CommandPool` + `BaseLeService`)

- **`commandPoolWriteClockDial(bytes, desc)`** queues **normal NUS TX** (`writeChar`), same as other UART traffic — no separate UUID for dial-only.
- **Default GATT fragment size:** **`addCommand(..., 20)`** — long frames split into **20-byte** writes (same default as `dg01-ble --gatt-fragment-bytes 20`).
- **Inter-write delay:** **`sendSpaceDuraion`** (default **100** ms). During watchface UI work, the app sets **`CommandPool.setSendSpaceDuraion(3)`** before starting the transfer and restores **100** after success/failure (`ClockDialListActivity` listener). **`dg01-ble`** exposes **`--gatt-fragment-gap-ms`** (default 100) — try **3** to match aggressive app timing if the link stalls.
- **Write lock:** semaphore **`write_characer_lock`** serializes GATT writes (conceptually similar to sequential `await` writes in Rust).

**Gap:** Rust does not auto-switch gap from 100 → 3 for upload; user must pass **`--gatt-fragment-gap-ms 3`** when emulating the app.

---

## 3. `WatchThemeTools` (command **31** dial)

| Topic | APK behaviour | `dg01-ble` |
|--------|----------------|------------|
| Chunk size | **`WRITE_MAX_SIZE`** **120** if **`intToBinary(config)[1]==1`**, else **200** | **`--use-device-dial-dims`** + **`apk_dial_chunk_size_from_config_byte`** |
| Start payload | Short: **font, custom, RGB** (+ optional **replacePicPos**). Extended paths combine font + image + **thumb** with OEM headers (`convertYiZhaoWeiBin`, rotation). | Minimal + **`--extended-dial-start`**, **`--strip-bmp`**, solid test pattern — full APK image pipeline not ported |
| Finish trailer | **`calculateFinishCheckcode`**: **`NumberUtils.intToBytes`** ( **big-endian** ) length + sum of all file bytes | **`dial_finish_payload`** — BE u32 length + BE u32 sum ✓ |
| Per-step timeout | Main timer **10 s**, tick **2 s**; on tick while **`j <= 8000`**, **`readStatus()`** | **`--step-timeout-ms`** (single knob); no automatic **`getDialUpdateStatus`** poll |
| Resend timer | **10 s** / **2 s** tick; **`j <= 8000`** → **`readStatus()`** | Not replicated — use **`--reconnect`** / **`--blind-chunks`** for parity experiments |
| **`readStatus()`** | **`SendData.getDialUpdateStatus()`** = **`getReadDialValue(1)`** = **`getNoValueProtocol(32, 1)`** | Not exposed as standalone CLI (could add for debugging) |
| Device errors | Status **1,3,4,5,7** → fatal (mapped to **1003,1008,…** in UI) | **`wait_notify_status`** + **`watch_theme_protocol_error_message`** ✓ |
| Client errors | **1004** missing image, **1005** font, **1006** disconnect, **1001/1002** timeouts | Partially covered (timeouts via **`step-timeout-ms`**) |

---

## 4. `BleFileSendTools` (command **34** file)

- Same **`response(int)`** state machine as dial for **≥1000** chunk ACKs and **&lt;1000** errors, but **`readStatus()`** uses **`SendData.getFileSendStatus()`** = **`getReadFileValue(1)`** = **`getNoValueProtocol(35, 1)`** — **cmd 35**, not 32.
- **`setFileSendStatus`** in **`BaseReceiveData`** only forwards to **`BleFileSendTools`** when **`resultValueItem(5) == 1`** (same sub-key routing idea as cmd 32).
- Main timeout uses **`MBInterstitialActivity.WEB_LOAD_TIME`** for millis-in-future (often **15 s** class); tick calls **`readStatus`** while **`j <= 13000`** — slightly different from **`WatchThemeTools`**.

**Gap:** `upload-dial --protocol file34` does not implement a separate **cmd 35** status poll path like the APK.

---

## 5. Incoming notify routing (`BaseReceiveData`)

- **Cmd 32**: **`setDialUpdateInfo`**. **`resultValueItem(5)`** (sub-key / “item” at byte 5): **1** → dial/file **`WatchThemeTools.response`**, **2** → **`parseDialInfo`** (clock body), **3** → **`deviceControlEnterWatchTheme`** (UI + **`getDialDeviceContrlReponse`**).
- **`parseDialUpCode`**: first **4** payload bytes as **BE int** — matches **`parse_cd_notify_status`** after **`0xCD`** header.

**Gap:** Rust does not implement the **cmd 32 sub 3** “enter watch theme” UI handshake (optional **`--dial32-sub3-control`** only sends a control byte).

---

## 6. Preflight / ordering (app vs capture)

- Real-world successful captures (`--preflight-upload2`) include **time**, **language**, **realtime step**, **dial info**, **DC shorts**, **cmd 26/18** pairs, **weather** fragments — order matters for some firmware builds.
- **`WeatherProxy`** builds **`getWeatherInfoValue`** / BLE weather blobs (server + location). Not fully replicated except via replay in **`preflight-upload2`**.

---

## 7. Client-side gates (not UART protocol)

- **`SDKCmdMannager.isConnected()`** before **`startFile`**.
- **OTA** (firmware): **`Constant.mCurBatteryNum <= 30`** blocks upgrade (`DeviceBaseFragment`) — separate from dial status **3**.
- **Paid themes:** **`HttpHelper.getWatchChargeStatus()`** — server flag, not GATT.

**Gap:** Rust has **`device-info`** / **`battery-watch`** for manual checks; no automatic “block upload if BAS &lt; X%” (firmware already returns **3** when it refuses).

---

## 8. Suggested implementation backlog (priority)

1. **Upload tuning:** document or default **lower `--gatt-fragment-gap-ms`** (e.g. **3**) when emulating successful phone captures.
2. **Optional `dial-status` subcommand:** send **`getNoValueProtocol(32, 1)`** and print one **`0xCD`** / error — mirrors APK **`readStatus()`** when debugging stalls.
3. **`file34` parity:** optional **cmd 35** read-status poll matching **`BleFileSendTools`**.
4. **Extended image pipeline:** font + 8-bit + thumb prepend — only if you need full custom-face parity (large effort; JNI **`BmpConvertTools`** in APK).
5. **Resend logic:** optional resend of last chunk on **1002**-style timeout — low priority if **`blind-chunks`** + stable power suffice.

---

## 9. Reference classes

| Class | Role |
|--------|------|
| `bluetooth.SendData` | All TX frame builders |
| `bluetooth.CommandPool` | 20-byte split, **100** ms spacing ( **3** ms during some upload UIs ) |
| `service.BaseLeService` | **`commandPoolWriteClockDial`**, GATT write |
| `bluetooth.revData.BaseReceiveData` | **`setDialUpdateInfo`**, **`parseDialUpCode`**, **`parseDialInfo`** |
| `utils.WatchThemeTools` | Dial **31** state machine, timers, errors |
| `utils.BleFileSendTools` | File **34** / **35** path |
| `activity.clockDial.WatchThemeHelper` | UI, downloads, **`startFile`** entry |

This file is descriptive; behaviour in **`dg01-ble`** may intentionally stay smaller than the full app.
