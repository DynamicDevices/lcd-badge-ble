//! JPEG upload profile captured on BG02 / LJ733_MB_V1.5 / V35922 (issue #2).

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use bluer::{
    gatt::{
        remote::{Characteristic, CharacteristicWriteRequest},
        WriteOp,
    },
    Adapter,
};
use futures::{pin_mut, Stream, StreamExt};

use crate::dial_upload::{
    dial_finish_payload, dial_start_extended, is_watch_theme_fatal_protocol_status,
    parse_dial_watch_ack_status, watch_theme_protocol_error_message, CdNotifyAssembler,
    CMD_DIAL_TRANSFER, SUB_DIAL_FILE,
};
use crate::{
    connect_ble_device, device_disconnect_best_effort, find_characteristic, get_protocol, hex,
    wait_gatt_ready_for_upload, BLE_CONNECT_TIMEOUT, DEFAULT_NOTIFY_UUID, DEFAULT_WRITE_UUID,
    GATT_WRITE_TIMEOUT, NOTIFY_ENABLE_TIMEOUT,
};

const CHUNK_SIZE: usize = 5000;
const GATT_FRAGMENT_SIZE: usize = 20;
const GATT_FRAGMENT_GAP_MS: u64 = 3;
const STATUS_TIMEOUT: Duration = Duration::from_secs(15);

pub async fn upload(
    adapter: &Adapter,
    addr: &str,
    path: &Path,
    write_with_response: bool,
    verbose: bool,
) -> Result<()> {
    let jpeg = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let payload = jpeg_payload(&jpeg)?;
    let device =
        connect_ble_device(adapter, addr, false, 10, false, BLE_CONNECT_TIMEOUT, None).await?;

    let result = async {
        wait_gatt_ready_for_upload(&device).await;
        let tx = find_characteristic(&device, DEFAULT_WRITE_UUID.parse()?)
            .await
            .context("resolve BG02 write characteristic")?;
        let rx = find_characteristic(&device, DEFAULT_NOTIFY_UUID.parse()?)
            .await
            .context("resolve BG02 notify characteristic")?;
        let writer = UploadWriter {
            tx: &tx,
            op: if write_with_response {
                WriteOp::Request
            } else {
                WriteOp::Command
            },
            verbose,
        };
        writer.send_payload(&rx, &payload).await
    }
    .await;

    device_disconnect_best_effort(&device).await;
    result
}

fn jpeg_payload(jpeg: &[u8]) -> Result<Vec<u8>> {
    if jpeg.len() > usize::from(u16::MAX) * CHUNK_SIZE - 4 {
        bail!("JPEG is too large for the BG02 transfer sequence");
    }
    let mut decoder = jpeg_decoder::Decoder::new(jpeg);
    decoder.read_info().context("read JPEG header")?;
    let info = decoder.info().context("JPEG dimensions are missing")?;
    if (info.width, info.height) != (240, 240) {
        bail!(
            "BG02 requires a 240x240 JPEG (got {}x{})",
            info.width,
            info.height
        );
    }
    decoder.decode().context("decode JPEG")?;
    let mut payload = Vec::with_capacity(4 + jpeg.len());
    payload.extend_from_slice(&(jpeg.len() as u32).to_be_bytes());
    payload.extend_from_slice(jpeg);
    Ok(payload)
}

fn start_frame(len: u32) -> Vec<u8> {
    dial_start_extended(0, 0, [0x15, 0xa2, 0x02, 0x08], 0, 0, 0, len)
}

fn chunk_frame(seq: u16, chunk: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(2 + chunk.len() + 4);
    body.extend_from_slice(&seq.to_be_bytes());
    body.extend_from_slice(chunk);
    let sum: u32 = body.iter().map(|&byte| u32::from(byte)).sum();
    body.extend_from_slice(&sum.to_be_bytes());
    get_protocol(CMD_DIAL_TRANSFER, SUB_DIAL_FILE, &body)
}

fn notify_ack(packet: &[u8]) -> Option<[u8; 8]> {
    let (&cmd, &sub) = (packet.get(3)?, packet.get(5)?);
    let [high, low] = u16::try_from(packet.len()).ok()?.to_be_bytes();
    Some([0xdc, 0, 5, cmd, sub, high, low, 1])
}

struct UploadWriter<'a> {
    tx: &'a Characteristic,
    op: WriteOp,
    verbose: bool,
}

impl UploadWriter<'_> {
    async fn write(&self, frame: &[u8]) -> Result<()> {
        let request = CharacteristicWriteRequest {
            op_type: self.op,
            ..Default::default()
        };
        tokio::time::timeout(GATT_WRITE_TIMEOUT, async {
            for (index, fragment) in frame.chunks(GATT_FRAGMENT_SIZE).enumerate() {
                if index > 0 {
                    tokio::time::sleep(Duration::from_millis(GATT_FRAGMENT_GAP_MS)).await;
                }
                self.tx
                    .write_ext(fragment, &request)
                    .await
                    .with_context(|| format!("write BG02 GATT fragment {}", index + 1))?;
            }
            Ok(())
        })
        .await
        .context("BG02 GATT write timed out")?
    }

    async fn send_payload(&self, rx: &Characteristic, payload: &[u8]) -> Result<()> {
        let stream = tokio::time::timeout(NOTIFY_ENABLE_TIMEOUT, rx.notify())
            .await
            .context("BG02 notify subscribe timed out")?
            .context("subscribe to BG02 notifications")?;
        pin_mut!(stream);
        let mut assembler = CdNotifyAssembler::default();
        tokio::time::sleep(Duration::from_millis(250)).await;

        let chunk_count = payload.len().div_ceil(CHUNK_SIZE);
        println!(
            "BG02 upload: {} bytes in {chunk_count} chunk(s), ATT {}",
            payload.len(),
            self.op
        );
        self.write(&start_frame(payload.len() as u32))
            .await
            .context("send start")?;
        self.wait_status(&mut stream, &mut assembler, 1000).await?;
        for (index, chunk) in payload.chunks(CHUNK_SIZE).enumerate() {
            let seq = (index + 1) as u16;
            self.write(&chunk_frame(seq, chunk))
                .await
                .with_context(|| format!("send chunk {seq}/{chunk_count}"))?;
            self.wait_status(&mut stream, &mut assembler, 1000 + i32::from(seq))
                .await?;
            println!("BG02 acknowledged chunk {seq}/{chunk_count}");
        }
        self.write(&dial_finish_payload(payload))
            .await
            .context("send finish")?;
        self.wait_status(&mut stream, &mut assembler, 2).await?;
        println!("BG02 reported upload success (status 2).");
        Ok(())
    }

    async fn wait_status(
        &self,
        stream: &mut (impl Stream<Item = Vec<u8>> + Unpin),
        assembler: &mut CdNotifyAssembler,
        expected: i32,
    ) -> Result<()> {
        tokio::time::timeout(STATUS_TIMEOUT, async {
            while let Some(data) = stream.next().await {
                if self.verbose {
                    eprintln!("BG02 notify: {}", hex(&data[..data.len().min(64)]));
                }
                let mut matched = false;
                for packet in assembler.push(&data) {
                    if let Some(ack) = notify_ack(&packet) {
                        self.write(&ack)
                            .await
                            .context("acknowledge BG02 notification")?;
                    }
                    if let Some(status) = parse_dial_watch_ack_status(&packet) {
                        if self.verbose {
                            eprintln!("BG02 status {status} (waiting for {expected})");
                        }
                        if is_watch_theme_fatal_protocol_status(status) {
                            let detail = watch_theme_protocol_error_message(status)
                                .unwrap_or("upload failed");
                            bail!("BG02 reported status {status}: {detail}");
                        }
                        matched |= status == expected;
                    }
                }
                if matched {
                    return Ok(());
                }
            }
            bail!("BG02 notification stream ended while waiting for status {expected}");
        })
        .await
        .with_context(|| format!("timed out waiting for BG02 status {expected}"))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_and_notification_ack_match_capture() {
        assert_eq!(
            start_frame(11_189),
            [
                0xcd, 0, 0x16, 0x1f, 1, 2, 0, 0x11, 0, 0, 0x15, 0xa2, 2, 8, 0, 0, 0, 0, 0, 0x2b,
                0xb5, 0, 0, 0, 0,
            ]
        );
        let success = [0xcd, 0, 9, 0x20, 1, 1, 0, 4, 0, 0, 0, 2];
        assert_eq!(notify_ack(&success), Some([0xdc, 0, 5, 0x20, 1, 0, 12, 1]));
        assert_eq!(parse_dial_watch_ack_status(&success), Some(2));
        let chunk_ack = [0xcd, 0, 9, 0x1f, 1, 1, 0, 4, 0, 0, 3, 0xe9];
        assert_eq!(parse_dial_watch_ack_status(&chunk_ack), Some(1001));
    }

    #[test]
    fn chunk_checksum_keeps_upper_16_bits() {
        let frame = chunk_frame(0x0102, &[0xff; CHUNK_SIZE]);
        assert_eq!(
            &frame[..10],
            &[0xcd, 0x13, 0x93, 0x1f, 1, 1, 0x13, 0x8e, 1, 2]
        );
        assert_eq!(&frame[frame.len() - 4..], &[0x00, 0x13, 0x74, 0x7b]);
    }
}
