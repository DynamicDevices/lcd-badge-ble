//! Watchface / dial binary transfer — `WatchThemeTools` + `SendData` cmd **31** (FitPro APK).

use crate::get_protocol;

/// Command 31 sub-keys (`SendData`).
pub const SUB_DIAL_FILE: u8 = 1;
pub const SUB_DIAL_START: u8 = 2;
pub const SUB_DIAL_FINISH: u8 = 3;
pub const CMD_DIAL_TRANSFER: u8 = 31;
/// `BleFileSendTools` / `SendData.getFile*` — alternate file pipe used for some OEM uploads.
pub const CMD_FILE_UART: u8 = 34;
/// Incoming dial/watchface replies use cmd **32** (`BaseReceiveData` → `setDialUpdateInfo`), not 31.
pub const CMD_DIAL_NOTIFY: u8 = 32;
/// Sub-key **1** routes to `WatchThemeTools.response` (file chunk / dial transfer ACK).
pub const SUB_DIAL_NOTIFY_FILE: u8 = 1;
/// Sub-key **2** — `SendData.getDialClockInfo` / `getReadDialValue(2)` → `BaseReceiveData.parseDialInfo`.
pub const SUB_DIAL_NOTIFY_CLOCK_INFO: u8 = 2;

/// Reassembles `0xCD` frames split across multiple BLE notifications (`BaseReceiveData.testParse2`).
#[derive(Default)]
pub struct CdNotifyAssembler {
    buf: Vec<u8>,
}

impl CdNotifyAssembler {
    /// Append one notify payload; returns every **complete** `0xCD` packet (in order).
    pub fn push(&mut self, chunk: &[u8]) -> Vec<Vec<u8>> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        loop {
            if self.buf.is_empty() {
                break;
            }
            if self.buf[0] != 0xCD {
                if let Some(pos) = self.buf.iter().position(|&x| x == 0xCD) {
                    self.buf.drain(..pos);
                } else {
                    self.buf.clear();
                }
                continue;
            }
            if self.buf.len() < 3 {
                break;
            }
            let need = u16::from_be_bytes([self.buf[1], self.buf[2]]) as usize + 3;
            if self.buf.len() < need {
                break;
            }
            out.push(self.buf[..need].to_vec());
            self.buf.drain(..need);
        }
        out
    }
}

/// Status int for dial upload (`WatchThemeTools.response` / `parseDialUpCode`).
///
/// **Chunk ACKs** use **cmd 31 or 32** + sub **1** (same as APK `setDialUpdateInfo` when `bResultValueItem == 1`).
/// **Start ACK** (status **1000** before first file chunk — the step that arms the badge “uploading” UI in the
/// stock app) is sometimes sent as **cmd 31** + sub **2** (mirror of `getDialUpdateStartValue`), which we
/// previously ignored — leading to `--skip-start-ack` and no on-device upload splash.
///
/// ## Fatal protocol statuses (`< 1000`, not chunk ACK `>= 1000`)
///
/// Same mapping as **`WatchThemeTools.response`** / **`BleFileSendTools.response`** on the first payload **i32 BE**
/// (`BaseReceiveData.parseDialUpCode`). Stock Android toasts: `battery_low_not_dial_clock` (3), `charge_battery_not_dial_clock` (4).
pub fn parse_dial_watch_ack_status(packet: &[u8]) -> Option<i32> {
    let cmd = packet.get(3).copied()?;
    let sub = packet.get(5).copied()?;
    if cmd != CMD_DIAL_NOTIFY && cmd != CMD_DIAL_TRANSFER {
        return None;
    }
    let chunk_or_start_ack = sub == SUB_DIAL_NOTIFY_FILE
        || (cmd == CMD_DIAL_TRANSFER && sub == SUB_DIAL_START);
    if !chunk_or_start_ack {
        return None;
    }
    parse_cd_notify_status(packet)
}

/// `true` if firmware status `code` (`< 1000`) means the upload cannot proceed — abort waits instead of spinning.
///
/// Matches APK branches that call `upgradeFailed(...)` for device-reported errors (not **1001**/**1002** timeouts, which are client-side).
pub fn is_watch_theme_fatal_protocol_status(code: i32) -> bool {
    matches!(code, 1 | 3 | 4 | 5 | 7)
}

/// Human-readable reason for a fatal **`WatchThemeTools`** / **`BleFileSendTools`** protocol status (`< 1000`).
///
/// Returns **`None`** for non-fatal or unknown codes (e.g. chunk **1000+n** uses `>= 1000`).
pub fn watch_theme_protocol_error_message(code: i32) -> Option<&'static str> {
    match code {
        1 => Some("check failed (APK ERROR_CHECK 1003 — verify / checksum)"),
        3 => Some("battery too low to upgrade dial (APK ERROR_BATTERY_LOW 1008)"),
        4 => Some("charging — device refuses watch face upgrade (APK ERROR_CHARGE_BATTERY 1009)"),
        5 => Some("out of memory (APK ERROR_OUT_OF_MEMORY 1010; cmd 31 path)"),
        7 => Some("unknown / not ready (APK ERROR_UNKNOWN 1007)"),
        _ => None,
    }
}

/// `SendData.getDialDeviceContrlReponse` — **cmd 32** sub **3** with a **single** payload byte (not empty `getNoValueProtocol`).
pub fn dial_device_control_response(control: u8) -> Vec<u8> {
    get_protocol(CMD_DIAL_NOTIFY, SUB_DIAL_FINISH, &[control])
}

/// `SendData.getDialUpdateStartValue` — font slot, custom flag, RGB (white in app = 255,255,255).
/// When `replace_pic_pos` is `Some`, appends one byte (`WatchThemeDetailsResponse.getReplacePicPos`) —
/// matches APK when `ClockDialInfoBody.getPictureNums() > 0`.
pub fn dial_start(
    font_position: u8,
    custom: u8,
    r: u8,
    g: u8,
    b: u8,
    replace_pic_pos: Option<u8>,
) -> Vec<u8> {
    let mut pl = vec![font_position, custom, r, g, b];
    if let Some(p) = replace_pic_pos {
        pl.push(p);
    }
    get_protocol(CMD_DIAL_TRANSFER, SUB_DIAL_START, &pl)
}

/// **17-byte** cmd 31/2 start payload seen in `logs/upload-2.log.pcapng` (stock app successful upload).
///
/// Layout: `font`, `custom`, **`mid4`** (OEM / layout — capture default `15 a2 00 08`), **`r,g,b`**,
/// **`file_len`** big-endian u32, then **four zero** bytes (reserved).
pub fn dial_start_extended(
    font_position: u8,
    custom: u8,
    mid4: [u8; 4],
    r: u8,
    g: u8,
    b: u8,
    file_len: u32,
) -> Vec<u8> {
    let mut pl = Vec::with_capacity(17);
    pl.push(font_position);
    pl.push(custom);
    pl.extend_from_slice(&mid4);
    pl.extend_from_slice(&[r, g, b]);
    pl.extend_from_slice(&file_len.to_be_bytes());
    pl.extend_from_slice(&[0u8; 4]);
    debug_assert_eq!(pl.len(), 17);
    get_protocol(CMD_DIAL_TRANSFER, SUB_DIAL_START, &pl)
}

/// `mid4` from screen dimensions as **big-endian** `width` then `height` (u16 each) — try when capture OEM bytes are wrong for your asset.
pub fn dial_start_mid4_dims_be(width: u16, height: u16) -> [u8; 4] {
    let mut m = [0u8; 4];
    m[0..2].copy_from_slice(&width.to_be_bytes());
    m[2..4].copy_from_slice(&height.to_be_bytes());
    m
}

/// Eight hex digits → four bytes (e.g. `15a20008` for capture default).
pub fn parse_mid4_hex(s: &str) -> anyhow::Result<[u8; 4]> {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if s.len() != 8 {
        anyhow::bail!("--dial-start-mid4 must be exactly 8 hex digits, got {} chars", s.len());
    }
    let mut out = [0u8; 4];
    for (i, chunk) in out.iter_mut().enumerate() {
        *chunk = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)
            .map_err(|_| anyhow::anyhow!("invalid hex in --dial-start-mid4"))?;
    }
    Ok(out)
}

/// Shared chunk layout: `getDialUpdateFileValue` / `getFileDataValue` (cmd 31 or 34, sub 1).
pub fn uart_file_chunk(cmd: u8, seq: u16, chunk: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(2 + chunk.len() + 2);
    body.extend_from_slice(&seq.to_be_bytes());
    body.extend_from_slice(chunk);
    let ck = checksum_seq_and_chunk(&body);
    body.extend_from_slice(&ck);
    get_protocol(cmd, SUB_DIAL_FILE, &body)
}

/// Body for `getDialUpdateFileValue` (cmd **31**).
pub fn dial_file_frame(seq: u16, chunk: &[u8]) -> Vec<u8> {
    uart_file_chunk(CMD_DIAL_TRANSFER, seq, chunk)
}

/// `getFileDataValue` — cmd **34**, sub **1** (same chunk layout as dial).
pub fn file34_file_frame(seq: u16, chunk: &[u8]) -> Vec<u8> {
    uart_file_chunk(CMD_FILE_UART, seq, chunk)
}

/// `getFileStartValue`: 8-byte len+sum payload, cmd **34** sub **2** (`BleFileSendTools.sendStartCmd`).
pub fn file34_start(file: &[u8]) -> Vec<u8> {
    let len = file.len() as u32;
    let sum: u32 = file.iter().map(|&b| u32::from(b)).sum();
    let mut pl = Vec::with_capacity(8);
    pl.extend_from_slice(&len.to_be_bytes());
    pl.extend_from_slice(&sum.to_be_bytes());
    get_protocol(CMD_FILE_UART, 2, &pl)
}

/// `getFileFinishValue` — `getNoValueProtocol(34, 3)`.
pub fn file34_finish_frame() -> Vec<u8> {
    let mut b = vec![0u8; 8];
    b[0] = 0xCD;
    let lm = 5u32;
    let lb = lm.to_be_bytes();
    b[1] = lb[2];
    b[2] = lb[3];
    b[3] = CMD_FILE_UART;
    b[4] = 1;
    b[5] = 3;
    b
}

fn checksum_seq_and_chunk(seq_plus_chunk: &[u8]) -> [u8; 2] {
    let mut s: u16 = 0;
    for &b in seq_plus_chunk {
        s = s.wrapping_add(b as u16);
    }
    s.to_be_bytes()
}

/// `calculateFinishCheckcode`: BE file length + BE sum of all file bytes (Java `int` sum; fits watchface sizes).
pub fn dial_finish_payload(file: &[u8]) -> Vec<u8> {
    let len = file.len() as u32;
    let sum: u32 = file.iter().map(|&b| u32::from(b)).sum();
    let mut tail = Vec::with_capacity(8);
    tail.extend_from_slice(&len.to_be_bytes());
    tail.extend_from_slice(&sum.to_be_bytes());
    get_protocol(CMD_DIAL_TRANSFER, SUB_DIAL_FINISH, &tail)
}

/// First 4 bytes of **payload** (after 8-byte `0xCD` header) as big-endian int — `BaseReceiveData.parseDialUpCode` / `NumberUtils.bytes2int`.
/// Short **`0xDC`** notify (`BaseReceiveData` first branch) — 8-byte frames, not merged with `0xCD`.
/// Example after `file34` start: `dc 00 05 22 02 00 10 00` (cmd **34**, sub **2**).
pub fn parse_dc_short(packet: &[u8]) -> Option<(u8, u8)> {
    if packet.first()? != &0xDC || packet.len() < 6 {
        return None;
    }
    Some((packet[3], packet[4]))
}

/// APK `getSetInfoByKey` sub-key names (cmd **26**) — partial map from `SendData` / setting keys; best-effort labels.
pub fn cmd26_info_key_label(key: u8) -> Option<&'static str> {
    Some(match key {
        1 => "personal / profile",
        10 => "classic Bluetooth address",
        12 => "device info (APK key 12)",
        15 => "info key 15",
        16 => "hardware revision string",
        17 => "info key 17",
        20 => "product / model string",
        21 => "language",
        _ => return None,
    })
}

/// Human-readable line for an 8-byte **`0xDC`** notify (cmd + sub in bytes 3–4; tail bytes 5–7 firmware-specific).
pub fn decode_dc_notify_line(packet: &[u8]) -> Option<String> {
    if packet.first().copied() != Some(0xDC) || packet.len() < 8 {
        return None;
    }
    let cmd = packet[3];
    let sub = packet[4];
    let t0 = packet[5];
    let t1 = packet[6];
    let t2 = packet[7];
    if cmd == 26 {
        let label = cmd26_info_key_label(sub)
            .map(|s| format!(" ({s})"))
            .unwrap_or_default();
        return Some(format!(
            "  decode: short 0xDC ack — getSetInfoByKey cmd26, key {sub}{label}; tail [{t0:02x} {t1:02x} {t2:02x}] (ACK/status — hardware/product text often follows in a 0xCD packet on some builds)"
        ));
    }
    Some(format!(
        "  decode: short 0xDC — cmd {cmd}, sub {sub}; tail [{t0:02x} {t1:02x} {t2:02x}]"
    ))
}

pub fn parse_cd_notify_status(packet: &[u8]) -> Option<i32> {
    if packet.first()? != &0xCD || packet.len() < 12 {
        return None;
    }
    let total_minus_3 = u16::from_be_bytes([packet[1], packet[2]]) as usize;
    if packet.len() < total_minus_3 + 3 {
        return None;
    }
    let p = &packet[8..];
    if p.len() < 4 {
        return None;
    }
    Some(i32::from_be_bytes([p[0], p[1], p[2], p[3]]))
}

/// Clock/dial screen info from **cmd 32** sub **2** (`BaseReceiveData.parseDialInfo`).
///
/// The APK hex-decodes the payload **after** the 8-byte `0xCD` header; first fields are:
/// `screenType`, `grade`, then **big-endian** `width`, `height` (`NumberUtils.bytesToShort` on each pair).
pub fn parse_dial_clock_info_cd(packet: &[u8]) -> Option<(u8, u8, u16, u16)> {
    parse_dial_clock_info_full(packet).map(|d| (d.screen_type, d.grade, d.width, d.height))
}

/// Parsed **`parseDialInfo`** payload (cmd **32** sub **2**), including **`config`** when present.
///
/// `config` is used by the APK (`NumberUtils.intToBinary(config)[1] == 1` → **120**-byte dial chunks else **200**).
#[derive(Clone, Debug)]
pub struct DialClockInfoParsed {
    pub screen_type: u8,
    pub grade: u8,
    pub width: u16,
    pub height: u16,
    /// Raw **`config`** octet from the device (`BaseReceiveData.parseDialInfo`), if the payload is long enough.
    pub config: Option<u8>,
}

/// APK `WatchThemeTools.WRITE_MAX_SIZE`: **120** if **`(config >> 1) & 1 == 1`**, else **200**.
pub fn apk_dial_chunk_size_from_config_byte(config: u8) -> usize {
    if ((config >> 1) & 1) == 1 {
        120
    } else {
        200
    }
}

/// Full **`parseDialInfo`** walk (`BaseReceiveData.parseDialInfo`) on the **payload** after the **`0xCD`** header.
pub fn parse_dial_clock_info_full(packet: &[u8]) -> Option<DialClockInfoParsed> {
    if packet.first().copied()? != 0xCD {
        return None;
    }
    if packet.len() < 8 + 6 {
        return None;
    }
    if packet.get(3).copied()? != CMD_DIAL_NOTIFY {
        return None;
    }
    if packet.get(5).copied()? != SUB_DIAL_NOTIFY_CLOCK_INFO {
        return None;
    }
    let plen = u16::from_be_bytes([packet[6], packet[7]]) as usize;
    if plen < 6 || packet.len() < 8 + plen {
        return None;
    }
    let p = &packet[8..8 + plen];
    let screen_type = p[0];
    let grade = p[1];
    let width = u16::from_be_bytes([p[2], p[3]]);
    let height = u16::from_be_bytes([p[4], p[5]]);
    let mut config = None;
    if p.len() > 6 {
        let lm = p[6] as usize;
        if p.len() > 7 + lm {
            let main_len = p[7 + lm] as usize;
            if p.len() >= 8 + lm + main_len {
                let i5 = 8 + lm + main_len;
                if p.len() > i5 {
                    config = Some(p[i5]);
                }
            }
        }
    }
    Some(DialClockInfoParsed {
        screen_type,
        grade,
        width,
        height,
        config,
    })
}

/// Some firmware ACKs dial **start** with **`0xCD`** cmd **0x15** sub **0x0c** (seen after `--preflight-upload2`), not cmd **31** status **1000**.
pub fn is_dial_start_banner_cd(packet: &[u8]) -> bool {
    packet.first().copied() == Some(0xCD)
        && packet.len() >= 8
        && packet.get(3).copied() == Some(0x15)
        && packet.get(5).copied() == Some(0x0c)
}

/// Match `WatchThemeTools.getNotHeaderBmp`: keep last `width * height * 2` bytes (RGB565 body).
pub fn strip_bmp_rgb565_tail(bmp: &[u8], width: u32, height: u32) -> anyhow::Result<Vec<u8>> {
    let need = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(2))
        .filter(|&n| n > 0)
        .ok_or_else(|| anyhow::anyhow!("invalid width/height"))?;
    if bmp.len() < need {
        anyhow::bail!("file length {} < expected RGB565 body {}", bmp.len(), need);
    }
    Ok(bmp[bmp.len() - need..].to_vec())
}

/// Solid RGB565 fill (little-endian pixels), row-major — for quick smoke tests.
pub fn solid_rgb565_buffer(width: u16, height: u16, rgb565_le: u16) -> Vec<u8> {
    let n = usize::from(width) * usize::from(height) * 2;
    let mut v = vec![0u8; n];
    let b = rgb565_le.to_le_bytes();
    for chunk in v.chunks_exact_mut(2) {
        chunk.copy_from_slice(&b);
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_frame_shape() {
        let f = dial_start(0, 0, 255, 255, 255, None);
        assert_eq!(f[0], 0xCD);
        assert_eq!(f[3], 31);
        assert_eq!(f[5], 2);
        assert_eq!(&f[8..], &[0, 0, 255, 255, 255]);
    }

    #[test]
    fn extended_start_matches_upload2_capture() {
        let mid = [0x15, 0xa2, 0x00, 0x08];
        let f = dial_start_extended(0, 0, mid, 255, 255, 255, 0x0001_fe94);
        assert_eq!(
            f,
            &[
                0xcd, 0x00, 0x16, 0x1f, 0x01, 0x02, 0x00, 0x11, 0x00, 0x00, 0x15, 0xa2, 0x00, 0x08,
                0xff, 0xff, 0xff, 0x00, 0x01, 0xfe, 0x94, 0x00, 0x00, 0x00, 0x00
            ][..]
        );
    }

    #[test]
    fn parse_mid4_hex_ok() {
        assert_eq!(parse_mid4_hex("15a20008").unwrap(), [0x15, 0xa2, 0x00, 0x08]);
    }

    #[test]
    fn start_frame_with_replace_pic_pos() {
        let f = dial_start(0, 0, 255, 255, 255, Some(2));
        assert_eq!(f[3], 31);
        assert_eq!(f[5], 2);
        assert_eq!(&f[8..], &[0, 0, 255, 255, 255, 2]);
    }

    #[test]
    fn finish_payload_eight_bytes_pl() {
        let file = [1u8, 2, 3, 10];
        let p = dial_finish_payload(&file);
        assert_eq!(p[3], 31);
        assert_eq!(p[5], 3);
        assert_eq!(&p[8..12], &(4u32.to_be_bytes()));
        assert_eq!(&p[12..16], &(16u32.to_be_bytes()));
    }

    #[test]
    fn dc_short_file34_start_sample() {
        let s = [0xDCu8, 0x00, 0x05, 0x22, 0x02, 0x00, 0x10, 0x00];
        assert_eq!(parse_dc_short(&s), Some((34, 2)));
    }

    #[test]
    fn watch_theme_fatal_status_matches_apk() {
        assert!(is_watch_theme_fatal_protocol_status(1));
        assert!(is_watch_theme_fatal_protocol_status(3));
        assert!(is_watch_theme_fatal_protocol_status(7));
        assert!(!is_watch_theme_fatal_protocol_status(2));
        assert!(!is_watch_theme_fatal_protocol_status(1000));
        assert!(
            watch_theme_protocol_error_message(3)
                .unwrap()
                .contains("battery")
        );
    }

    #[test]
    fn parse_dial_watch_ack_accepts_cmd31_sub2_start_ok() {
        // Firmware may ACK dial **start** (cmd 31 sub 2) with status **1000** in first payload u32 BE — not sub 1.
        let mut pkt = vec![0xCDu8, 0x00, 0x09, CMD_DIAL_TRANSFER, 1, SUB_DIAL_START, 0, 4];
        pkt.extend_from_slice(&1000i32.to_be_bytes());
        assert_eq!(parse_dial_watch_ack_status(&pkt), Some(1000));
    }

    #[test]
    fn parse_dial_clock_info_matches_apk_layout() {
        // Minimal cmd32/2 packet: 8-byte header + 6-byte payload (screen, grade, w=360, h=360 BE).
        // len_field = total_len - 3 = 14 - 3 = 11 → bytes [1,2] = 00 0b
        let mut pkt = vec![0xCDu8, 0x00, 0x0b, 32, 1, 2, 0, 6];
        pkt.extend_from_slice(&[0u8, 0, 0x01, 0x68, 0x01, 0x68]);
        let r = parse_dial_clock_info_cd(&pkt).expect("parse");
        assert_eq!(r, (0, 0, 360, 360));
    }

    #[test]
    fn parse_dial_clock_info_full_config_and_chunk_hint() {
        // Real-style payload: mch len 3 "K66", main len 5 "LJ733", then more fields; config at i5 = 0x07.
        // plen = 32 at [6,7]; frame = 8+32 = 40 bytes → len at [1,2] = 40−3 = 37 = 0x0025.
        let mut pkt = vec![0xCDu8, 0x00, 0x25, 32, 1, 2, 0, 32];
        pkt.extend_from_slice(&[
            1, 0, 0x01, 0x68, 0x01, 0x68, 0x03, 0x4b, 0x36, 0x36, 0x05, 0x4c, 0x4a, 0x37, 0x33,
            0x33, 0x07, 0x03, 0xff, 0xfe, 0x01, 0x05, 0x4a, 0x51, 0x30, 0x30, 0x31, 0x00, 0x00,
            0x09, 0x61, 0xa8,
        ]);
        let d = parse_dial_clock_info_full(&pkt).expect("full parse");
        assert_eq!(d.width, 360);
        assert_eq!(d.height, 360);
        assert_eq!(d.config, Some(0x07));
        assert_eq!(apk_dial_chunk_size_from_config_byte(0x07), 120);
    }

    #[test]
    fn dial_start_banner_cd_matches_upload2_preflight() {
        let pkt = [0xCDu8, 0x00, 0x1b, 0x15, 1, 0x0c, 0, 4, 0, 0, 0, 0];
        assert!(is_dial_start_banner_cd(&pkt));
    }

    #[test]
    fn decode_dc_cmd26_keys_16_and_20_sample() {
        let k16 = [0xDCu8, 0x00, 0x05, 0x1a, 0x10, 0x00, 0x08, 0x01];
        let k20 = [0xDCu8, 0x00, 0x05, 0x1a, 0x14, 0x00, 0x08, 0x01];
        assert!(decode_dc_notify_line(&k16).unwrap().contains("key 16"));
        assert!(decode_dc_notify_line(&k16).unwrap().contains("hardware"));
        assert!(decode_dc_notify_line(&k20).unwrap().contains("key 20"));
        assert!(decode_dc_notify_line(&k20).unwrap().contains("product"));
    }

    #[test]
    fn assembler_merges_split_cd() {
        // 30-byte CD packet: len_field = 27 → bytes [1,2] = 00 1b
        let mut full = vec![0xCDu8, 0x00, 0x1b];
        full.resize(30, 0); // len_field 27 → 30-byte frame
        let a = full[..20].to_vec();
        let b = full[20..].to_vec();
        let mut asm = CdNotifyAssembler::default();
        assert!(asm.push(&a).is_empty());
        let v = asm.push(&b);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].len(), 30);
    }
}
