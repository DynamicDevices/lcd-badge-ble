//! `dg01-ble` — Linux-only BLE tool using BlueZ via [bluer] (same DBus path as `bluetoothctl`).
//! By default, connect-style commands do **not** run an extra LE scan before `Connect()` (same as the panel).
//! Use `--warm-scan-secs N` if `Connect()` fails because the device object is not on D-Bus yet (never seen).
//!
//! ```text
//! cd dg01-ble && cargo run --release -- find --addr 0A:93:79:0C:DD:20
//! cd dg01-ble && cargo run --release -- find --addr ... --nus-profile-connect   # if generic Connect hangs
//! cd dg01-ble && cargo run --release -- find --addr ... --connect-timeout-secs 45   # slow / flaky link
//! cd dg01-ble && cargo run --release -- find --addr ... --reconnect
//! cd dg01-ble && cargo run --release -- find --addr ... --warm-scan-secs 6
//! cd dg01-ble && cargo run --release -- find --addr ... --notify-first   # APK-style: notify on before TX
//! cd dg01-ble && cargo run --release -- sync-time --addr 0A:93:79:0C:DD:20
//! cd dg01-ble && cargo run --release -- query --addr 0A:93:79:0C:DD:20
//! cd dg01-ble && cargo run --release -- dial-dims --addr 0A:93:79:0C:DD:20
//! cd dg01-ble && cargo run --release -- upload-dial --addr ... --solid --skip-start-ack
//! cd dg01-ble && cargo run --release -- dial-status --addr ... --apk-uart
//! cd dg01-ble && cargo run --release -- file-send-status --addr ... --apk-uart
//! cd dg01-ble && cargo run --release -- scan --seconds 15
//! cd dg01-ble && cargo run --release -- is-connected --addr 0A:93:79:0C:DD:20
//! cd dg01-ble && cargo run --release -- device-info --addr 0A:93:79:0C:DD:20   # DIS + Battery (0x180A, 0x180F)
//! cd dg01-ble && cargo run --release -- battery-watch --addr 0A:93:79:0C:DD:20   # BAS NOTIFY when level changes
//! cd dg01-ble && cargo run --release -- connect --addr 0A:93:79:0C:DD:20
//! cd dg01-ble && cargo run --release -- disconnect --addr 0A:93:79:0C:DD:20
//! ```

mod dial_upload;
mod device_info_gatt;

use anyhow::{bail, Context};
use chrono::{Datelike, Local, Timelike};
use bluer::gatt::remote::{Characteristic, CharacteristicWriteRequest};
use bluer::gatt::WriteOp;
use bluer::{Adapter, AdapterEvent, Address, Device, DiscoveryFilter, DiscoveryTransport, Session};
use clap::{Parser, Subcommand};
use futures::{pin_mut, StreamExt};
use dial_upload::{
    apk_dial_chunk_size_from_config_byte, decode_dc_notify_line, dial_device_control_response,
    dial_file_frame, dial_finish_payload, dial_start, dial_start_extended, dial_start_mid4_dims_be,
    file34_file_frame, file34_finish_frame, file34_start, is_dial_start_banner_cd,
    parse_cd_notify_status, parse_dc_short, parse_dial_clock_info_cd, parse_mid4_hex,
    parse_dial_clock_info_full, parse_dial_watch_ack_status, solid_rgb565_buffer, strip_bmp_rgb565_tail,
    is_watch_theme_fatal_protocol_status, watch_theme_protocol_error_message,
    CdNotifyAssembler, DialClockInfoParsed, CMD_DIAL_TRANSFER, CMD_FILE_UART, SUB_DIAL_FINISH,
    SUB_DIAL_NOTIFY_CLOCK_INFO, SUB_DIAL_START,
};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use uuid::Uuid;

/// Command `18` = settings; sub-key `1` = sync time (`SendData.getSetTimesValue` / `SDKCmdMannager.synchronTime`).
const CMD_SETTINGS: u8 = 18;
const KEY_SYNC_TIME: u8 = 1;
/// Sub-key `11` = find-me (`SendData.getSetFindMeValue` / `PBSmartBandCommandIdSettingKeyFindMe`).
const KEY_SETTING_FIND_ME: u8 = 11;

/// NUS TX on DG01 (first octet 0x7E). SuperBand APK uses `6e400002-…`.
const DEFAULT_WRITE_UUID: &str = "7e400002-b5a3-f393-e0a9-e50e24dcca9d";
const DEFAULT_NOTIFY_UUID: &str = "7e400003-b5a3-f393-e0a9-e50e24dcca9d";
const APK_UART_TX: &str = "6e400002-b5a3-f393-e0a9-e50e24dcca9d";
const APK_UART_NOTIFY: &str = "6e400003-b5a3-f393-e0a9-e50e24dcca9d";

/// `SendData.getSetInfoByKey` → `getNoValueProtocol((byte)26, key)`.
const CMD_GET_INFO_BY_KEY: u8 = 26;
/// `SendData.getReadDialValue` → `getNoValueProtocol((byte)32, key)`.
const CMD_DIAL_READ: u8 = 32;
/// `SendData.getReadFileValue` / **`getFileSendStatus()`** → `getNoValueProtocol((byte)35, key)` (`BleFileSendTools` status poll).
const CMD_FILE_SEND_STATUS: u8 = 35;
/// `SendData.getSetLanguage` → `SwitchProtocol(18, 21, lang)`.
const KEY_SETTING_LANGUAGE: u8 = 21;
/// `SendData.getTurnOnRealTimeStep` → `SwitchProtocol(21, 6, on)`.
const CMD_SPORT: u8 = 21;
const KEY_SPORT_REALTIME_STEP: u8 = 6;
/// `BaseLeService` post-connect: `getSetInfoByKey(10)` classic Bluetooth address.
const KEY_SETTING_CLASSIC_BT_ADDR: u8 = 10;
/// `SendData.getEnterOtaMode` → `getNoValueProtocol(18, 25)`.
const KEY_ENTER_OTA_MODE: u8 = 25;

/// Short `0xDC` writes from `upload-2.log.pcapng` before dial bulk (exact bytes).
const PREFLIGHT_UPLOAD2_DC1: [u8; 8] = [0xDC, 0x00, 0x05, 0x15, 0x0C, 0x00, 0x1E, 0x01];
const PREFLIGHT_UPLOAD2_DC2: [u8; 8] = [0xDC, 0x00, 0x05, 0x20, 0x02, 0x00, 0x28, 0x01];
/// Frame **3434** — immediately before `SendData.getWeatherInfoValue` (cmd **18**/**32**) burst.
const PREFLIGHT_UPLOAD2_DC_BEFORE_WEATHER: [u8; 8] =
    [0xDC, 0x00, 0x05, 0x20, 0x03, 0x00, 0x12, 0x01];
/// Three **ATT Write Request** segments from `upload-2` frames **3454–3464** (`getWeatherInfoValue` + city **Lithuania** + tail). Replayed in order (MTU-sized fragments).
const PREFLIGHT_UPLOAD2_WEATHER_FRAG1: [u8; 20] = [
    0xCD, 0x00, 0x4D, 0x12, 0x01, 0x20, 0x00, 0x48, 0x0A, 0x4C, 0x69, 0x74, 0x68, 0x65, 0x72, 0x6C,
    0x61, 0x6E, 0x64, 0x03,
];
const PREFLIGHT_UPLOAD2_WEATHER_FRAG2: [u8; 20] = [
    0x01, 0x35, 0x26, 0x40, 0x05, 0x0A, 0x0F, 0x03, 0x03, 0x00, 0x04, 0x57, 0x16, 0x13, 0x11, 0x00,
    0x75, 0x01, 0xBC, 0x04,
];
const PREFLIGHT_UPLOAD2_WEATHER_FRAG3: [u8; 20] = [
    0x01, 0x35, 0x26, 0x42, 0x07, 0x06, 0x0C, 0x03, 0x03, 0x00, 0x04, 0x4E, 0x18, 0x1C, 0x0D, 0x00,
    0x71, 0x01, 0xC0, 0x04,
];

/// BlueZ `connect()` / GATT discovery can block indefinitely; fail fast with a clear error.
const BLE_CONNECT_TIMEOUT: Duration = Duration::from_secs(45);
const GATT_DISCOVER_TIMEOUT: Duration = Duration::from_secs(45);
const NOTIFY_ENABLE_TIMEOUT: Duration = Duration::from_secs(30);
const GATT_WRITE_TIMEOUT: Duration = Duration::from_secs(30);
/// `disconnect()` can also block indefinitely on BlueZ; don’t stall `--reconnect`.
const BLE_DISCONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// `is_connected()` can block indefinitely when bluetoothd is wedged.
const BLE_IS_CONNECTED_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Parser)]
#[command(name = "dg01-ble")]
#[command(about = "BLE utilities for DG01 / SuperBand-style devices (Linux BlueZ)", long_about = None)]
struct Cli {
    /// Bluetooth adapter name (e.g. hci0)
    #[arg(long, default_value = "hci0")]
    adapter: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Send "find device" (flash colours / alert on the badge)
    Find {
        /// Peripheral MAC, e.g. 0A:93:79:0C:DD:20
        #[arg(long)]
        addr: String,

        /// GATT characteristic UUID to write (UART TX)
        #[arg(long, default_value = DEFAULT_WRITE_UUID)]
        write_uuid: String,

        /// Use Nordic UART TX UUID from the SuperBand APK (`6e400002-…`) instead of default `7e400002-…`
        #[arg(long)]
        apk_uart: bool,

        /// Disconnect after sending (default: keep connection open)
        #[arg(long)]
        disconnect: bool,

        /// Disconnect then reconnect before writing (helps when already connected but GATT discovery hangs)
        #[arg(long)]
        reconnect: bool,

        /// Optional LE discovery before `Connect()` (only if `>0`). Default `0` = none — match Settings: call
        /// `Connect()` on the device path immediately. Use e.g. `4` if the device has never been seen by BlueZ.
        #[arg(long, default_value_t = 0)]
        warm_scan_secs: u64,

        /// Bond/pair after connect if the device requires it for writes
        #[arg(long)]
        pair: bool,

        /// Subscribe to NUS notify (`…0003`) before writing find on TX — APK-like path; **slower** and needs
        /// both characteristics discovered. Default is **off**: plain connect + write TX only (same as
        /// `superband_find_device.py` / original `dg01-ble` behaviour).
        #[arg(long)]
        notify_first: bool,

        /// With `--notify-first`: GATT write-with-response instead of write-without-response
        #[arg(long)]
        write_request: bool,

        /// With `--notify-first`: ms to wait after notify subscribe before the find write
        #[arg(long, default_value_t = 250)]
        notify_settle_ms: u64,

        /// Max seconds for BlueZ `Connect` (lower = fail faster when the phone holds the link)
        #[arg(long, default_value_t = 25)]
        connect_timeout_secs: u64,

        /// Use `ConnectProfile(NUS service)` instead of generic `Connect` — often works better for
        /// **BLE-only** peripherals when generic connect sits until timeout (try if connect hangs).
        #[arg(long)]
        nus_profile_connect: bool,
    },
    /// Sync date/time to the device (same packing as `SendData.getSetTimesValue` — local timezone)
    SyncTime {
        #[arg(long)]
        addr: String,

        #[arg(long, default_value = DEFAULT_WRITE_UUID)]
        write_uuid: String,

        #[arg(long)]
        apk_uart: bool,

        #[arg(long)]
        disconnect: bool,
    },
    /// Query device info: `getSetInfoByKey` (cmd 26) and/or dial read (cmd 32); read replies on NUS notify
    Query {
        #[arg(long)]
        addr: String,

        #[arg(long, default_value = DEFAULT_WRITE_UUID)]
        write_uuid: String,

        #[arg(long, default_value = DEFAULT_NOTIFY_UUID)]
        notify_uuid: String,

        #[arg(long)]
        apk_uart: bool,

        /// Comma-separated sub-keys for cmd 26 (see APK `getSetInfoByKey`): e.g. 16=hardware, 20=product, 1=personal
        #[arg(long, value_delimiter = ',', default_values_t = [16, 20])]
        info_keys: Vec<u8>,

        /// Optional comma-separated sub-keys for cmd 32 (`getReadDialValue`), e.g. 2 = clock/dial info
        #[arg(long, value_delimiter = ',')]
        dial_keys: Vec<u8>,

        /// Wait for one notify after each write (milliseconds)
        #[arg(long, default_value_t = 2500)]
        response_timeout_ms: u64,

        /// Pause between writes (milliseconds)
        #[arg(long, default_value_t = 150)]
        gap_ms: u64,

        #[arg(long)]
        disconnect: bool,
    },
    /// Read dial clock info (**cmd 32** sub **2** — APK `getDialClockInfo`): print raw notifies and parsed width×height.
    DialDims {
        #[arg(long)]
        addr: String,

        #[arg(long, default_value = DEFAULT_WRITE_UUID)]
        write_uuid: String,

        #[arg(long, default_value = DEFAULT_NOTIFY_UUID)]
        notify_uuid: String,

        #[arg(long)]
        apk_uart: bool,

        /// Total time to wait for a complete **cmd 32/2** `0xCD` reply (may arrive in multiple notifies).
        #[arg(long, default_value_t = 6000)]
        response_timeout_ms: u64,

        /// Milliseconds to wait after notify subscribe before writing **cmd 32/2**.
        #[arg(long, default_value_t = 200)]
        notify_settle_ms: u64,

        #[arg(long)]
        disconnect: bool,
    },
    /// Send **`getNoValueProtocol(32, 1)`** — APK **`SendData.getDialUpdateStatus()`** / **`getReadDialValue(1)`**; print notifies (debug stalled **dial31** uploads).
    DialStatus {
        #[arg(long)]
        addr: String,

        #[arg(long, default_value = DEFAULT_WRITE_UUID)]
        write_uuid: String,

        #[arg(long, default_value = DEFAULT_NOTIFY_UUID)]
        notify_uuid: String,

        #[arg(long)]
        apk_uart: bool,

        #[arg(long, default_value_t = 5000)]
        response_timeout_ms: u64,

        #[arg(long, default_value_t = 200)]
        notify_settle_ms: u64,

        #[arg(long)]
        disconnect: bool,
    },
    /// Send **`getNoValueProtocol(35, 1)`** — APK **`SendData.getFileSendStatus()`** / **`getReadFileValue(1)`**; print notifies (debug **file34** stalls).
    FileSendStatus {
        #[arg(long)]
        addr: String,

        #[arg(long, default_value = DEFAULT_WRITE_UUID)]
        write_uuid: String,

        #[arg(long, default_value = DEFAULT_NOTIFY_UUID)]
        notify_uuid: String,

        #[arg(long)]
        apk_uart: bool,

        #[arg(long, default_value_t = 5000)]
        response_timeout_ms: u64,

        #[arg(long, default_value_t = 200)]
        notify_settle_ms: u64,

        #[arg(long)]
        disconnect: bool,
    },
    /// Read standard Bluetooth **Device Information** (0x180A) and **Battery** (0x180F) services.
    /// Output is similar to nRF Connect / BLE scanner apps (DIS strings + PnP ID + battery %).
    DeviceInfo {
        #[arg(long)]
        addr: String,

        #[arg(long)]
        disconnect: bool,
    },
    /// **Battery Level (0x2A19)**: subscribe to **NOTIFY** and print when the device pushes an update; if NOTIFY is unavailable, poll with **READ** on `--interval-secs`.
    BatteryWatch {
        #[arg(long)]
        addr: String,

        /// If **Battery Level** has no NOTIFY: poll with a GATT read this often (seconds). Ignored when NOTIFY works.
        #[arg(long, default_value_t = 10)]
        interval_secs: u64,

        /// Stop after this many seconds (`0` = run until Ctrl+C).
        #[arg(long, default_value_t = 0)]
        duration_secs: u64,

        #[arg(long)]
        disconnect: bool,
    },
    /// Print `org.bluez.Device1` **Connected** / **ServicesResolved** (no `Connect`). Exit **1** if **Connected** is false.
    IsConnected {
        #[arg(long)]
        addr: String,

        /// Optional LE discovery before `Connect()` if `>0`. Default `0` = none (same as Settings).
        #[arg(long, default_value_t = 0)]
        warm_scan_secs: u64,
    },
    /// BlueZ `Connect()` / `ConnectProfile(NUS)` — same D-Bus call as the Settings connect switch (no extra steps by default).
    Connect {
        #[arg(long)]
        addr: String,

        /// Optional LE discovery before `Connect()` if `>0`. Default `0` = none (same as Settings).
        #[arg(long, default_value_t = 0)]
        warm_scan_secs: u64,

        #[arg(long, default_value_t = 25)]
        connect_timeout_secs: u64,

        /// Use `ConnectProfile` on NUS service UUID derived from `7e400002…` / `--write-uuid`
        #[arg(long)]
        nus_profile_connect: bool,

        /// UART TX UUID (used with `--nus-profile-connect` to derive service UUID)
        #[arg(long, default_value = DEFAULT_WRITE_UUID)]
        write_uuid: String,
    },
    /// BlueZ `Disconnect()` for the device MAC
    Disconnect {
        #[arg(long)]
        addr: String,

        /// Optional LE discovery before `Disconnect()` if `>0`. Default `0` = none.
        #[arg(long, default_value_t = 0)]
        warm_scan_secs: u64,
    },
    /// LE scan and print seen devices (uses BlueZ discovery)
    Scan {
        #[arg(long, default_value_t = 15)]
        seconds: u64,

        /// Only print devices whose name contains this substring (case-insensitive)
        #[arg(long)]
        name_contains: Option<String>,
    },
    /// Upload watchface / dial binary (cmd 31 — `WatchThemeTools`: start → chunks → finish)
    UploadDial {
        #[arg(long)]
        addr: String,

        #[arg(long, default_value = DEFAULT_WRITE_UUID)]
        write_uuid: String,

        #[arg(long, default_value = DEFAULT_NOTIFY_UUID)]
        notify_uuid: String,

        /// Use Nordic UART UUIDs from the **FitPro / SuperBand APK** (`6e400002` / `6e400003`) instead of the **DG01
        /// hardware** defaults (`7e400002` / `7e400003`). For a **real DG01** connected to Linux, leave this **off**
        /// — otherwise GATT lookup fails with “characteristic not found”.
        #[arg(long)]
        apk_uart: bool,

        /// Raw `.bin` read as-is (RGB565 or vendor format). Omit if `--solid`.
        #[arg(long)]
        file: Option<PathBuf>,

        /// Treat `--file` as BMP: keep last `width*height*2` bytes (APK `getNotHeaderBmp`).
        #[arg(long)]
        strip_bmp: bool,

        /// Ignore `--file`; send a solid RGB565 test pattern (`width`×`height`×2 bytes).
        #[arg(long)]
        solid: bool,

        /// Ignored when `--use-device-dial-dims` is set (dimensions come from **cmd 32** sub **2**).
        #[arg(long, default_value_t = 360)]
        width: u16,

        /// Ignored when `--use-device-dial-dims` is set.
        #[arg(long, default_value_t = 360)]
        height: u16,

        /// Read `width`/`height` from the badge via **cmd 32** sub **2** (`getDialClockInfo`), same fields as
        /// APK `BaseReceiveData.parseDialInfo` → `ClockDialInfoBody`. Use this instead of guessing 360×360.
        #[arg(long)]
        use_device_dial_dims: bool,

        /// Little-endian RGB565 value per pixel when `--solid` (default 63488 = red `0xF800`).
        #[arg(long, default_value_t = 63488)]
        solid_rgb565: u16,

        /// Bytes per chunk (APK uses 200, or 120 if device reports small MTU).
        #[arg(long, default_value_t = 200)]
        chunk: usize,

        #[arg(long, default_value_t = 0)]
        font_pos: u8,

        #[arg(long)]
        custom: bool,

        #[arg(long, default_value_t = 255)]
        rgb_r: u8,

        #[arg(long, default_value_t = 255)]
        rgb_g: u8,

        #[arg(long, default_value_t = 255)]
        rgb_b: u8,

        /// Append one byte after RGB in cmd 31 sub 2 (APK `getReplacePicPos` when device has `pictureNums > 0`).
        #[arg(long)]
        replace_pic_pos: Option<u8>,

        /// Use the **17-byte** dial start payload from Wireshark `upload-2` (mid4 + RGB + BE file length + 4 zero),
        /// instead of the minimal 5-byte `WatchThemeTools` start. Incompatible with `--replace-pic-pos` and `file34`.
        #[arg(long)]
        extended_dial_start: bool,

        /// Four payload bytes between font/custom and RGB in `--extended-dial-start` (8 hex digits). Default: **`15a20008`**
        /// (value from `logs/upload-2.log.pcapng`). Ignored if **`--dial-start-mid-from-dims`** is set.
        #[arg(long, default_value = "15a20008")]
        dial_start_mid4: String,

        /// Set extended start **`mid4`** from **`width`** and **`height`** as two big-endian u16 (same order as dial info).
        #[arg(long)]
        dial_start_mid_from_dims: bool,

        /// Per-step notify wait (APK main timer **~10 s** for dial/file transfer).
        #[arg(long, default_value_t = 10000)]
        step_timeout_ms: u64,

        #[arg(long)]
        disconnect: bool,

        /// Disconnect then reconnect before transfer (helps if notify stream was idle / stale).
        #[arg(long)]
        reconnect: bool,

        /// `dial31` = `WatchThemeTools` (cmd 31). `file34` = `BleFileSendTools` (cmd 34).
        #[arg(long, default_value = "dial31")]
        protocol: String,

        /// Sleep after subscribing to NUS notify before first write (milliseconds).
        #[arg(long, default_value_t = 250)]
        notify_settle_ms: u64,

        /// Send time sync + dial read keys 2 and 1 before upload (matches common app order).
        #[arg(long)]
        preflight: bool,

        /// Use the init sequence from `logs/upload-2.log.pcapng` (time → language → realtime step → dial
        /// 32/2 → two `0xDC` frames → settings 18/10 → repeated dial 32/3 → **third** `0xDC` → **weather**
        /// cmd **18**/**32** in three ATT segments — same order as successful iPhone capture; **no** cmd32/1
        /// read before dial start). Implies `--preflight`.
        #[arg(long)]
        preflight_upload2: bool,

        /// APK `getSetInfoByKey` — **cmd 26** sub-keys (comma-separated), e.g. `12,15,16,17,20`. Runs
        /// after subscribe, **before** main `--preflight` / `--preflight-upload2`.
        #[arg(long, value_delimiter = ',')]
        preflight_cmd26_keys: Vec<u8>,

        /// `SendData.getEnterOtaMode` → `getNoValueProtocol(18, 25)` (after cmd26 keys, before main preflight).
        #[arg(long)]
        enter_ota_mode: bool,

        /// `SendData.getDialDeviceContrlReponse` — **cmd 32** sub **3** with this **one** data byte (after
        /// main preflight, before dial/file start; APK uses a device-dependent control value).
        #[arg(long)]
        dial32_sub3_control: Option<u8>,

        /// Do not wait for notify status **1000** after dial start (cmd 31 sub 2). Prefer leaving this
        /// **off** so the firmware completes the same handshake as the APK (often required for the badge’s
        /// “uploading” screen). Only enable when the device never sends a parsable start ACK (e.g. some
        /// captures show chunks before any start notify).
        #[arg(long)]
        skip_start_ack: bool,

        /// After sending finish (cmd 31 sub 3), do not wait for notify status **2** — exit after a short
        /// drain (for devices that apply the image but omit or reshape the final ACK).
        #[arg(long)]
        skip_finish_ack: bool,

        /// Treat any complete `0xCD` payload int as ACK (not only cmd 32/31 sub 1). Noisier; for RE only.
        #[arg(long)]
        loose_ack: bool,

        /// After start handling, send chunk frames **without** waiting for per-chunk notify ACKs (short
        /// delay between chunks). Still waits for **finish** ACK unless you debug further. Use when the
        /// device accepts writes but does not emit per-chunk notifications on this central (see capture).
        #[arg(long)]
        blind_chunks: bool,

        /// FitPro-style upload tuning: forces **`--step-timeout-ms`** to at least **10000** and **`--gatt-fragment-gap-ms`** to **3** (chunk/dial-config rules unchanged).
        #[arg(long)]
        apk_parity: bool,

        /// Bytes per **GATT** write segment (**FitPro `CommandPool`** default **20**). Use **`0`** for one write
        /// per protocol frame (faster debug, not APK-like).
        #[arg(long)]
        gatt_fragment_bytes: Option<usize>,

        /// Milliseconds between **GATT** fragments (FitPro uses **~3** ms during watchface upload, **~100** ms
        /// when idle). Ignored when fragment size is **0**.
        #[arg(long, default_value_t = 3)]
        gatt_fragment_gap_ms: u64,

        /// If **>0**, read **BAS** (0x180F / 0x2A19) after connect and abort when level is below this (**0** = off).
        #[arg(long, default_value_t = 0)]
        min_battery_percent: u8,

        /// On **chunk** ACK timeout, resend the same chunk up to this many **extra** writes (**0** = no resend).
        #[arg(long, default_value_t = 1)]
        chunk_write_retries: u32,

        /// After **dial31** start ACK **1000**, send **`getNoValueProtocol(32,1)`** and drain (APK **`readStatus()`**).
        #[arg(long)]
        dial_read_status_after_start: bool,

        /// After **file34** start ACK **1000**, send **`getNoValueProtocol(35,1)`** and drain (**`getFileSendStatus()`**).
        #[arg(long)]
        file_read_status_after_start: bool,

        /// Only accept cmd **31**/status **1000** for dial start (omit this; APK also treats **0x15**/**0x0c** as start banner).
        #[arg(long)]
        strict_start_ack: bool,

        /// Set **`--chunk`** from dial **`config`** (**120** vs **200**, `WatchThemeTools.startFile`). On by
        /// default whenever **`--use-device-dial-dims`** supplies **`config`**; **`--manual-chunk`** skips.
        #[arg(long)]
        apk_auto_chunk: bool,

        /// Keep **`--chunk`** as given (do not apply **`config`** 120/200 rule).
        #[arg(long)]
        manual_chunk: bool,

        /// Extra milliseconds to sleep after each chunk when `--blind-chunks` is set (in addition to the
        /// built-in 15 ms). Helps long 360×360 uploads when the link drops mid-transfer (`Not connected`).
        #[arg(long, default_value_t = 0)]
        chunk_gap_ms: u64,

        /// Send **dial start only** (cmd 31 sub 2), then print all notifications for ~10s and exit.
        /// Use to see whether the badge shows an “uploading” state and what ACK shape the firmware sends.
        #[arg(long)]
        dial_start_probe: bool,
    },
    /// Run several upload combinations (small payload) to see if any ACK pattern appears.
    ProbeUpload {
        #[arg(long)]
        addr: String,

        #[arg(long)]
        apk_uart: bool,

        /// Bytes for test file (default 400).
        #[arg(long, default_value_t = 400)]
        test_bytes: usize,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let session = Session::new().await.context("DBus session (is bluetoothd running?)")?;
    let adapter = session.adapter(&cli.adapter).context("adapter")?;
    adapter.set_powered(true).await.context("set_powered")?;

    match cli.command {
        Command::Find {
            addr,
            write_uuid,
            apk_uart,
            disconnect,
            reconnect,
            warm_scan_secs,
            pair,
            notify_first,
            write_request,
            notify_settle_ms,
            connect_timeout_secs,
            nus_profile_connect,
        } => {
            let uuid = if apk_uart {
                APK_UART_TX.to_string()
            } else {
                write_uuid
            };
            cmd_find(
                &adapter,
                &addr,
                &uuid,
                disconnect,
                reconnect,
                warm_scan_secs,
                pair,
                notify_first,
                write_request,
                notify_settle_ms,
                Duration::from_secs(connect_timeout_secs.max(1)),
                nus_profile_connect,
            )
            .await?;
        }
        Command::SyncTime {
            addr,
            write_uuid,
            apk_uart,
            disconnect,
        } => {
            let uuid = if apk_uart {
                APK_UART_TX.to_string()
            } else {
                write_uuid
            };
            cmd_sync_time(&adapter, &addr, &uuid, disconnect).await?;
        }
        Command::Query {
            addr,
            write_uuid,
            notify_uuid,
            apk_uart,
            info_keys,
            dial_keys,
            response_timeout_ms,
            gap_ms,
            disconnect,
        } => {
            let (w, n) = if apk_uart {
                (APK_UART_TX.to_string(), APK_UART_NOTIFY.to_string())
            } else {
                (write_uuid, notify_uuid)
            };
            cmd_query(
                &adapter,
                &addr,
                &w,
                &n,
                &info_keys,
                &dial_keys,
                response_timeout_ms,
                gap_ms,
                disconnect,
            )
            .await?;
        }
        Command::DialDims {
            addr,
            write_uuid,
            notify_uuid,
            apk_uart,
            response_timeout_ms,
            notify_settle_ms,
            disconnect,
        } => {
            let (w, n) = if apk_uart {
                (APK_UART_TX.to_string(), APK_UART_NOTIFY.to_string())
            } else {
                (write_uuid, notify_uuid)
            };
            cmd_dial_dims(
                &adapter,
                &addr,
                &w,
                &n,
                response_timeout_ms,
                notify_settle_ms,
                disconnect,
            )
            .await?;
        }
        Command::DialStatus {
            addr,
            write_uuid,
            notify_uuid,
            apk_uart,
            response_timeout_ms,
            notify_settle_ms,
            disconnect,
        } => {
            let (w, n) = if apk_uart {
                (APK_UART_TX.to_string(), APK_UART_NOTIFY.to_string())
            } else {
                (write_uuid, notify_uuid)
            };
            cmd_uart_read_status(
                &adapter,
                &addr,
                &w,
                &n,
                CMD_DIAL_READ,
                1,
                "getDialUpdateStatus / getReadDialValue(1) — cmd32/1",
                response_timeout_ms,
                notify_settle_ms,
                disconnect,
            )
            .await?;
        }
        Command::FileSendStatus {
            addr,
            write_uuid,
            notify_uuid,
            apk_uart,
            response_timeout_ms,
            notify_settle_ms,
            disconnect,
        } => {
            let (w, n) = if apk_uart {
                (APK_UART_TX.to_string(), APK_UART_NOTIFY.to_string())
            } else {
                (write_uuid, notify_uuid)
            };
            cmd_uart_read_status(
                &adapter,
                &addr,
                &w,
                &n,
                CMD_FILE_SEND_STATUS,
                1,
                "getFileSendStatus / getReadFileValue(1) — cmd35/1",
                response_timeout_ms,
                notify_settle_ms,
                disconnect,
            )
            .await?;
        }
        Command::DeviceInfo { addr, disconnect } => {
            cmd_device_info(&adapter, &addr, disconnect).await?;
        }
        Command::BatteryWatch {
            addr,
            interval_secs,
            duration_secs,
            disconnect,
        } => {
            cmd_battery_watch(
                &adapter,
                &addr,
                interval_secs,
                duration_secs,
                disconnect,
            )
            .await?;
        }
        Command::Scan {
            seconds,
            name_contains,
        } => {
            cmd_scan(&adapter, seconds, name_contains.as_deref()).await?;
        }
        Command::IsConnected {
            addr,
            warm_scan_secs,
        } => {
            cmd_is_connected(&adapter, &addr, warm_scan_secs).await?;
        }
        Command::Connect {
            addr,
            warm_scan_secs,
            connect_timeout_secs,
            nus_profile_connect,
            write_uuid,
        } => {
            let nus_profile = if nus_profile_connect {
                Some(nus_service_uuid_from_tx(&write_uuid)?)
            } else {
                None
            };
            cmd_connect(
                &adapter,
                &addr,
                warm_scan_secs,
                Duration::from_secs(connect_timeout_secs.max(1)),
                nus_profile,
            )
            .await?;
        }
        Command::Disconnect {
            addr,
            warm_scan_secs,
        } => {
            cmd_disconnect(&adapter, &addr, warm_scan_secs).await?;
        }
        Command::UploadDial {
            addr,
            write_uuid,
            notify_uuid,
            apk_uart,
            file,
            strip_bmp,
            solid,
            width,
            height,
            use_device_dial_dims,
            solid_rgb565,
            chunk,
            font_pos,
            custom,
            rgb_r,
            rgb_g,
            rgb_b,
            replace_pic_pos,
            extended_dial_start,
            dial_start_mid4,
            dial_start_mid_from_dims,
            step_timeout_ms,
            disconnect,
            reconnect,
            protocol,
            notify_settle_ms,
            preflight,
            preflight_upload2,
            skip_start_ack,
            skip_finish_ack,
            loose_ack,
            blind_chunks,
            apk_parity,
            gatt_fragment_bytes,
            gatt_fragment_gap_ms,
            strict_start_ack,
            apk_auto_chunk,
            manual_chunk,
            chunk_gap_ms,
            dial_start_probe,
            preflight_cmd26_keys,
            enter_ota_mode,
            dial32_sub3_control,
            min_battery_percent,
            chunk_write_retries,
            dial_read_status_after_start,
            file_read_status_after_start,
        } => {
            let (w, n) = if apk_uart {
                (APK_UART_TX.to_string(), APK_UART_NOTIFY.to_string())
            } else {
                (write_uuid, notify_uuid)
            };
            // FitPro: CommandPool uses 20-byte GATT segments; omit flag → 20. Pass --gatt-fragment-bytes 0 for MTU.
            let gatt_fragment = gatt_fragment_bytes.unwrap_or(20);
            // FitPro: accept cmd 0x15/0x0c start banner unless user forces strict cmd31-only ACK.
            let apk_loose_start = !strict_start_ack;
            // WatchThemeTools: WRITE_MAX_SIZE 120 vs 200 from dial config — apply when using device dial info.
            let apply_chunk_from_dial_config =
                !manual_chunk && (apk_auto_chunk || apk_parity || use_device_dial_dims);
            let mut step_timeout_ms = step_timeout_ms;
            let mut gatt_fragment_gap_ms = gatt_fragment_gap_ms;
            if apk_parity {
                step_timeout_ms = step_timeout_ms.max(10_000);
                gatt_fragment_gap_ms = 3;
            }
            cmd_upload_dial(
                &adapter,
                &addr,
                &w,
                &n,
                file.as_deref(),
                strip_bmp,
                solid,
                width,
                height,
                use_device_dial_dims,
                solid_rgb565,
                chunk,
                font_pos,
                custom,
                rgb_r,
                rgb_g,
                rgb_b,
                replace_pic_pos,
                extended_dial_start,
                dial_start_mid4,
                dial_start_mid_from_dims,
                step_timeout_ms,
                disconnect,
                reconnect,
                &protocol,
                notify_settle_ms,
                preflight,
                preflight_upload2,
                skip_start_ack,
                skip_finish_ack,
                loose_ack,
                blind_chunks,
                chunk_gap_ms,
                dial_start_probe,
                &preflight_cmd26_keys,
                enter_ota_mode,
                dial32_sub3_control,
                min_battery_percent,
                chunk_write_retries,
                dial_read_status_after_start,
                file_read_status_after_start,
                gatt_fragment,
                gatt_fragment_gap_ms,
                apk_loose_start,
                apply_chunk_from_dial_config,
            )
            .await?;
        }
        Command::ProbeUpload {
            addr,
            apk_uart,
            test_bytes,
        } => {
            cmd_probe_upload(&adapter, &addr, apk_uart, test_bytes).await?;
        }
    }
    Ok(())
}

async fn cmd_find(
    adapter: &Adapter,
    addr_str: &str,
    write_uuid_str: &str,
    disconnect_after: bool,
    reconnect: bool,
    warm_scan_secs: u64,
    pair: bool,
    notify_first: bool,
    write_with_response: bool,
    notify_settle_ms: u64,
    connect_timeout: Duration,
    nus_profile_connect: bool,
) -> anyhow::Result<()> {
    let find_on = switch_protocol(CMD_SETTINGS, KEY_SETTING_FIND_ME, 1);
    let nus_profile = if nus_profile_connect {
        Some(nus_service_uuid_from_tx(write_uuid_str)?)
    } else {
        None
    };

    if notify_first {
        let (device, write_ch, notify_ch) = connect_nus_tx_notify(
            adapter,
            addr_str,
            write_uuid_str,
            reconnect,
            warm_scan_secs,
            pair,
            connect_timeout,
            nus_profile,
        )
        .await?;

        println!("Subscribing to NUS notify (…0003) before find write (`--notify-first`)");
        let notify_stream = tokio::time::timeout(NOTIFY_ENABLE_TIMEOUT, notify_ch.notify())
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "notify subscribe timed out after {:?}",
                    NOTIFY_ENABLE_TIMEOUT
                )
            })?
            .context("notify()")?;
        pin_mut!(notify_stream);
        if notify_settle_ms > 0 {
            println!("Notify settle {notify_settle_ms} ms…");
            tokio::time::sleep(Duration::from_millis(notify_settle_ms)).await;
        }

        let op = if write_with_response {
            WriteOp::Request
        } else {
            WriteOp::Command
        };
        let write_req = CharacteristicWriteRequest {
            op_type: op,
            ..Default::default()
        };
        println!(
            "Writing find payload: {} ({})",
            hex(&find_on),
            if write_with_response {
                "write-with-response"
            } else {
                "write-without-response"
            }
        );
        tokio::time::timeout(GATT_WRITE_TIMEOUT, write_ch.write_ext(&find_on, &write_req))
            .await
            .map_err(|_| anyhow::anyhow!("GATT write timed out after {:?}", GATT_WRITE_TIMEOUT))?
            .context("write")?;
        drop(notify_stream);
        println!("Done. Check the device for locate / colours.");

        if disconnect_after {
            device_disconnect_best_effort(&device).await;
            println!("Disconnected.");
        }
        return Ok(());
    }

    // Default: same as original `dg01-ble` / `superband_find_device.py` — one TX characteristic, one
    // write-without-response (`bluer` default), no notify subscription (faster GATT path).
    let (device, ch) = connect_uart(
        adapter,
        addr_str,
        write_uuid_str,
        reconnect,
        warm_scan_secs,
        pair,
        connect_timeout,
        nus_profile,
    )
    .await?;

    println!("Writing find payload: {}", hex(&find_on));
    tokio::time::timeout(GATT_WRITE_TIMEOUT, ch.write(&find_on))
        .await
        .map_err(|_| anyhow::anyhow!("GATT write timed out after {:?}", GATT_WRITE_TIMEOUT))?
        .context("write")?;
    println!("Done. Check the device for locate / colours.");

    if disconnect_after {
        device_disconnect_best_effort(&device).await;
        println!("Disconnected.");
    }
    Ok(())
}

async fn cmd_sync_time(
    adapter: &Adapter,
    addr_str: &str,
    write_uuid_str: &str,
    disconnect_after: bool,
) -> anyhow::Result<()> {
    let (device, ch) = connect_uart(
        adapter,
        addr_str,
        write_uuid_str,
        false,
        0,
        false,
        BLE_CONNECT_TIMEOUT,
        None,
    )
    .await?;
    let payload = build_set_times_value();
    println!(
        "Local time: {}",
        Local::now().format("%Y-%m-%d %H:%M:%S %:z")
    );
    println!("Writing time sync: {}", hex(&payload));
    tokio::time::timeout(GATT_WRITE_TIMEOUT, ch.write(&payload))
        .await
        .map_err(|_| anyhow::anyhow!("GATT write timed out after {:?}", GATT_WRITE_TIMEOUT))?
        .context("write")?;
    println!("Done. If supported, the device clock should match this machine’s local time.");

    if disconnect_after {
        device_disconnect_best_effort(&device).await;
        println!("Disconnected.");
    }
    Ok(())
}

/// `SendData.getProtocol` — matches `xfkj.fitpro.bluetooth.SendData.getProtocol`.
fn get_protocol(cmd: u8, subkey: u8, payload: &[u8]) -> Vec<u8> {
    const BASE: usize = 8;
    let total = BASE + payload.len();
    let mut out = vec![0u8; total];
    out[0] = 0xCD;
    let len_field = (total - 3) as u32;
    let lb = len_field.to_be_bytes();
    out[1] = lb[2];
    out[2] = lb[3];
    out[3] = cmd;
    out[4] = 1;
    out[5] = subkey;
    let plen = payload.len() as u32;
    let pb = plen.to_be_bytes();
    out[6] = pb[2];
    out[7] = pb[3];
    out[8..].copy_from_slice(payload);
    out
}

/// `SendData.getSetTimesValue` — packed u32 from `DateUtils.getDate()` fields.
fn build_set_times_value() -> Vec<u8> {
    let now = Local::now();
    let year = now.year();
    let month = now.month();
    let day = now.day();
    let hour = now.hour();
    let minute = now.minute();
    let second = now.second();

    let y = (year - 2000).clamp(0, 63) as u32;
    let packed: u32 = (second & 0x3f)
        | (y << 26)
        | (month << 22)
        | (day << 17)
        | (hour << 12)
        | (minute << 6);

    get_protocol(
        CMD_SETTINGS,
        KEY_SYNC_TIME,
        &packed.to_be_bytes(),
    )
}

/// `SendData.SwitchProtocol` — fixed 9-byte frames (`getSetLanguage`, `getTurnOnRealTimeStep`, find-me, etc.).
fn switch_protocol(cmd: u8, subkey: u8, value: u8) -> [u8; 9] {
    [0xCD, 0x00, 0x06, cmd, 0x01, subkey, 0x00, 0x01, value]
}

/// `SendData.getNoValueProtocol` — fixed 8-byte frames (no variable payload).
fn get_no_value_protocol(cmd: u8, subkey: u8) -> [u8; 8] {
    let mut b = [0u8; 8];
    b[0] = 0xCD;
    let lm = 5u32; // length - 3 for 8-byte packet
    let lb = lm.to_be_bytes();
    b[1] = lb[2];
    b[2] = lb[3];
    b[3] = cmd;
    b[4] = 1;
    b[5] = subkey;
    b
}

async fn cmd_query(
    adapter: &Adapter,
    addr_str: &str,
    write_uuid_str: &str,
    notify_uuid_str: &str,
    info_keys: &[u8],
    dial_keys: &[u8],
    response_timeout_ms: u64,
    gap_ms: u64,
    disconnect_after: bool,
) -> anyhow::Result<()> {
    let addr: Address = addr_str
        .parse()
        .with_context(|| format!("invalid BLE address: {addr_str}"))?;
    let wu = Uuid::parse_str(write_uuid_str).context("write uuid")?;
    let nu = Uuid::parse_str(notify_uuid_str).context("notify uuid")?;

    let device = adapter.device(addr).context("adapter.device")?;
    println!("Device {} (adapter {})", addr, adapter.name());

    println!("Calling BlueZ Connect() (same API as Settings / `bluetoothctl connect`)…");
    device_connect(&device, BLE_CONNECT_TIMEOUT, None).await?;

    let write_ch = find_characteristic(&device, wu)
        .await
        .with_context(|| format!("write characteristic not found: {write_uuid_str}"))?;
    let notify_ch = find_characteristic(&device, nu)
        .await
        .with_context(|| format!("notify characteristic not found: {notify_uuid_str}"))?;

    println!("Subscribing to notifications…");
    let notify_stream = tokio::time::timeout(NOTIFY_ENABLE_TIMEOUT, notify_ch.notify())
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "notify subscribe timed out after {:?}",
                NOTIFY_ENABLE_TIMEOUT
            )
        })?
        .context("notify()")?;
    pin_mut!(notify_stream);
    // Also subscribe to 7e400004 (secondary NUS notify) — iPhone app enables this CCCD too
    let extra_notify_uuid = Uuid::parse_str("7e400004-b5a3-f393-e0a9-e50e24dcca9d").unwrap();
    if let Ok(Some(extra_ch)) = find_characteristic(&device, extra_notify_uuid).await.map(Some).or_else(|_| Ok::<_, anyhow::Error>(None)) {
        let _ = tokio::time::timeout(NOTIFY_ENABLE_TIMEOUT, extra_ch.notify()).await;
    }

    let wait = Duration::from_millis(response_timeout_ms);
    let gap = Duration::from_millis(gap_ms);

    for &key in info_keys {
        let frame = get_no_value_protocol(CMD_GET_INFO_BY_KEY, key);
        println!("\ncmd26 key {key}: write {}", hex(&frame));
        write_ch.write(&frame).await.context("write")?;
        match tokio::time::timeout(wait, notify_stream.next()).await {
            Ok(Some(data)) => {
                println!("  notify: {}", hex(&data));
                if let Some(line) = decode_dc_notify_line(&data) {
                    println!("{line}");
                }
            }
            Ok(None) => println!("  (notify stream ended)"),
            Err(_) => println!("  (timeout, no notify)"),
        }
        tokio::time::sleep(gap).await;
    }

    for &key in dial_keys {
        let frame = get_no_value_protocol(CMD_DIAL_READ, key);
        println!("\ncmd32 key {key}: write {}", hex(&frame));
        write_ch.write(&frame).await.context("write")?;
        match tokio::time::timeout(wait, notify_stream.next()).await {
            Ok(Some(data)) => {
                println!("  notify: {}", hex(&data));
                if let Some(line) = decode_dc_notify_line(&data) {
                    println!("{line}");
                }
            }
            Ok(None) => println!("  (notify stream ended)"),
            Err(_) => println!("  (timeout, no notify)"),
        }
        tokio::time::sleep(gap).await;
    }

    if disconnect_after {
        device_disconnect_best_effort(&device).await;
        println!("\nDisconnected.");
    }
    Ok(())
}

/// **`getDialClockInfo`** — `SendData.getNoValueProtocol(32, 2)`; reply is **`0xCD`** (often split across notifies).
async fn cmd_dial_dims(
    adapter: &Adapter,
    addr_str: &str,
    write_uuid_str: &str,
    notify_uuid_str: &str,
    response_timeout_ms: u64,
    notify_settle_ms: u64,
    disconnect_after: bool,
) -> anyhow::Result<()> {
    let addr: Address = addr_str
        .parse()
        .with_context(|| format!("invalid BLE address: {addr_str}"))?;
    let wu = Uuid::parse_str(write_uuid_str).context("write uuid")?;
    let nu = Uuid::parse_str(notify_uuid_str).context("notify uuid")?;

    let device = adapter.device(addr).context("adapter.device")?;
    println!("Device {} (adapter {})", addr, adapter.name());

    println!("Calling BlueZ Connect() (same API as Settings / `bluetoothctl connect`)…");
    device_connect(&device, BLE_CONNECT_TIMEOUT, None).await?;

    let write_ch = find_characteristic(&device, wu)
        .await
        .with_context(|| format!("write characteristic not found: {write_uuid_str}"))?;
    let notify_ch = find_characteristic(&device, nu)
        .await
        .with_context(|| format!("notify characteristic not found: {notify_uuid_str}"))?;

    println!("Subscribing to notifications…");
    let notify_stream = tokio::time::timeout(NOTIFY_ENABLE_TIMEOUT, notify_ch.notify())
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "notify subscribe timed out after {:?}",
                NOTIFY_ENABLE_TIMEOUT
            )
        })?
        .context("notify()")?;
    pin_mut!(notify_stream);

    println!("Notify settle {} ms…", notify_settle_ms);
    tokio::time::sleep(Duration::from_millis(notify_settle_ms)).await;

    let frame = get_no_value_protocol(CMD_DIAL_READ, SUB_DIAL_NOTIFY_CLOCK_INFO);
    println!(
        "\ngetDialClockInfo — cmd32/2 (APK SendData.getDialClockInfo): write {}",
        hex(&frame)
    );
    write_ch.write(&frame).await.context("write cmd32/2")?;

    let mut asm = CdNotifyAssembler::default();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(response_timeout_ms.max(500));

    while tokio::time::Instant::now() < deadline {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(left.min(Duration::from_millis(500)), notify_stream.next()).await {
            Ok(Some(data)) => {
                println!("  notify ({} bytes): {}", data.len(), hex(&data));
                if let Some(line) = decode_dc_notify_line(&data) {
                    println!("{line}");
                }
                for pkt in asm.push(&data) {
                    println!(
                        "  assembled 0xCD ({} bytes): {}",
                        pkt.len(),
                        hex(&pkt[..pkt.len().min(96)])
                    );
                    if pkt.len() > 96 {
                        println!("    … (truncated in log; full length {})", pkt.len());
                    }
                    if let Some((screen, grade, w, h)) = parse_dial_clock_info_cd(&pkt) {
                        println!("\n--- Parsed dial clock info (BaseReceiveData.parseDialInfo) ---");
                        println!("  screenType: {screen}");
                        println!("  grade:      {grade}");
                        println!("  width:      {w}");
                        println!("  height:     {h}");
                        if let Some(full) = parse_dial_clock_info_full(&pkt) {
                            if let Some(cfg) = full.config {
                                println!("  config:     0x{cfg:02x} (APK chunk hint: {} bytes)", apk_dial_chunk_size_from_config_byte(cfg));
                            } else {
                                println!("  config:     (not present in payload)");
                            }
                        }
                        println!(
                            "  RGB565 payload size (width×height×2): {} bytes",
                            (w as u32) * (h as u32) * 2
                        );
                        if disconnect_after {
                            device_disconnect_best_effort(&device).await;
                            println!("\nDisconnected.");
                        }
                        return Ok(());
                    }
                }
            }
            Ok(None) => {
                bail!("notify stream ended before cmd32/2 dial info reply");
            }
            Err(_) => {}
        }
    }

    bail!(
        "timeout after {} ms: no complete cmd32/2 dial clock info (0xCD with width/height). \
         Try increasing --response-timeout-ms; device may send a long multi-notify frame.",
        response_timeout_ms
    );
}

/// Standalone **`getNoValueProtocol(cmd, sub)`** write + print notifies until **`response_timeout_ms`** (APK **`readStatus()`**-style debugging).
async fn cmd_uart_read_status(
    adapter: &Adapter,
    addr_str: &str,
    write_uuid_str: &str,
    notify_uuid_str: &str,
    cmd: u8,
    sub: u8,
    title: &str,
    response_timeout_ms: u64,
    notify_settle_ms: u64,
    disconnect_after: bool,
) -> anyhow::Result<()> {
    let addr: Address = addr_str
        .parse()
        .with_context(|| format!("invalid BLE address: {addr_str}"))?;
    let wu = Uuid::parse_str(write_uuid_str).context("write uuid")?;
    let nu = Uuid::parse_str(notify_uuid_str).context("notify uuid")?;

    let device = adapter.device(addr).context("adapter.device")?;
    println!("Device {} (adapter {})", addr, adapter.name());

    println!("Calling BlueZ Connect()…");
    device_connect(&device, BLE_CONNECT_TIMEOUT, None).await?;

    let write_ch = find_characteristic(&device, wu)
        .await
        .with_context(|| format!("write characteristic not found: {write_uuid_str}"))?;
    let notify_ch = find_characteristic(&device, nu)
        .await
        .with_context(|| format!("notify characteristic not found: {notify_uuid_str}"))?;

    println!("Subscribing to notifications…");
    let notify_stream = tokio::time::timeout(NOTIFY_ENABLE_TIMEOUT, notify_ch.notify())
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "notify subscribe timed out after {:?}",
                NOTIFY_ENABLE_TIMEOUT
            )
        })?
        .context("notify()")?;
    pin_mut!(notify_stream);

    println!("Notify settle {} ms…", notify_settle_ms);
    tokio::time::sleep(Duration::from_millis(notify_settle_ms)).await;

    let frame = get_no_value_protocol(cmd, sub);
    println!("\n{title}");
    println!("write {} ({} bytes)", hex(&frame), frame.len());
    write_ch.write(&frame).await.context("write getNoValueProtocol")?;

    let mut asm = CdNotifyAssembler::default();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(response_timeout_ms.max(200));

    while tokio::time::Instant::now() < deadline {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(left.min(Duration::from_millis(500)), notify_stream.next()).await {
            Ok(Some(data)) => {
                println!("  notify ({} bytes): {}", data.len(), hex(&data));
                if let Some(line) = decode_dc_notify_line(&data) {
                    println!("{line}");
                }
                for pkt in asm.push(&data) {
                    println!(
                        "  assembled 0xCD ({} bytes): {}",
                        pkt.len(),
                        hex(&pkt[..pkt.len().min(128)])
                    );
                }
            }
            Ok(None) => {
                println!("  (notify stream ended)");
                break;
            }
            Err(_) => break,
        }
    }

    println!("Listen window done ({} ms).", response_timeout_ms);
    if disconnect_after {
        device_disconnect_best_effort(&device).await;
        println!("Disconnected.");
    }
    Ok(())
}

fn parse_ack_packet(packet: &[u8], loose_ack: bool) -> Option<i32> {
    if loose_ack {
        parse_cd_notify_status(packet)
    } else {
        parse_dial_watch_ack_status(packet)
    }
}

/// APK **CommandPool** sends long frames as **≤20**-byte GATT writes with **~100** ms between segments.
async fn gatt_write_fragmented(
    write_ch: &Characteristic,
    frame: &[u8],
    fragment: usize,
    gap_ms: u64,
) -> anyhow::Result<()> {
    if fragment == 0 || frame.len() <= fragment {
        write_ch.write(frame).await.context("gatt write")?;
        return Ok(());
    }
    let n = (frame.len() + fragment - 1) / fragment;
    for (i, chunk) in frame.chunks(fragment).enumerate() {
        if i > 0 && gap_ms > 0 {
            tokio::time::sleep(Duration::from_millis(gap_ms)).await;
        }
        write_ch
            .write(chunk)
            .await
            .with_context(|| format!("gatt fragment {}/{} ({} bytes)", i + 1, n, chunk.len()))?;
    }
    Ok(())
}

/// Wait until a complete `0xCD` packet parses to status `want` (`parseDialUpCode` int).
async fn wait_notify_status<S>(
    stream: &mut S,
    asm: &mut CdNotifyAssembler,
    want: i32,
    timeout: Duration,
    loose_ack: bool,
    file34: bool,
    loose_start_banner: bool,
) -> anyhow::Result<()>
where
    S: futures::Stream<Item = Vec<u8>> + Unpin,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            bail!("timeout waiting for dial notify status {want}");
        }
        let data = tokio::time::timeout(remaining, stream.next())
            .await
            .map_err(|_| anyhow::anyhow!("timeout waiting for notify"))?
            .ok_or_else(|| anyhow::anyhow!("notify stream ended"))?;
        println!(
            "  raw notify ({} bytes): {}",
            data.len(),
            hex(&data[..data.len().min(64)])
        );
        if file34 {
            if let Some((cmd, sub)) = parse_dc_short(&data) {
                println!("  DC short cmd={cmd} sub={sub}: {}", hex(&data));
                if cmd == CMD_FILE_UART && sub == 2 && want == 1000 {
                    return Ok(());
                }
                if cmd == CMD_FILE_UART && sub == 3 && want == 2 {
                    return Ok(());
                }
            }
        } else if let Some((cmd, sub)) = parse_dc_short(&data) {
            // Same short `0xDC` path as file34 — firmware often ACKs dial start/finish this way.
            println!("  DC short cmd={cmd} sub={sub}: {}", hex(&data));
            if cmd == CMD_DIAL_TRANSFER && sub == SUB_DIAL_START && want == 1000 {
                return Ok(());
            }
            if cmd == CMD_DIAL_TRANSFER && sub == SUB_DIAL_FINISH && want == 2 {
                return Ok(());
            }
	    if cmd == 0 && (want == 1000 || sub as i32 == want -1000) {
		return Ok(());
	    }
        }
        for pkt in asm.push(&data) {
            if want == 1000 && loose_start_banner && is_dial_start_banner_cd(&pkt) {
                println!(
                    "  parsed dial start banner (0xCD cmd 0x15 sub 0x0c): {}",
                    hex(&pkt)
                );
                return Ok(());
            }
            if let Some(code) = parse_ack_packet(&pkt, loose_ack) {
                println!(
                    "  parsed ACK status: {code} {} ({})",
                    if loose_ack { "(loose)" } else { "" },
                    hex(&pkt)
                );
                if code == want {
                    return Ok(());
                }
                if code < 1000 && is_watch_theme_fatal_protocol_status(code) {
                    let detail = watch_theme_protocol_error_message(code)
                        .unwrap_or("fatal device status (see APK WatchThemeTools / BleFileSendTools)");
                    bail!("device returned protocol status {code}: {detail}");
                }
                println!("  (expected {want}, still waiting…)");
            } else {
                println!("  notify (ignored): {}", hex(&pkt));
            }
        }
    }
}

async fn drain_notifies<S>(stream: &mut S, asm: &mut CdNotifyAssembler)
where
    S: futures::Stream<Item = Vec<u8>> + Unpin,
{
    loop {
        match tokio::time::timeout(Duration::from_millis(45), stream.next()).await {
            Ok(Some(data)) => {
                for pkt in asm.push(&data) {
                    println!("  preflight drain: {}", hex(&pkt[..pkt.len().min(48)]));
                }
            }
            _ => break,
        }
    }
}

async fn write_no_value_poll_and_drain<S>(
    write_ch: &Characteristic,
    stream: &mut S,
    asm: &mut CdNotifyAssembler,
    cmd: u8,
    sub: u8,
    label: &str,
) -> anyhow::Result<()>
where
    S: futures::Stream<Item = Vec<u8>> + Unpin,
{
    let frame = get_no_value_protocol(cmd, sub);
    println!("{label}: write {}", hex(&frame));
    write_ch
        .write(&frame)
        .await
        .with_context(|| label.to_string())?;
    tokio::time::sleep(Duration::from_millis(80)).await;
    drain_notifies(stream, asm).await;
    Ok(())
}

async fn run_preflight_uart<S>(
    write_ch: &Characteristic,
    stream: &mut S,
    asm: &mut CdNotifyAssembler,
) -> anyhow::Result<()>
where
    S: futures::Stream<Item = Vec<u8>> + Unpin,
{
    println!("Preflight: time sync…");
    write_ch
        .write(&build_set_times_value())
        .await
        .context("preflight sync-time")?;
    tokio::time::sleep(Duration::from_millis(90)).await;
    drain_notifies(stream, asm).await;

    for key in [2u8, 1u8] {
        println!("Preflight: cmd32 key {key} (dial read)…");
        write_ch
            .write(&get_no_value_protocol(CMD_DIAL_READ, key))
            .await
            .with_context(|| format!("preflight dial read {key}"))?;
        tokio::time::sleep(Duration::from_millis(90)).await;
        drain_notifies(stream, asm).await;
    }
    Ok(())
}

/// Order and payloads from `logs/upload-2.log.pcapng` (first central writes on NUS TX before image chunks).
async fn run_preflight_uart_upload2<S>(
    write_ch: &Characteristic,
    stream: &mut S,
    asm: &mut CdNotifyAssembler,
) -> anyhow::Result<()>
where
    S: futures::Stream<Item = Vec<u8>> + Unpin,
{
    const GAP_MS: u64 = 90;
    const DIAL3_REPEAT: usize = 15;
    const DIAL3_GAP_MS: u64 = 45;

    println!("Preflight (upload-2 capture): time sync…");
    write_ch
        .write(&build_set_times_value())
        .await
        .context("preflight sync-time")?;
    tokio::time::sleep(Duration::from_millis(GAP_MS)).await;
    drain_notifies(stream, asm).await;

    println!("Preflight: getSetLanguage(1) — SwitchProtocol(18,21,1)…");
    write_ch
        .write(&switch_protocol(CMD_SETTINGS, KEY_SETTING_LANGUAGE, 1))
        .await
        .context("preflight language")?;
    tokio::time::sleep(Duration::from_millis(GAP_MS)).await;
    drain_notifies(stream, asm).await;

    println!("Preflight: getTurnOnRealTimeStep(true) — SwitchProtocol(21,6,1)…");
    write_ch
        .write(&switch_protocol(CMD_SPORT, KEY_SPORT_REALTIME_STEP, 1))
        .await
        .context("preflight realtime step")?;
    tokio::time::sleep(Duration::from_millis(GAP_MS)).await;
    drain_notifies(stream, asm).await;

    println!("Preflight: getDialClockInfo — cmd32/2…");
    write_ch
        .write(&get_no_value_protocol(CMD_DIAL_READ, 2))
        .await
        .context("preflight dial clock info")?;
    tokio::time::sleep(Duration::from_millis(GAP_MS)).await;
    drain_notifies(stream, asm).await;

    println!("Preflight: DC short #1…");
    write_ch
        .write(&PREFLIGHT_UPLOAD2_DC1)
        .await
        .context("preflight DC #1")?;
    tokio::time::sleep(Duration::from_millis(GAP_MS)).await;
    drain_notifies(stream, asm).await;

    println!("Preflight: DC short #2…");
    write_ch
        .write(&PREFLIGHT_UPLOAD2_DC2)
        .await
        .context("preflight DC #2")?;
    tokio::time::sleep(Duration::from_millis(GAP_MS)).await;
    drain_notifies(stream, asm).await;

    println!("Preflight: getSetInfoByKey(10) — SwitchProtocol(18,10,1)…");
    write_ch
        .write(&switch_protocol(CMD_SETTINGS, KEY_SETTING_CLASSIC_BT_ADDR, 1))
        .await
        .context("preflight classic bt key")?;
    tokio::time::sleep(Duration::from_millis(GAP_MS)).await;
    drain_notifies(stream, asm).await;

    for i in 1..=DIAL3_REPEAT {
        println!("Preflight: cmd32/3 dial device get ({i}/{DIAL3_REPEAT})…");
        write_ch
            .write(&get_no_value_protocol(CMD_DIAL_READ, 3))
            .await
            .with_context(|| format!("preflight dial read 3 ({i})"))?;
        tokio::time::sleep(Duration::from_millis(DIAL3_GAP_MS)).await;
        drain_notifies(stream, asm).await;
    }

    // Successful **iPhone** capture does **not** send `getNoValueProtocol(32,1)` here; it sends DC → weather
    // (`SendData.getWeatherInfoValue`, cmd **18**/**32**) then dial start. Omitting cmd32/1 matches `upload-2`.
    println!("Preflight: DC short #3 (upload-2 frame 3434, before weather burst)…");
    write_ch
        .write(&PREFLIGHT_UPLOAD2_DC_BEFORE_WEATHER)
        .await
        .context("preflight DC #3 before weather")?;
    tokio::time::sleep(Duration::from_millis(GAP_MS)).await;
    drain_notifies(stream, asm).await;

    println!("Preflight: weather cmd18/32 — fragment 1/3 (upload-2 frame 3454, start of getWeatherInfoValue)…");
    write_ch
        .write(&PREFLIGHT_UPLOAD2_WEATHER_FRAG1)
        .await
        .context("preflight weather frag 1")?;
    tokio::time::sleep(Duration::from_millis(GAP_MS)).await;
    drain_notifies(stream, asm).await;

    println!("Preflight: weather — fragment 2/3 (upload-2 frame 3458)…");
    write_ch
        .write(&PREFLIGHT_UPLOAD2_WEATHER_FRAG2)
        .await
        .context("preflight weather frag 2")?;
    tokio::time::sleep(Duration::from_millis(GAP_MS)).await;
    drain_notifies(stream, asm).await;

    println!("Preflight: weather — fragment 3/3 (upload-2 frame 3464)…");
    write_ch
        .write(&PREFLIGHT_UPLOAD2_WEATHER_FRAG3)
        .await
        .context("preflight weather frag 3")?;
    tokio::time::sleep(Duration::from_millis(GAP_MS)).await;
    drain_notifies(stream, asm).await;

    Ok(())
}

/// Section 1 (APK parity): `getSetInfoByKey` sweep + optional `getEnterOtaMode` before main preflight.
async fn run_section1_preflight<S>(
    write_ch: &Characteristic,
    stream: &mut S,
    asm: &mut CdNotifyAssembler,
    cmd26_keys: &[u8],
    enter_ota: bool,
) -> anyhow::Result<()>
where
    S: futures::Stream<Item = Vec<u8>> + Unpin,
{
    const GAP_MS: u64 = 90;
    for &key in cmd26_keys {
        println!("Section-1: getSetInfoByKey({key}) — cmd26…");
        write_ch
            .write(&get_no_value_protocol(CMD_GET_INFO_BY_KEY, key))
            .await
            .with_context(|| format!("cmd26 key {key}"))?;
        tokio::time::sleep(Duration::from_millis(GAP_MS)).await;
        drain_notifies(stream, asm).await;
    }
    if enter_ota {
        println!("Section-1: getEnterOtaMode — getNoValueProtocol(18,25)…");
        write_ch
            .write(&get_no_value_protocol(CMD_SETTINGS, KEY_ENTER_OTA_MODE))
            .await
            .context("enter OTA mode")?;
        tokio::time::sleep(Duration::from_millis(GAP_MS)).await;
        drain_notifies(stream, asm).await;
    }
    Ok(())
}

/// APK `SendData.getDialClockInfo` → `BaseReceiveData.parseDialInfo` (incl. optional **`config`** for chunk size).
async fn fetch_dial_clock_info<S>(
    write_ch: &Characteristic,
    stream: &mut S,
    asm: &mut CdNotifyAssembler,
) -> anyhow::Result<DialClockInfoParsed>
where
    S: futures::Stream<Item = Vec<u8>> + Unpin,
{
    println!("Reading dial dimensions (cmd32/2 getDialClockInfo — same as APK ClockDialInfoBody width/height)…");
    write_ch
        .write(&get_no_value_protocol(CMD_DIAL_READ, SUB_DIAL_NOTIFY_CLOCK_INFO))
        .await
        .context("write cmd32/2 getDialClockInfo")?;
    tokio::time::sleep(Duration::from_millis(120)).await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(6);
    while tokio::time::Instant::now() < deadline {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(left.min(Duration::from_millis(400)), stream.next()).await {
            Ok(Some(data)) => {
                for pkt in asm.push(&data) {
                    if let Some(info) = parse_dial_clock_info_full(&pkt) {
                        let screen = info.screen_type;
                        let grade = info.grade;
                        let w = info.width;
                        let h = info.height;
                        println!(
                            "  cmd32/2 dial info: screenType={screen} grade={grade} width={w} height={h}"
                        );
                        if let Some(c) = info.config {
                            println!(
                                "  config=0x{c:02x} — APK dial chunk hint: {} bytes",
                                apk_dial_chunk_size_from_config_byte(c)
                            );
                        }
                        if w == 0 || h == 0 {
                            bail!("device reported zero width or height in dial info");
                        }
                        return Ok(info);
                    }
                }
            }
            Ok(None) => break,
            Err(_) => {}
        }
    }
    bail!(
        "timeout waiting for cmd32/2 dial clock info (0xCD frame with width/height). \
         Check notify subscription, or try without --use-device-dial-dims and pass --width/--height"
    );
}

async fn cmd_upload_dial(
    adapter: &Adapter,
    addr_str: &str,
    write_uuid_str: &str,
    notify_uuid_str: &str,
    file: Option<&std::path::Path>,
    strip_bmp: bool,
    solid: bool,
    mut width: u16,
    mut height: u16,
    use_device_dial_dims: bool,
    solid_rgb565: u16,
    mut chunk: usize,
    font_pos: u8,
    custom: bool,
    rgb_r: u8,
    rgb_g: u8,
    rgb_b: u8,
    replace_pic_pos: Option<u8>,
    extended_dial_start: bool,
    dial_start_mid4: String,
    dial_start_mid_from_dims: bool,
    step_timeout_ms: u64,
    disconnect_after: bool,
    reconnect: bool,
    protocol: &str,
    notify_settle_ms: u64,
    preflight: bool,
    preflight_upload2: bool,
    skip_start_ack: bool,
    skip_finish_ack: bool,
    loose_ack: bool,
    blind_chunks: bool,
    chunk_gap_ms: u64,
    dial_start_probe: bool,
    preflight_cmd26_keys: &[u8],
    enter_ota_mode: bool,
    dial32_sub3_control: Option<u8>,
    min_battery_percent: u8,
    chunk_write_retries: u32,
    dial_read_status_after_start: bool,
    file_read_status_after_start: bool,
    gatt_fragment: usize,
    gatt_fragment_gap_ms: u64,
    apk_loose_start: bool,
    apply_chunk_from_dial_config: bool,
) -> anyhow::Result<()> {
    if gatt_fragment > 512 {
        bail!("--gatt-fragment-bytes must be <= 512");
    }
    let use_file34 = match protocol {
        "dial31" => false,
        "file34" => true,
        _ => bail!("--protocol must be dial31 or file34"),
    };
    if dial_start_probe && use_file34 {
        bail!("--dial-start-probe is only for protocol dial31");
    }
    if use_device_dial_dims && dial_start_probe {
        bail!("--use-device-dial-dims is not compatible with --dial-start-probe");
    }
    if extended_dial_start {
        if use_file34 {
            bail!("--extended-dial-start is only for protocol dial31");
        }
        if replace_pic_pos.is_some() {
            bail!("--extended-dial-start is incompatible with --replace-pic-pos");
        }
    }
    let effective_preflight = preflight || preflight_upload2;
    if preflight_upload2 && !preflight {
        println!("Note: --preflight-upload2 implies preflight.");
    }

    let addr: Address = addr_str
        .parse()
        .with_context(|| format!("invalid BLE address: {addr_str}"))?;
    let wu = Uuid::parse_str(write_uuid_str).context("write uuid")?;
    let nu = Uuid::parse_str(notify_uuid_str).context("notify uuid")?;

    let device = adapter.device(addr).context("adapter.device")?;
    println!("Device {} (adapter {})", addr, adapter.name());

    if reconnect {
        println!("Reconnecting (disconnect + connect)…");
        device_disconnect_best_effort(&device).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        device_connect(&device, BLE_CONNECT_TIMEOUT, None).await?;
    } else {
        println!("Calling BlueZ Connect() (same API as Settings / `bluetoothctl connect`)…");
        device_connect(&device, BLE_CONNECT_TIMEOUT, None).await?;
    }

    wait_gatt_ready_for_upload(&device).await;

    let write_ch = find_characteristic(&device, wu)
        .await
        .with_context(|| {
            format!(
                "write characteristic not found: {write_uuid_str} — real DG01 NUS TX is 7e400002 (default); --apk-uart uses 6e400002 (FitPro APK only)"
            )
        })?;
    let notify_ch = find_characteristic(&device, nu)
        .await
        .with_context(|| {
            format!(
                "notify characteristic not found: {notify_uuid_str} — DG01 default is 7e400003; --apk-uart uses 6e400003"
            )
        })?;

    println!("Subscribing to notifications…");
    let notify_stream = tokio::time::timeout(NOTIFY_ENABLE_TIMEOUT, notify_ch.notify())
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "notify subscribe timed out after {:?}",
                NOTIFY_ENABLE_TIMEOUT
            )
        })?
        .context("notify()")?;
    pin_mut!(notify_stream);

    let mut asm = CdNotifyAssembler::default();
    let step = Duration::from_millis(step_timeout_ms);

    if min_battery_percent > 0 {
        match device_info_gatt::read_battery_level_percent(&device).await? {
            Some(p) if p < min_battery_percent => {
                bail!(
                    "battery {p}% is below --min-battery-percent {min_battery_percent}% (abort before upload)"
                );
            }
            Some(p) => println!(
                "Battery (BAS): {p}% — gate OK (≥ {min_battery_percent}%)"
            ),
            None => println!(
                "Warning: could not read BAS battery level — continuing (--min-battery-percent {})",
                min_battery_percent
            ),
        }
    }

    let mut dial_info: Option<DialClockInfoParsed> = None;
    if use_device_dial_dims {
        let info = fetch_dial_clock_info(&write_ch, &mut notify_stream, &mut asm).await?;
        width = info.width;
        height = info.height;
        dial_info = Some(info);
    }
    if apply_chunk_from_dial_config {
        if let Some(ref info) = dial_info {
            if let Some(c) = info.config {
                chunk = apk_dial_chunk_size_from_config_byte(c);
                println!(
                    "dial chunk (APK WatchThemeTools): config 0x{c:02x} → {chunk} bytes (120 if bit1 of config else 200)"
                );
            } else {
                println!("dial chunk: no config byte in dial info; keeping --chunk {chunk}");
            }
        } else {
            println!("dial chunk: no cmd32/2 dial info; keeping --chunk {chunk} (use --use-device-dial-dims for auto)");
        }
    }
    if !(1..=512).contains(&chunk) {
        bail!("--chunk must be 1..=512");
    }

    let file_data: Vec<u8> = if dial_start_probe {
        vec![]
    } else if solid {
        if file.is_some() {
            bail!("do not pass --file with --solid");
        }
        solid_rgb565_buffer(width, height, solid_rgb565)
    } else {
        let p = file.ok_or_else(|| anyhow::anyhow!("pass --file PATH or use --solid"))?;
        let b = std::fs::read(p).with_context(|| format!("read {}", p.display()))?;
        if strip_bmp {
            strip_bmp_rgb565_tail(&b, u32::from(width), u32::from(height))?
        } else {
            b
        }
    };

    println!(
        "protocol={} size={}×{} payload={} bytes chunk={} gatt_frag={} gatt_gap_ms={} min_bat={} chunk_retries={} dial32/1_after_start={} cmd35/1_after_start={} apk_loose_start={} replace_pic_pos={:?} extended_start={} mid_from_dims={} preflight={} preflight_upload2={} cmd26_keys={:?} enter_ota={} dial32_sub3={:?} skip_start_ack={} skip_finish_ack={} loose_ack={} blind_chunks={} chunk_gap_ms={} — first 16: {}",
        protocol,
        width,
        height,
        file_data.len(),
        chunk,
        gatt_fragment,
        gatt_fragment_gap_ms,
        min_battery_percent,
        chunk_write_retries,
        dial_read_status_after_start,
        file_read_status_after_start,
        apk_loose_start,
        replace_pic_pos,
        extended_dial_start,
        dial_start_mid_from_dims,
        effective_preflight,
        preflight_upload2,
        preflight_cmd26_keys,
        enter_ota_mode,
        dial32_sub3_control,
        skip_start_ack,
        skip_finish_ack,
        loose_ack,
        blind_chunks,
        chunk_gap_ms,
        hex(&file_data[..file_data.len().min(16)])
    );

    if !preflight_cmd26_keys.is_empty() || enter_ota_mode {
        run_section1_preflight(
            &write_ch,
            &mut notify_stream,
            &mut asm,
            preflight_cmd26_keys,
            enter_ota_mode,
        )
        .await?;
    }

    if effective_preflight {
        if preflight_upload2 {
            run_preflight_uart_upload2(&write_ch, &mut notify_stream, &mut asm).await?;
        } else {
            run_preflight_uart(&write_ch, &mut notify_stream, &mut asm).await?;
        }
    }

    if let Some(ctrl) = dial32_sub3_control {
        println!("Preflight tail: getDialDeviceContrlReponse({ctrl}) — cmd32/3 + 1 byte…");
        let p = dial_device_control_response(ctrl);
        println!("  {}", hex(&p));
        gatt_write_fragmented(&write_ch, &p, gatt_fragment, gatt_fragment_gap_ms)
            .await
            .context("dial device control response (cmd32/3)")?;
        tokio::time::sleep(Duration::from_millis(90)).await;
        drain_notifies(&mut notify_stream, &mut asm).await;
    }

    println!("Notify settle {} ms…", notify_settle_ms);
    tokio::time::sleep(Duration::from_millis(notify_settle_ms)).await;

    let custom_b = u8::from(custom);

    if use_file34 {
        let start = file34_start(&file_data);
        println!("Start cmd34/2 (file len+sum): {}", hex(&start));
        gatt_write_fragmented(&write_ch, &start, gatt_fragment, gatt_fragment_gap_ms)
            .await
            .context("write start (file34)")?;
    } else {
        let file_len_u32 = file_data.len() as u32;
        let start = if extended_dial_start {
            let mid4 = if dial_start_mid_from_dims {
                dial_start_mid4_dims_be(width, height)
            } else {
                parse_mid4_hex(&dial_start_mid4)?
            };
            println!(
                "Start cmd31/2 **extended** (17 B pl): mid4={} file_len={}",
                hex(&mid4),
                file_len_u32
            );
            dial_start_extended(
                font_pos,
                custom_b,
                mid4,
                rgb_r,
                rgb_g,
                rgb_b,
                file_len_u32,
            )
        } else {
            println!("Start cmd31/2 (minimal):");
            dial_start(font_pos, custom_b, rgb_r, rgb_g, rgb_b, replace_pic_pos)
        };
        println!("  {}", hex(&start));
        gatt_write_fragmented(&write_ch, &start, gatt_fragment, gatt_fragment_gap_ms)
            .await
            .context("write start (dial31)")?;
    }

    if dial_start_probe {
        println!(
            "Dial start sent — watch the badge ~10s for an “uploading” / transfer UI. Raw notifies:"
        );
        tokio::time::sleep(Duration::from_millis(150)).await;
        let probe_end = tokio::time::Instant::now() + Duration::from_secs(10);
        let mut notify_count: u32 = 0;
        while tokio::time::Instant::now() < probe_end {
            let left = probe_end.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                break;
            }
            match tokio::time::timeout(left.min(Duration::from_millis(500)), notify_stream.next())
                .await
            {
                Ok(Some(data)) => {
                    notify_count += 1;
                    println!("  notify ({}): {}", data.len(), hex(&data));
                    if let Some((c, s)) = parse_dc_short(&data) {
                        println!("    DC parse: cmd={c} sub={s}");
                    }
                    for pkt in asm.push(&data) {
                        println!("    CD assembled ({}): {}", pkt.len(), hex(&pkt));
                        if is_dial_start_banner_cd(&pkt) {
                            println!(
                                "    dial start banner (0xCD cmd 0x15 sub 0x0c) — seen after some preflights"
                            );
                        }
                        if let Some(st) = parse_dial_watch_ack_status(&pkt) {
                            println!("    dial ACK status int: {st}");
                        } else if let Some(st) = parse_cd_notify_status(&pkt) {
                            println!("    CD payload int (loose): {st}");
                        }
                    }
                }
                Ok(None) => break,
                Err(_) => {}
            }
        }
        if notify_count == 0 {
            println!(
                "  (no notifications in 10s — common on some FW: no splash until the first file chunk, \
                 or start is accepted with no notify. Next step: short solid upload with --preflight-upload2.)"
            );
        }
        drain_notifies(&mut notify_stream, &mut asm).await;
        println!("--dial-start-probe done.");
        if disconnect_after {
            device_disconnect_best_effort(&device).await;
            println!("Disconnected.");
        }
        return Ok(());
    }

    if skip_start_ack && !use_file34 {
        println!("Skipping dial start ACK wait (--skip-start-ack). Short settle…");
        tokio::time::sleep(Duration::from_millis(120)).await;
        drain_notifies(&mut notify_stream, &mut asm).await;
    } else {
        wait_notify_status(
            &mut notify_stream,
            &mut asm,
            1000,
            step,
            loose_ack,
            use_file34,
            apk_loose_start,
        )
        .await?;
    }

    if !use_file34 && dial_read_status_after_start {
        write_no_value_poll_and_drain(
            &write_ch,
            &mut notify_stream,
            &mut asm,
            CMD_DIAL_READ,
            1,
            "dial readStatus poll (cmd32/1)",
        )
        .await?;
    }
    if use_file34 && file_read_status_after_start {
        write_no_value_poll_and_drain(
            &write_ch,
            &mut notify_stream,
            &mut asm,
            CMD_FILE_SEND_STATUS,
            1,
            "file send status poll (cmd35/1)",
        )
        .await?;
    }

    let mut off = 0usize;
    let mut seq: u32 = 1;
    let max_chunk_writes = 1u32.saturating_add(chunk_write_retries);
    while off < file_data.len() {
        let end = (off + chunk).min(file_data.len());
        let piece = &file_data[off..end];
        let frame = if use_file34 {
            file34_file_frame(seq as u16, piece)
        } else {
            dial_file_frame(seq as u16, piece)
        };
        println!(
            "Chunk seq={seq} len={} frame_len={}",
            piece.len(),
            frame.len()
        );
        let mut write_attempt: u32 = 0;
        loop {
            write_attempt += 1;
            gatt_write_fragmented(&write_ch, &frame, gatt_fragment, gatt_fragment_gap_ms)
                .await
                .context("write chunk")?;
            if blind_chunks {
                tokio::time::sleep(Duration::from_millis(15)).await;
                if chunk_gap_ms > 0 {
                    tokio::time::sleep(Duration::from_millis(chunk_gap_ms)).await;
                }
                break;
            }
            match wait_notify_status(
                &mut notify_stream,
                &mut asm,
                1000 + seq as i32,
                step,
                loose_ack,
                use_file34,
                false,
            )
            .await
            {
                Ok(()) => break,
                Err(e) => {
                    if write_attempt >= max_chunk_writes {
                        return Err(e);
                    }
                    println!(
                        "  chunk seq={seq} notify timeout (attempt {write_attempt}/{max_chunk_writes}) — resend chunk: {e}"
                    );
                }
            }
        }
        off = end;
        seq += 1;
    }

    if use_file34 {
        let fin = file34_finish_frame();
        println!("Finish cmd34/3: {}", hex(&fin));
        gatt_write_fragmented(&write_ch, &fin, gatt_fragment, gatt_fragment_gap_ms)
            .await
            .context("write finish (file34)")?;
    } else {
        let fin = dial_finish_payload(&file_data);
        println!("Finish cmd31/3 ({} bytes): {}", fin.len(), hex(&fin));
        gatt_write_fragmented(&write_ch, &fin, gatt_fragment, gatt_fragment_gap_ms)
            .await
            .context("write finish (dial31)")?;
    }
    if skip_finish_ack {
        println!("Skipping finish ACK wait (--skip-finish-ack). Draining notifies…");
        tokio::time::sleep(Duration::from_millis(200)).await;
        drain_notifies(&mut notify_stream, &mut asm).await;
        println!("Done — finish frame sent (notify status 2 not verified). Check the badge.");
    } else {
        wait_notify_status(
            &mut notify_stream,
            &mut asm,
            2,
            step,
            loose_ack,
            use_file34,
            false,
        )
        .await?;
        println!("Done — device reported success (status 2).");
    }

    if disconnect_after {
        device_disconnect_best_effort(&device).await;
        println!("Disconnected.");
    }
    Ok(())
}

async fn cmd_probe_upload(
    adapter: &Adapter,
    addr: &str,
    apk_uart: bool,
    test_bytes: usize,
) -> anyhow::Result<()> {
    let path = std::env::temp_dir().join("dg01-probe.bin");
    std::fs::write(&path, vec![0x5Au8; test_bytes]).with_context(|| format!("write {}", path.display()))?;
    println!("Probe file {} ({} bytes)", path.display(), test_bytes);

    let (w, n) = if apk_uart {
        (APK_UART_TX.to_string(), APK_UART_NOTIFY.to_string())
    } else {
        (DEFAULT_WRITE_UUID.to_string(), DEFAULT_NOTIFY_UUID.to_string())
    };

    let attempts: [(&str, &str, usize, bool, bool, bool, bool); 6] = [
        ("dial31 chunk200 preflight+reconnect", "dial31", 200, true, true, true, false),
        ("dial31 chunk120 preflight+reconnect", "dial31", 120, true, true, true, false),
        ("file34 chunk120 preflight+reconnect", "file34", 120, true, true, false, false),
        ("file34 chunk200 preflight+reconnect", "file34", 200, true, true, false, false),
        ("dial31 chunk120 loose+preflight+reconnect", "dial31", 120, true, true, true, true),
        ("dial31 chunk120 no-preflight reconnect", "dial31", 120, true, false, true, false),
    ];

    for (label, protocol, ch, reconnect, preflight, skip_start, loose) in attempts {
        println!("\n=== Probe: {label} ===");
        let r = cmd_upload_dial(
            adapter,
            addr,
            &w,
            &n,
            Some(path.as_path()),
            false,
            false,
            64,
            64,
            false,
            0,
            ch,
            0,
            false,
            255,
            255,
            255,
            None,
            false,
            "15a20008".to_string(),
            false,
            6_000,
            false,
            reconnect,
            protocol,
            300,
            preflight,
            false,
            skip_start,
            false,
            loose,
            false,
            0,
            false,
            &[],
            false,
            None,
            0,
            1,
            false,
            false,
            0,
            100,
            false,
            false,
        )
        .await;
        match r {
            Ok(()) => println!(">>> SUCCESS: {label}"),
            Err(e) => println!(">>> fail: {e}"),
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }

    std::fs::remove_file(&path).ok();
    println!("\nProbe finished.");
    Ok(())
}

/// LE discovery until `target` shows up in BlueZ’s device list, or `max_secs` elapses.
///
/// GNOME Settings feels instant because the device is often **already** cached; when it is not, the UI still
/// only waits until the peripheral is seen — it does not sit through an arbitrary full window. We mirror that
/// by stopping discovery as soon as `adapter.device_addresses()` contains `target` (and on `DeviceAdded`).
async fn warm_scan_le_until_cached(adapter: &Adapter, target: Address, max_secs: u64) -> anyhow::Result<()> {
    if max_secs == 0 {
        return Ok(());
    }
    if adapter.device_addresses().await?.contains(&target) {
        println!("Device {target} already in BlueZ cache — skipping LE scan.");
        return Ok(());
    }

    let filter = DiscoveryFilter {
        transport: DiscoveryTransport::Le,
        ..Default::default()
    };
    adapter.set_discovery_filter(filter).await?;
    let events = adapter.discover_devices().await?;
    pin_mut!(events);

    println!(
        "LE scan until {target} is cached (max {max_secs}s; stops early when seen — same idea as Settings)…"
    );

    let deadline = Instant::now() + Duration::from_secs(max_secs);
    loop {
        if Instant::now() >= deadline {
            return Ok(());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        let poll = tokio::time::sleep(remaining.min(Duration::from_millis(80)));
        tokio::select! {
            _ = poll => {
                if adapter.device_addresses().await?.contains(&target) {
                    println!("  {target} visible to BlueZ — stopping scan early.");
                    return Ok(());
                }
            }
            evt = events.next() => {
                match evt {
                    Some(AdapterEvent::DeviceAdded(a)) if a == target => {
                        println!("  advertisement seen — stopping scan early.");
                        return Ok(());
                    }
                    Some(_) => {
                        if adapter.device_addresses().await?.contains(&target) {
                            println!("  {target} visible to BlueZ — stopping scan early.");
                            return Ok(());
                        }
                    }
                    None => return Ok(()),
                }
            }
        }
    }
}

/// Optional LE discovery before connect/disconnect when **`warm_scan_secs > 0`** and the address is not yet in
/// BlueZ’s cache. When **`warm_scan_secs == 0`**, does nothing — same as the Settings toggle: we call
/// `Device1.Connect` on the object path for the MAC (see [`Adapter::device`](bluer::Adapter::device)); no scan.
async fn warm_scan_le_if_unseen(
    adapter: &Adapter,
    addr: Address,
    warm_scan_secs: u64,
) -> anyhow::Result<()> {
    if warm_scan_secs == 0 {
        return Ok(());
    }
    let known = adapter
        .device_addresses()
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    if known.contains(&addr) {
        return Ok(());
    }
    println!(
        "Device {addr} not in adapter device list — LE discovery until cached (max {warm_scan_secs}s)…"
    );
    warm_scan_le_until_cached(adapter, addr, warm_scan_secs).await
}

/// Nordic UART Service notify UUID from TX UUID (`…400002-…` → `…400003-…`).
fn nus_notify_uuid_from_tx(write_uuid_str: &str) -> anyhow::Result<Uuid> {
    let s = write_uuid_str.trim().to_lowercase();
    if !s.contains("400002") {
        bail!(
            "NUS TX UUID must contain 400002 to derive notify (got {write_uuid_str}); pass a standard NUS TX UUID"
        );
    }
    let n = s.replace("400002", "400003");
    Uuid::parse_str(&n).with_context(|| format!("derived notify uuid from {write_uuid_str}"))
}

/// NUS **primary service** UUID from TX (`…400002-…` → `…400001-…`) for `Device::connect_profile`.
fn nus_service_uuid_from_tx(write_uuid_str: &str) -> anyhow::Result<Uuid> {
    let s = write_uuid_str.trim().to_lowercase();
    if !s.contains("400002") {
        bail!(
            "NUS TX UUID must contain 400002 to derive service UUID (got {write_uuid_str})"
        );
    }
    let n = s.replace("400002", "400001");
    Uuid::parse_str(&n).with_context(|| format!("derived NUS service uuid from {write_uuid_str}"))
}

async fn connect_ble_device(
    adapter: &Adapter,
    addr_str: &str,
    reconnect: bool,
    warm_scan_secs: u64,
    pair: bool,
    connect_timeout: Duration,
    nus_profile: Option<Uuid>,
) -> anyhow::Result<bluer::Device> {
    let addr: Address = addr_str
        .parse()
        .with_context(|| format!("invalid BLE address: {addr_str}"))?;

    warm_scan_le_if_unseen(adapter, addr, warm_scan_secs).await?;

    let device = adapter.device(addr).context("adapter.device")?;
    println!("Device {} (adapter {})", addr, adapter.name());

    // GNOME often sets Trusted for connectable LE peripherals; helps unpaired devices (Paired=no in Settings).
    if let Err(e) = device.set_trusted(true).await {
        eprintln!("Warning: could not set Trusted=true ({e}); connect may still work");
    }

    if reconnect {
        println!(
            "Reconnecting (disconnect + connect, {:?} timeout)…",
            connect_timeout
        );
        device_disconnect_best_effort(&device).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        device_connect(&device, connect_timeout, nus_profile).await?;
    } else {
        // Same as GNOME Settings / `bluetoothctl connect`: call org.bluez.Device1.Connect.
        // BlueZ returns immediately if the ACL is already up; no pre-poll loop.
        println!("Connecting (timeout {:?})…", connect_timeout);
        device_connect(&device, connect_timeout, nus_profile).await?;
    }

    if pair {
        println!("Pairing…");
        device.pair().await.context("pair")?;
    }

    Ok(device)
}

/// Connect and resolve NUS TX + notify (used by `find` so firmware matches APK: notify on before UART TX).
async fn connect_nus_tx_notify(
    adapter: &Adapter,
    addr_str: &str,
    write_uuid_str: &str,
    reconnect: bool,
    warm_scan_secs: u64,
    pair: bool,
    connect_timeout: Duration,
    nus_profile: Option<Uuid>,
) -> anyhow::Result<(bluer::Device, Characteristic, Characteristic)> {
    let device = connect_ble_device(
        adapter,
        addr_str,
        reconnect,
        warm_scan_secs,
        pair,
        connect_timeout,
        nus_profile,
    )
    .await?;
    let write_uuid = Uuid::parse_str(write_uuid_str)
        .with_context(|| format!("invalid UUID: {write_uuid_str}"))?;
    let notify_uuid = nus_notify_uuid_from_tx(write_uuid_str)?;

    let write_ch = find_characteristic(&device, write_uuid)
        .await
        .with_context(|| format!("GATT characteristic not found: {write_uuid}"))?;
    let notify_ch = find_characteristic(&device, notify_uuid)
        .await
        .with_context(|| format!("GATT notify characteristic not found: {notify_uuid}"))?;
    Ok((device, write_ch, notify_ch))
}

async fn connect_uart(
    adapter: &Adapter,
    addr_str: &str,
    write_uuid_str: &str,
    reconnect: bool,
    warm_scan_secs: u64,
    pair: bool,
    connect_timeout: Duration,
    nus_profile: Option<Uuid>,
) -> anyhow::Result<(bluer::Device, Characteristic)> {
    let device = connect_ble_device(
        adapter,
        addr_str,
        reconnect,
        warm_scan_secs,
        pair,
        connect_timeout,
        nus_profile,
    )
    .await?;
    let write_uuid = Uuid::parse_str(write_uuid_str)
        .with_context(|| format!("invalid UUID: {write_uuid_str}"))?;
    let ch = find_characteristic(&device, write_uuid)
        .await
        .with_context(|| format!("GATT characteristic not found: {write_uuid}"))?;
    Ok((device, ch))
}

async fn device_disconnect_best_effort(device: &Device) {
    match tokio::time::timeout(BLE_DISCONNECT_TIMEOUT, device.disconnect()).await {
        Ok(Ok(())) | Ok(Err(_)) | Err(_) => {}
    }
}

/// After **`Connect()`**, BlueZ may not list GATT until **`ServicesResolved`** is true; polling avoids
/// intermittent “characteristic not found” on the first **`device.services()`** pass.
async fn wait_gatt_ready_for_upload(device: &Device) {
    const POLL_MS: u64 = 150;
    const MAX_WAIT: Duration = Duration::from_secs(30);
    let deadline = Instant::now() + MAX_WAIT;
    loop {
        match device.is_services_resolved().await {
            Ok(true) => {
                println!("GATT: ServicesResolved=true — cache ready for NUS lookup");
                return;
            }
            Ok(false) => {}
            Err(e) => {
                eprintln!("Warning: is_services_resolved: {e} — continuing");
                return;
            }
        }
        if Instant::now() >= deadline {
            eprintln!(
                "Warning: ServicesResolved still false after {:?} — continuing (first GATT access may populate services)",
                MAX_WAIT
            );
            return;
        }
        tokio::time::sleep(Duration::from_millis(POLL_MS)).await;
    }
}

/// Read `Connected` and `ServicesResolved` (debug / `is-connected` only — **not** for skip-`Connect` logic).
async fn read_bluez_link_props(device: &Device) -> anyhow::Result<(bool, bool)> {
    let c = match tokio::time::timeout(BLE_IS_CONNECTED_TIMEOUT, device.is_connected()).await {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => return Err(e.into()),
        Err(_) => {
            eprintln!(
                "Warning: is_connected() slower than {:?} — treating as false this round",
                BLE_IS_CONNECTED_TIMEOUT
            );
            false
        }
    };
    let s = match tokio::time::timeout(BLE_IS_CONNECTED_TIMEOUT, device.is_services_resolved()).await {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => return Err(e.into()),
        Err(_) => {
            eprintln!(
                "Warning: is_services_resolved() slower than {:?} — treating as false this round",
                BLE_IS_CONNECTED_TIMEOUT
            );
            false
        }
    };
    Ok((c, s))
}

/// BlueZ `Connect` can sit silent for a long time; we log every 2s and honour `timeout`.
async fn device_connect(
    device: &Device,
    timeout: Duration,
    nus_profile: Option<Uuid>,
) -> anyhow::Result<()> {
    let limit_secs = timeout.as_secs().max(1);
    if let Some(ref u) = nus_profile {
        println!(
            "Using BlueZ ConnectProfile({}) — NUS service (use plain connect without --nus-profile-connect if this fails)",
            u
        );
    }
    let progress = tokio::spawn(async move {
        let start = Instant::now();
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            eprintln!(
                "  …still connecting ({:.0}s / {}s) — Ctrl+C to abort; phone Bluetooth off; or try --nus-profile-connect",
                start.elapsed().as_secs_f32(),
                limit_secs
            );
        }
    });

    let op = async {
        match nus_profile {
            Some(u) => device.connect_profile(&u).await,
            None => device.connect().await,
        }
    };
    let result = tokio::time::timeout(timeout, op).await;
    progress.abort();

    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(e).context(
            "BlueZ Connect/ConnectProfile — if the device was never discovered, use `--warm-scan-secs 4` or connect once from Settings; if generic Connect hangs on LE-only gear, try `--nus-profile-connect`",
        ),
        Err(_) => bail!(
            "BLE connect timed out after {:?} — disconnect the **phone app**, turn off phone Bluetooth, then retry (or try `find --nus-profile-connect`)",
            timeout
        ),
    }
}

async fn find_characteristic(device: &Device, want: Uuid) -> anyhow::Result<Characteristic> {
    match tokio::time::timeout(GATT_DISCOVER_TIMEOUT, find_characteristic_inner(device, want)).await {
        Ok(inner) => inner,
        Err(_elapsed) => bail!(
            "GATT discovery timed out after {:?} — try `--reconnect` or `bluetoothctl disconnect <MAC>` then retry",
            GATT_DISCOVER_TIMEOUT
        ),
    }
}

async fn find_characteristic_inner(device: &Device, want: Uuid) -> anyhow::Result<Characteristic> {
    for service in device.services().await? {
        for ch in service.characteristics().await? {
            if ch.uuid().await? == want {
                return Ok(ch);
            }
        }
    }
    bail!("characteristic {want} not found");
}

/// Read **`org.bluez.Device1`** **`Connected`** and **`ServicesResolved`** (no `Connect`). Exit **1** if
/// **`Connected`** is false — that matches when BlueZ needs **`Connect()`** (see bluer `gatt_client` example).
async fn cmd_is_connected(
    adapter: &Adapter,
    addr_str: &str,
    warm_scan_secs: u64,
) -> anyhow::Result<()> {
    let addr: Address = addr_str
        .parse()
        .with_context(|| format!("invalid BLE address: {addr_str}"))?;

    warm_scan_le_if_unseen(adapter, addr, warm_scan_secs).await?;

    let device = adapter.device(addr).context("adapter.device")?;

    println!("{} (adapter {})", addr, adapter.name());
    let (c, s) = read_bluez_link_props(&device).await?;
    println!("  org.bluez.Device1.Connected         = {c}");
    println!("  org.bluez.Device1.ServicesResolved  = {s}");
    println!("  ACL up (skip Connect if true)       = {c}");
    if !c {
        bail!("not connected per BlueZ (Connected=false)");
    }
    Ok(())
}

/// Standard **Battery Level** characteristic UUID (`0x2A19`).
const BATTERY_LEVEL_CHAR_UUID: &str = "00002a19-0000-1000-8000-00805f9b34fb";

/// Subscribe to **Battery Level** NOTIFY when available (prints only when the device pushes an update).
/// If NOTIFY is missing or subscribe fails, falls back to periodic GATT reads (`--interval-secs`).
async fn cmd_battery_watch(
    adapter: &Adapter,
    addr_str: &str,
    interval_secs: u64,
    duration_secs: u64,
    disconnect_after: bool,
) -> anyhow::Result<()> {
    let addr: Address = addr_str
        .parse()
        .with_context(|| format!("invalid BLE address: {addr_str}"))?;

    warm_scan_le_if_unseen(adapter, addr, 0).await?;

    let device = adapter.device(addr).context("adapter.device")?;
    println!("Device {} (adapter {})", addr, adapter.name());
    if let Err(e) = device.set_trusted(true).await {
        eprintln!("Warning: could not set Trusted=true ({e})");
    }

    println!("Connecting (timeout {:?})…", BLE_CONNECT_TIMEOUT);
    device_connect(&device, BLE_CONNECT_TIMEOUT, None).await?;

    let bat_uuid = Uuid::parse_str(BATTERY_LEVEL_CHAR_UUID).context("battery level uuid")?;
    let ch = find_characteristic(&device, bat_uuid).await?;
    let flags = ch.flags().await.context("battery char flags")?;

    println!(
        "Battery Level (0x2A19): READ={} NOTIFY={}",
        flags.read, flags.notify
    );

    let notify_stream = if flags.notify {
        match tokio::time::timeout(NOTIFY_ENABLE_TIMEOUT, ch.notify()).await {
            Ok(Ok(s)) => Some(s),
            Ok(Err(e)) => {
                eprintln!("notify subscribe failed ({e}); falling back to periodic reads");
                None
            }
            Err(_) => {
                eprintln!("notify subscribe timed out; falling back to periodic reads");
                None
            }
        }
    } else {
        None
    };

    let end_at: Option<Instant> = if duration_secs > 0 {
        Some(Instant::now() + Duration::from_secs(duration_secs))
    } else {
        None
    };
    let end_at_tokio: Option<tokio::time::Instant> = end_at.map(tokio::time::Instant::from_std);

    if let Some(ns) = notify_stream {
        pin_mut!(ns);
        println!("Subscribed to NOTIFY only — output when the device pushes a new battery level (no polling reads).");
        if end_at.is_some() {
            println!("Stopping after {duration_secs}s (Ctrl+C to exit early).");
        } else {
            println!("Press Ctrl+C to stop.");
        }
        loop {
            if let Some(end) = end_at {
                if Instant::now() >= end {
                    println!("Duration elapsed.");
                    break;
                }
            }
            tokio::select! {
                n = ns.next() => {
                    match n {
                        Some(data) => println!(
                            "[{}] {}",
                            Local::now().format("%H:%M:%S"),
                            device_info_gatt::decode_battery_level(&data)
                        ),
                        None => {
                            eprintln!("notify stream ended");
                            break;
                        }
                    }
                }
                _ = tokio::time::sleep_until(end_at_tokio.unwrap()), if end_at_tokio.is_some() => {
                    println!("Duration elapsed.");
                    break;
                }
                _ = tokio::signal::ctrl_c() => {
                    println!("Interrupted.");
                    break;
                }
            }
        }
    } else {
        let step = interval_secs.max(1);
        let mut ticker = tokio::time::interval(Duration::from_secs(step));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        eprintln!(
            "No NOTIFY subscription — polling with GATT read every {}s.",
            step
        );
        if end_at.is_some() {
            println!(
                "Sampling every {}s for {}s total (Ctrl+C to stop early).",
                step, duration_secs
            );
        } else {
            println!("Sampling every {}s until Ctrl+C.", step);
        }
        loop {
            if let Some(end) = end_at {
                if Instant::now() >= end {
                    println!("Duration elapsed.");
                    break;
                }
            }
            tokio::select! {
                _ = ticker.tick() => {
                    if flags.read {
                        match ch.read().await {
                            Ok(raw) => println!(
                                "[{}] {}",
                                Local::now().format("%H:%M:%S"),
                                device_info_gatt::decode_battery_level(&raw)
                            ),
                            Err(e) => eprintln!("read error: {e}"),
                        }
                    } else {
                        eprintln!("characteristic not readable and NOTIFY unavailable — nothing to do");
                        break;
                    }
                }
                _ = tokio::signal::ctrl_c() => {
                    println!("Interrupted.");
                    break;
                }
            }
        }
    }

    if disconnect_after {
        device_disconnect_best_effort(&device).await;
        println!("Disconnected.");
    }
    Ok(())
}

/// GATT **Device Information** (0x180A) — decoded reads like BLE scanner apps.
async fn cmd_device_info(
    adapter: &Adapter,
    addr_str: &str,
    disconnect_after: bool,
) -> anyhow::Result<()> {
    let addr: Address = addr_str
        .parse()
        .with_context(|| format!("invalid BLE address: {addr_str}"))?;

    warm_scan_le_if_unseen(adapter, addr, 0).await?;

    let device = adapter.device(addr).context("adapter.device")?;
    println!("Device {} (adapter {})", addr, adapter.name());

    if let Err(e) = device.set_trusted(true).await {
        eprintln!("Warning: could not set Trusted=true ({e})");
    }

    println!("Connecting (timeout {:?})…", BLE_CONNECT_TIMEOUT);
    device_connect(&device, BLE_CONNECT_TIMEOUT, None).await?;

    device_info_gatt::print_device_information(&device).await?;
    device_info_gatt::print_battery_service(&device).await?;

    if disconnect_after {
        device_disconnect_best_effort(&device).await;
        println!("Disconnected.");
    }
    Ok(())
}

/// BlueZ `Connect()` / `ConnectProfile` — for testing (same D-Bus call as Settings / `bluetoothctl connect`).
async fn cmd_connect(
    adapter: &Adapter,
    addr_str: &str,
    warm_scan_secs: u64,
    connect_timeout: Duration,
    nus_profile: Option<Uuid>,
) -> anyhow::Result<()> {
    let addr: Address = addr_str
        .parse()
        .with_context(|| format!("invalid BLE address: {addr_str}"))?;

    warm_scan_le_if_unseen(adapter, addr, warm_scan_secs).await?;

    let device = adapter.device(addr).context("adapter.device")?;

    println!("{} (adapter {})", addr, adapter.name());

    if let Err(e) = device.set_trusted(true).await {
        eprintln!("Warning: could not set Trusted=true ({e})");
    }

    println!("Calling BlueZ Connect (timeout {connect_timeout:?})…");
    device_connect(&device, connect_timeout, nus_profile).await?;
    println!("Done.");
    Ok(())
}

async fn cmd_disconnect(
    adapter: &Adapter,
    addr_str: &str,
    warm_scan_secs: u64,
) -> anyhow::Result<()> {
    let addr: Address = addr_str
        .parse()
        .with_context(|| format!("invalid BLE address: {addr_str}"))?;

    warm_scan_le_if_unseen(adapter, addr, warm_scan_secs).await?;

    let device = adapter.device(addr).context("adapter.device")?;

    println!("{} (adapter {})", addr, adapter.name());
    println!("Calling BlueZ Disconnect (timeout {:?})…", BLE_DISCONNECT_TIMEOUT);
    match tokio::time::timeout(BLE_DISCONNECT_TIMEOUT, device.disconnect()).await {
        Ok(Ok(())) => println!("Disconnect OK."),
        Ok(Err(e)) => return Err(e).context("Device::disconnect"),
        Err(_) => bail!("disconnect timed out after {:?}", BLE_DISCONNECT_TIMEOUT),
    }
    Ok(())
}

async fn cmd_scan(
    adapter: &Adapter,
    seconds: u64,
    name_contains: Option<&str>,
) -> anyhow::Result<()> {
    let filter = DiscoveryFilter {
        transport: DiscoveryTransport::Le,
        ..Default::default()
    };
    adapter.set_discovery_filter(filter).await?;
    println!("LE scanning for {seconds}s on {}…", adapter.name());

    let events = adapter.discover_devices().await?;
    pin_mut!(events);

    let scan = async {
        while let Some(evt) = events.next().await {
            if let AdapterEvent::DeviceAdded(addr) = evt {
                let dev = match adapter.device(addr) {
                    Ok(d) => d,
                    Err(_) => continue,
                };
                let name = dev.name().await.ok().flatten().unwrap_or_default();
                if let Some(needle) = name_contains {
                    if !name.to_lowercase().contains(&needle.to_lowercase()) {
                        continue;
                    }
                }
                let rssi = dev.rssi().await.ok().flatten();
                println!("  {addr}  RSSI={rssi:?}  name={name:?}");
            }
        }
    };

    let _ = tokio::time::timeout(Duration::from_secs(seconds), scan).await;
    println!("Scan window ended.");
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_payload_matches_apk() {
        let p = switch_protocol(CMD_SETTINGS, KEY_SETTING_FIND_ME, 1);
        assert_eq!(p.len(), 9);
        assert_eq!(p, [0xCD, 0x00, 0x06, 0x12, 0x01, 0x0B, 0x00, 0x01, 0x01]);
    }

    /// `upload-2.log.pcapng` first NUS writes (SwitchProtocol).
    #[test]
    fn switch_protocol_matches_upload2_capture() {
        assert_eq!(
            switch_protocol(CMD_SETTINGS, KEY_SETTING_LANGUAGE, 1),
            [0xCD, 0x00, 0x06, 0x12, 0x01, 0x15, 0x00, 0x01, 0x01]
        );
        assert_eq!(
            switch_protocol(CMD_SPORT, KEY_SPORT_REALTIME_STEP, 1),
            [0xCD, 0x00, 0x06, 0x15, 0x01, 0x06, 0x00, 0x01, 0x01]
        );
        assert_eq!(
            switch_protocol(CMD_SETTINGS, KEY_SETTING_CLASSIC_BT_ADDR, 1),
            [0xCD, 0x00, 0x06, 0x12, 0x01, 0x0A, 0x00, 0x01, 0x01]
        );
        assert_eq!(PREFLIGHT_UPLOAD2_DC1, [0xDC, 0x00, 0x05, 0x15, 0x0C, 0x00, 0x1E, 0x01]);
        assert_eq!(PREFLIGHT_UPLOAD2_DC2, [0xDC, 0x00, 0x05, 0x20, 0x02, 0x00, 0x28, 0x01]);
        assert_eq!(PREFLIGHT_UPLOAD2_DC_BEFORE_WEATHER, [
            0xDC, 0x00, 0x05, 0x20, 0x03, 0x00, 0x12, 0x01
        ]);
        assert_eq!(
            PREFLIGHT_UPLOAD2_WEATHER_FRAG1,
            [
                0xCD, 0x00, 0x4D, 0x12, 0x01, 0x20, 0x00, 0x48, 0x0A, 0x4C, 0x69, 0x74, 0x68, 0x65,
                0x72, 0x6C, 0x61, 0x6E, 0x64, 0x03
            ]
        );
    }

    #[test]
    fn default_uuids_parse() {
        Uuid::parse_str(DEFAULT_WRITE_UUID).unwrap();
        Uuid::parse_str(APK_UART_TX).unwrap();
    }

    #[test]
    fn nus_notify_derivation() {
        assert_eq!(
            nus_notify_uuid_from_tx(DEFAULT_WRITE_UUID).unwrap().to_string(),
            DEFAULT_NOTIFY_UUID
        );
        assert_eq!(
            nus_notify_uuid_from_tx(APK_UART_TX).unwrap().to_string(),
            APK_UART_NOTIFY
        );
    }

    #[test]
    fn nus_service_derivation() {
        assert_eq!(
            nus_service_uuid_from_tx(DEFAULT_WRITE_UUID).unwrap().to_string(),
            "7e400001-b5a3-f393-e0a9-e50e24dcca9d"
        );
        assert_eq!(
            nus_service_uuid_from_tx(APK_UART_TX).unwrap().to_string(),
            "6e400001-b5a3-f393-e0a9-e50e24dcca9d"
        );
    }

    #[test]
    fn get_protocol_time_sync_shape() {
        // 4-byte payload => 12-byte frame (matches SendData.getProtocol layout).
        let p = get_protocol(CMD_SETTINGS, KEY_SYNC_TIME, &[0xAB, 0xCD, 0xEF, 0x01]);
        assert_eq!(p.len(), 12);
        assert_eq!(p[0], 0xCD);
        assert_eq!(p[3], CMD_SETTINGS);
        assert_eq!(p[5], KEY_SYNC_TIME);
        assert_eq!(&p[8..], &[0xAB, 0xCD, 0xEF, 0x01]);
    }

    #[test]
    fn packed_time_matches_apk_bit_layout() {
        // 2000-01-01 00:00:00 — year offset 0, month 1, day 1, rest 0
        let packed: u32 =
            (0u32 & 0x3f) | (0u32 << 26) | (1u32 << 22) | (1u32 << 17) | (0u32 << 12) | (0u32 << 6);
        assert_eq!(packed, 0x0042_0000);
        assert_eq!(packed.to_be_bytes(), [0x00, 0x42, 0x00, 0x00]);
    }

    #[test]
    fn get_no_value_protocol_matches_apk() {
        // getSetInfoByKey(16) => cmd 26, sub 0x10
        let p = get_no_value_protocol(26, 16);
        assert_eq!(&p[..], &[0xCD, 0x00, 0x05, 0x1A, 0x01, 0x10, 0x00, 0x00]);
        // getFileSendStatus / getReadFileValue(1) => cmd 35, sub 1
        let f = get_no_value_protocol(CMD_FILE_SEND_STATUS, 1);
        assert_eq!(&f[..], &[0xCD, 0x00, 0x05, 0x23, 0x01, 0x01, 0x00, 0x00]);
    }
}
