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

/// Status int for dial upload — **cmd 32** + sub **1** (APK `setDialUpdateInfo`). Some firmware may echo **cmd 31**.
pub fn parse_dial_watch_ack_status(packet: &[u8]) -> Option<i32> {
    let cmd = packet.get(3).copied()?;
    let sub = packet.get(5).copied()?;
    if sub != SUB_DIAL_NOTIFY_FILE {
        return None;
    }
    if cmd != CMD_DIAL_NOTIFY && cmd != CMD_DIAL_TRANSFER {
        return None;
    }
    parse_cd_notify_status(packet)
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
