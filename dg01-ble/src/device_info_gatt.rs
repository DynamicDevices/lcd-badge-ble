//! Standard Bluetooth **GATT** services — **Device Information** (0x180A) and **Battery** (0x180F) — read + decode
//! like nRF Connect / BLE scanner UIs.

use anyhow::Context;
use bluer::gatt::remote::Characteristic;
use bluer::Device;
use uuid::Uuid;

/// SIG base: 128-bit UUID from 16-bit alias `0xXXXX`.
fn uuid_from_u16(short: u16) -> Uuid {
    Uuid::parse_str(&format!("{:08x}-0000-1000-8000-00805f9b34fb", u32::from(short))).expect("uuid")
}

const DIS_SERVICE: u16 = 0x180a;
const BATTERY_SERVICE: u16 = 0x180f;

/// Standard DIS characteristic UUIDs (16-bit) → display name (Bluetooth DIS / adopted spec).
fn dis_characteristic_title(short: u16) -> &'static str {
    match short {
        0x2a23 => "System ID",
        0x2a24 => "Model Number",
        0x2a25 => "Serial Number",
        0x2a26 => "Firmware Revision String",
        0x2a27 => "Hardware Revision String",
        0x2a28 => "Software Revision String",
        0x2a29 => "Manufacturer Name String",
        0x2a2a => "IEEE Regulatory Certification Data List",
        0x2a50 => "PnP ID",
        _ => "Vendor characteristic",
    }
}

fn bas_characteristic_title(short: u16) -> &'static str {
    match short {
        0x2a19 => "Battery Level",
        _ => "Vendor characteristic",
    }
}

/// **Battery Level** (`0x2A19`) — SIG is **one** octet 0–100 ([Battery Service](https://www.bluetooth.com/specifications/specs/battery-service/)).
/// Extra octets are ignored; the first octet is treated as percentage when ≤100.
pub fn decode_battery_level(raw: &[u8]) -> String {
    if raw.is_empty() {
        return "(empty read)".to_string();
    }
    let first = raw[0];
    if first <= 100 {
        format!("{first}%")
    } else {
        format!("first octet 0x{first:02x} (>100 — reserved per SIG or vendor encoding)")
    }
}

fn u16_from_uuid(u: &Uuid) -> Option<u16> {
    let s = u.to_string();
    // `00002a26-0000-1000-8000-00805f9b34fb`
    let first = s.get(..8)?;
    if !first.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(first, 16).ok().map(|v| v as u16)
}

/// Decode **PnP ID** (`0x2A50`) per Bluetooth Device Information Service (7 octets).
fn decode_pnp_id(raw: &[u8]) -> String {
    if raw.len() < 7 {
        return format!(
            "(unexpected length {} — need 7): {}",
            raw.len(),
            hex_bytes(raw)
        );
    }
    let src = raw[0];
    let src_label = match src {
        0x01 => "Bluetooth SIG (assigned company identifier)",
        0x02 => "USB Implementer's Forum",
        _ => "Reserved / unknown",
    };
    let vendor = u16::from_le_bytes([raw[1], raw[2]]);
    let product = u16::from_le_bytes([raw[3], raw[4]]);
    let version = u16::from_le_bytes([raw[5], raw[6]]);
    format!(
        "Vendor ID Source: {src_label}\n    Vendor ID: {vendor} (hex 0x{vendor:04x})\n    Product ID: {product}\n    Product Version: {version}"
    )
}

fn hex_bytes(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ")
}

fn decode_utf8_string(raw: &[u8]) -> String {
    let t = String::from_utf8_lossy(raw);
    let t = t.trim_end_matches('\0').trim();
    if t.is_empty() {
        format!("(empty) hex {}", hex_bytes(raw))
    } else {
        format!("{t:?}")
    }
}

fn format_properties(flags: &bluer::gatt::CharacteristicFlags) -> String {
    // `CharacteristicFlags` uses public bool fields (bluer `define_flags!`).
    let mut v = Vec::new();
    if flags.read {
        v.push("READ");
    }
    if flags.write {
        v.push("WRITE");
    }
    if flags.write_without_response {
        v.push("WRITE_WITHOUT_RESPONSE");
    }
    if flags.notify {
        v.push("NOTIFY");
    }
    if flags.indicate {
        v.push("INDICATE");
    }
    if v.is_empty() {
        format!("{flags:?}")
    } else {
        v.join(", ")
    }
}

/// Print **Device Information (0x180A)** in a scanner-app style (service header, per-characteristic blocks).
pub async fn print_device_information(device: &Device) -> anyhow::Result<()> {
    let want_service = uuid_from_u16(DIS_SERVICE);
    let mut found_service = false;

    for service in device.services().await.context("GATT services")? {
        let su = service.uuid().await.context("service uuid")?;
        if su != want_service {
            continue;
        }
        found_service = true;
        println!();
        println!("Device Information ({:#06x})", DIS_SERVICE);
        println!("{}", "—".repeat(48));

        let mut chars: Vec<Characteristic> = service.characteristics().await.context("characteristics")?;
        chars.sort_by_key(|c| c.id());

        for ch in chars {
            let u = ch.uuid().await.context("char uuid")?;
            let flags = ch.flags().await.context("char flags")?;
            let short_opt = u16_from_uuid(&u);
            let title = short_opt
                .map(dis_characteristic_title)
                .unwrap_or("Vendor characteristic");

            println!();
            println!("{title}");
            if let Some(s) = short_opt {
                println!("  UUID: {:#06x}", s);
            } else {
                println!("  UUID: {u}");
            }
            println!("  Properties: {}", format_properties(&flags));

            if !flags.read {
                println!("  Value: (not readable — enable READ or use another tool)");
                continue;
            }

            match ch.read().await {
                Ok(raw) => {
                    if short_opt == Some(0x2a50) {
                        println!("  Value:");
                        for line in decode_pnp_id(&raw).lines() {
                            println!("    {line}");
                        }
                    } else {
                        println!("  Value: {}", decode_utf8_string(&raw));
                    }
                    println!("  Raw: {}", hex_bytes(&raw));
                }
                Err(e) => {
                    println!("  Read error: {e}");
                }
            }
        }
        break;
    }

    if !found_service {
        anyhow::bail!(
            "Device Information service (0x180A) not found — connect first and ensure GATT cache is resolved"
        );
    }
    Ok(())
}

/// Print **Battery Service (0x180F)** — **Battery Level (0x2A19)** as percentage when readable.
pub async fn print_battery_service(device: &Device) -> anyhow::Result<()> {
    let want_service = uuid_from_u16(BATTERY_SERVICE);
    let mut found_service = false;

    for service in device.services().await.context("GATT services")? {
        let su = service.uuid().await.context("service uuid")?;
        if su != want_service {
            continue;
        }
        found_service = true;
        println!();
        println!("Battery Service ({:#06x})", BATTERY_SERVICE);
        println!("{}", "—".repeat(48));

        let mut chars: Vec<Characteristic> = service.characteristics().await.context("characteristics")?;
        chars.sort_by_key(|c| c.id());

        for ch in chars {
            let u = ch.uuid().await.context("char uuid")?;
            let flags = ch.flags().await.context("char flags")?;
            let short_opt = u16_from_uuid(&u);
            let title = short_opt
                .map(bas_characteristic_title)
                .unwrap_or("Vendor characteristic");

            println!();
            println!("{title}");
            if let Some(s) = short_opt {
                println!("  UUID: {:#06x}", s);
            } else {
                println!("  UUID: {u}");
            }
            println!("  Properties: {}", format_properties(&flags));
            if flags.notify {
                println!("  Note: NOTIFY supported — enable CCCD (0x2902) in a scanner to stream updates");
            }

            if !flags.read {
                println!("  Value: (not readable — use NOTIFY or another tool)");
                continue;
            }

            match ch.read().await {
                Ok(raw) => {
                    let decoded = if short_opt == Some(0x2a19) {
                        decode_battery_level(&raw)
                    } else {
                        decode_utf8_string(&raw)
                    };
                    println!("  Value: {decoded}");
                    println!("  Raw: {}", hex_bytes(&raw));
                }
                Err(e) => println!("  Read error: {e}"),
            }
        }
        break;
    }

    if !found_service {
        println!();
        println!("Battery Service ({:#06x}): not found on this device.", BATTERY_SERVICE);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sample PnP ID: USB IF, vendor 0x248a, product 0x8276, version 1 (bytes LE).
    #[test]
    fn pnp_id_decode_matches_scanner_style() {
        let raw = [0x02u8, 0x8a, 0x24, 0x76, 0x82, 0x01, 0x00];
        let s = decode_pnp_id(&raw);
        assert!(s.contains("USB"));
        assert!(s.contains("0x248a"));
        assert!(s.contains("Product ID:"));
        assert!(s.contains("Product Version:"));
    }

    #[test]
    fn battery_level_percent() {
        assert_eq!(decode_battery_level(&[100]), "100%");
        assert_eq!(decode_battery_level(&[64]), "64%");
        assert_eq!(decode_battery_level(&[]), "(empty read)");
        assert_eq!(decode_battery_level(&[0x1a, 0x00]), "26%");
    }
}
