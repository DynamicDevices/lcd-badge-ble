//! `dg01-ble` — Linux-only BLE tool using BlueZ via [bluer] (same DBus path as `bluetoothctl`).
//!
//! ```text
//! cd dg01-ble && cargo run --release -- find --addr 0A:93:79:0C:DD:20
//! cd dg01-ble && cargo run --release -- sync-time --addr 0A:93:79:0C:DD:20
//! cd dg01-ble && cargo run --release -- query --addr 0A:93:79:0C:DD:20
//! cd dg01-ble && cargo run --release -- scan --seconds 15
//! ```

use anyhow::{bail, Context};
use chrono::{Datelike, Local, Timelike};
use bluer::gatt::remote::Characteristic;
use bluer::{Adapter, AdapterEvent, Address, Device, DiscoveryFilter, DiscoveryTransport, Session};
use clap::{Parser, Subcommand};
use futures::{pin_mut, StreamExt};
use std::time::Duration;
use uuid::Uuid;

/// SuperBand "find device" / 寻找手环 — `SendData.getSetFindMeValue(true)` (`SwitchProtocol(18, 11, 1)`).
const FIND_DEVICE_ON: [u8; 9] = [0xCD, 0x00, 0x06, 0x12, 0x01, 0x0B, 0x00, 0x01, 0x01];

/// Command `18` = settings; sub-key `1` = sync time (`SendData.getSetTimesValue` / `SDKCmdMannager.synchronTime`).
const CMD_SETTINGS: u8 = 18;
const KEY_SYNC_TIME: u8 = 1;

/// NUS TX on DG01 (first octet 0x7E). SuperBand APK uses `6e400002-…`.
const DEFAULT_WRITE_UUID: &str = "7e400002-b5a3-f393-e0a9-e50e24dcca9d";
const DEFAULT_NOTIFY_UUID: &str = "7e400003-b5a3-f393-e0a9-e50e24dcca9d";
const APK_UART_TX: &str = "6e400002-b5a3-f393-e0a9-e50e24dcca9d";
const APK_UART_NOTIFY: &str = "6e400003-b5a3-f393-e0a9-e50e24dcca9d";

/// `SendData.getSetInfoByKey` → `getNoValueProtocol((byte)26, key)`.
const CMD_GET_INFO_BY_KEY: u8 = 26;
/// `SendData.getReadDialValue` → `getNoValueProtocol((byte)32, key)`.
const CMD_DIAL_READ: u8 = 32;

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
    /// LE scan and print seen devices (uses BlueZ discovery)
    Scan {
        #[arg(long, default_value_t = 15)]
        seconds: u64,

        /// Only print devices whose name contains this substring (case-insensitive)
        #[arg(long)]
        name_contains: Option<String>,
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
        } => {
            let uuid = if apk_uart {
                APK_UART_TX.to_string()
            } else {
                write_uuid
            };
            cmd_find(&adapter, &addr, &uuid, disconnect).await?;
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
        Command::Scan {
            seconds,
            name_contains,
        } => {
            cmd_scan(&adapter, seconds, name_contains.as_deref()).await?;
        }
    }
    Ok(())
}

async fn cmd_find(
    adapter: &Adapter,
    addr_str: &str,
    write_uuid_str: &str,
    disconnect_after: bool,
) -> anyhow::Result<()> {
    let (device, ch) = connect_uart(adapter, addr_str, write_uuid_str).await?;

    println!("Writing find payload: {}", hex(&FIND_DEVICE_ON));
    ch.write(&FIND_DEVICE_ON).await.context("write")?;
    println!("Done. Check the device for locate / colours.");

    if disconnect_after {
        device.disconnect().await.ok();
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
    let (device, ch) = connect_uart(adapter, addr_str, write_uuid_str).await?;
    let payload = build_set_times_value();
    println!(
        "Local time: {}",
        Local::now().format("%Y-%m-%d %H:%M:%S %:z")
    );
    println!("Writing time sync: {}", hex(&payload));
    ch.write(&payload).await.context("write")?;
    println!("Done. If supported, the device clock should match this machine’s local time.");

    if disconnect_after {
        device.disconnect().await.ok();
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

    if !device.is_connected().await? {
        println!("Connecting…");
        device.connect().await.context("connect")?;
    } else {
        println!("Already connected.");
    }

    let write_ch = find_characteristic(&device, wu)
        .await
        .with_context(|| format!("write characteristic not found: {write_uuid_str}"))?;
    let notify_ch = find_characteristic(&device, nu)
        .await
        .with_context(|| format!("notify characteristic not found: {notify_uuid_str}"))?;

    println!("Subscribing to notifications…");
    let notify_stream = notify_ch.notify().await.context("notify()")?;
    pin_mut!(notify_stream);

    let wait = Duration::from_millis(response_timeout_ms);
    let gap = Duration::from_millis(gap_ms);

    for &key in info_keys {
        let frame = get_no_value_protocol(CMD_GET_INFO_BY_KEY, key);
        println!("\ncmd26 key {key}: write {}", hex(&frame));
        write_ch.write(&frame).await.context("write")?;
        match tokio::time::timeout(wait, notify_stream.next()).await {
            Ok(Some(data)) => println!("  notify: {}", hex(&data)),
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
            Ok(Some(data)) => println!("  notify: {}", hex(&data)),
            Ok(None) => println!("  (notify stream ended)"),
            Err(_) => println!("  (timeout, no notify)"),
        }
        tokio::time::sleep(gap).await;
    }

    if disconnect_after {
        device.disconnect().await.ok();
        println!("\nDisconnected.");
    }
    Ok(())
}

async fn connect_uart(
    adapter: &Adapter,
    addr_str: &str,
    write_uuid_str: &str,
) -> anyhow::Result<(bluer::Device, Characteristic)> {
    let addr: Address = addr_str
        .parse()
        .with_context(|| format!("invalid BLE address: {addr_str}"))?;
    let write_uuid = Uuid::parse_str(write_uuid_str)
        .with_context(|| format!("invalid UUID: {write_uuid_str}"))?;

    let device = adapter.device(addr).context("adapter.device")?;
    println!("Device {} (adapter {})", addr, adapter.name());

    if !device.is_connected().await? {
        println!("Connecting…");
        device.connect().await.context("connect")?;
    } else {
        println!("Already connected.");
    }

    let ch = find_characteristic(&device, write_uuid)
        .await
        .with_context(|| format!("GATT characteristic not found: {write_uuid}"))?;
    Ok((device, ch))
}

async fn find_characteristic(device: &Device, want: Uuid) -> anyhow::Result<Characteristic> {
    for service in device.services().await? {
        for ch in service.characteristics().await? {
            if ch.uuid().await? == want {
                return Ok(ch);
            }
        }
    }
    bail!("characteristic {want} not found");
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
        assert_eq!(FIND_DEVICE_ON.len(), 9);
        assert_eq!(FIND_DEVICE_ON[0], 0xCD);
        assert_eq!(FIND_DEVICE_ON[3], 18);
        assert_eq!(FIND_DEVICE_ON[5], 11);
        assert_eq!(FIND_DEVICE_ON[8], 1);
    }

    #[test]
    fn default_uuids_parse() {
        Uuid::parse_str(DEFAULT_WRITE_UUID).unwrap();
        Uuid::parse_str(APK_UART_TX).unwrap();
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
    }
}
