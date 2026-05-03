#!/usr/bin/env python3
"""
Convert a PNG/JPEG/BMP/… image to a JieLi-style RU50 resource blob for BLE / dial testing.

Default backend: **etcpak** (pip install etcpak Pillow) — works on normal Linux/macOS/Windows
glibc builds. Output is standard ETC2 RGB8 block data in the same byte count and layout as
the vendor scratch buffer for typical sizes (see scratch_bytes()).

Optional **native** backend loads jni/x86_64/libjl_bmp_convert.so and calls ETC2CompressRawData.
That library is built for Android (Bionic); on desktop Linux, dlopen usually fails — use etcpak.

CRC16 matches the vendor routine using the 512-byte table read from the .so file at offset
0x9460 (no need to execute the .so for CRC).

See ../decompile/ENCODER_SPEC.md for format notes.
"""

from __future__ import annotations

import argparse
import ctypes
import struct
import sys
from pathlib import Path

from PIL import Image

HEADER_RESERVED_OFF = 0x14
HEADER_RESERVED_LEN = 0x400
PAYLOAD_OFF = 0x450
MAGIC_RU50 = 0x30355552
HDR_QW_04 = 0x0000000100050100
HDR_QW_18 = 0x54000100000030
HDR_QW_20 = 0x3C00000000
HDR_QW_28 = 0x500001
HDR_QW_30 = 0x5000000100
HDR_DW_38 = 0x400
HDR_FLAGS = 0x00920001


def _script_dir() -> Path:
    return Path(__file__).resolve().parent


def _default_so() -> Path:
    return _script_dir().parent / "jni" / "x86_64" / "libjl_bmp_convert.so"


def _crc_table_from_so(so_path: Path) -> bytes:
    """512-byte CRC table at file offset 0x9460 (x86_64 BmpConvert 1.6.0)."""
    with so_path.open("rb") as f:
        f.seek(0x9460)
        t = f.read(512)
    if len(t) != 512:
        raise OSError(f"Could not read 512-byte CRC table from {so_path}")
    return t


def crc16_sw(data: bytes, table: bytes) -> int:
    tbl = struct.unpack("<256H", table)
    crc = 0
    for byte in data:
        for nibble in ((byte >> 4) & 0x0F, byte & 0x0F):
            idx = ((crc >> 12) ^ nibble) & 0x0F
            crc = ((crc << 4) & 0xFFFF) ^ tbl[idx]
    return crc & 0xFFFF


def scratch_bytes(width: int, height: int) -> int:
    return ((width * 2 + 6) & ~7) * ((height + 3) // 4)


def rgb888_row_major(im: Image.Image) -> bytes:
    """RGB888 row-major (R,G,B per pixel) as used by the vendor RGB path."""
    rgb = im.convert("RGB")
    return rgb.tobytes("raw", "RGB")


def bgra_for_etcpak(im: Image.Image) -> bytes:
    """etcpak ETC paths expect BGRA bytes (see K0lb3/etcpak README)."""
    rgb = im.convert("RGB")
    r, g, b = rgb.split()
    a = Image.new("L", rgb.size, 255)
    return Image.merge("RGBA", (b, g, r, a)).tobytes("raw", "RGBA")


def compress_etcpak(im: Image.Image, width: int, height: int) -> bytes:
    import etcpak  # type: ignore

    if im.width != width or im.height != height:
        im = im.resize((width, height), Image.Resampling.LANCZOS)
    raw = bgra_for_etcpak(im)
    payload = etcpak.compress_etc2_rgb(raw, width, height)
    nexpect = scratch_bytes(width, height)
    if len(payload) != nexpect:
        raise RuntimeError(
            f"ETC2 payload length mismatch: got {len(payload)}, expected {nexpect} "
            f"for {width}x{height}"
        )
    return payload


def compress_native(so_path: Path, rgb888: bytes, width: int, height: int) -> bytes:
    lib = ctypes.CDLL(str(so_path))
    fn = lib.ETC2CompressRawData
    fn.argtypes = [
        ctypes.c_int,
        ctypes.c_int,
        ctypes.c_void_p,
        ctypes.c_int,
        ctypes.c_int,
        ctypes.c_int,
        ctypes.c_int,
        ctypes.c_int,
        ctypes.c_int,
        ctypes.c_void_p,
    ]
    fn.restype = None
    nscratch = scratch_bytes(width, height)
    scratch = bytearray(nscratch)
    rgb_buf = (ctypes.c_ubyte * len(rgb888)).from_buffer_copy(rgb888)
    out_buf = (ctypes.c_ubyte * len(scratch)).from_buffer(scratch)
    fn(
        0,
        0,
        ctypes.cast(rgb_buf, ctypes.c_void_p),
        0,
        int(width),
        int(height),
        1,
        1,
        0,
        1,
        ctypes.cast(out_buf, ctypes.c_void_p),
    )
    return bytes(scratch)


def build_ru50_blob(
    width: int,
    height: int,
    payload: bytes,
    crc_payload: int,
    crc_hdr: int,
) -> bytes:
    total = PAYLOAD_OFF + len(payload)
    buf = bytearray(total)
    struct.pack_into("<I", buf, 0, MAGIC_RU50)
    struct.pack_into("<Q", buf, 4, HDR_QW_04)
    struct.pack_into("<I", buf, 12, 0)
    struct.pack_into("<Q", buf, 16, 0x1800000000)
    struct.pack_into("<Q", buf, 24, HDR_QW_18)
    struct.pack_into("<Q", buf, 32, HDR_QW_20)
    struct.pack_into("<Q", buf, 40, HDR_QW_28)
    struct.pack_into("<Q", buf, 48, HDR_QW_30)
    struct.pack_into("<I", buf, 56, HDR_DW_38)
    struct.pack_into("<I", buf, 0x4C, len(payload))
    struct.pack_into("<HH", buf, 0x44, width & 0xFFFF, height & 0xFFFF)
    w0 = HDR_FLAGS & 0xFFFFFFFF
    w1 = ((crc_hdr & 0xFFFF) << 16) | (crc_payload & 0xFFFF)
    struct.pack_into("<II", buf, 0x3C, w0, w1)
    buf[HEADER_RESERVED_OFF : HEADER_RESERVED_OFF + HEADER_RESERVED_LEN] = (
        b"\x00" * HEADER_RESERVED_LEN
    )
    buf[PAYLOAD_OFF : PAYLOAD_OFF + len(payload)] = payload
    return bytes(buf)


def hdr_crc_slice(width: int, height: int, payload_len: int, crc_p: int) -> bytes:
    """18 bytes fed to the second Crc16 (crc_h computed with last uint16 zero)."""
    hdr_slice = bytearray(0x12)
    struct.pack_into("<I", hdr_slice, 0, HDR_FLAGS)
    struct.pack_into("<HH", hdr_slice, 4, width & 0xFFFF, height & 0xFFFF)
    struct.pack_into("<I", hdr_slice, 8, payload_len)
    struct.pack_into("<HH", hdr_slice, 12, crc_p & 0xFFFF, 0)
    return bytes(hdr_slice)


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("input", type=Path, help="Input image path")
    p.add_argument("-o", "--output", type=Path, required=True, help="Output .bin path")
    p.add_argument("--width", type=int, help="Force width (default: image width)")
    p.add_argument("--height", type=int, help="Force height (default: image height)")
    p.add_argument(
        "--backend",
        choices=("etcpak", "native"),
        default="etcpak",
        help="etcpak (default, portable) or native Android .so",
    )
    p.add_argument(
        "--so",
        type=Path,
        default=_default_so(),
        help="Path to libjl_bmp_convert.so (for CRC table; native backend loads it)",
    )
    args = p.parse_args()

    if sys.maxsize <= 2**32:
        p.error("This tool requires 64-bit Python.")

    if not args.so.is_file():
        p.error(
            f"Missing vendor .so (needed for CRC table): {args.so}\n"
            "Place BmpConvert jni/x86_64 under third_party/bmpconvert_extract/."
        )

    table = _crc_table_from_so(args.so)
    im = Image.open(args.input)
    w = args.width or im.width
    h = args.height or im.height

    if args.backend == "etcpak":
        payload = compress_etcpak(im, w, h)
    else:
        if w != im.width or h != im.height:
            im = im.resize((w, h), Image.Resampling.LANCZOS)
        rgb888 = rgb888_row_major(im)
        try:
            payload = compress_native(args.so, rgb888, w, h)
        except OSError as e:
            p.error(
                f"Native backend failed to load or run vendor library ({e}).\n"
                "Use --backend etcpak (default) on desktop Linux; "
                "install: pip install -r third_party/bmpconvert_extract/tools/requirements-ru50.txt"
            )

    crc_p = crc16_sw(payload, table)
    crc_h = crc16_sw(hdr_crc_slice(w, h, len(payload), crc_p), table)
    blob = build_ru50_blob(w, h, payload, crc_p, crc_h)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_bytes(blob)
    print(
        f"Wrote {args.output} ({len(blob)} bytes) {w}x{h} "
        f"payload={len(payload)} backend={args.backend}"
    )


if __name__ == "__main__":
    main()
