#!/usr/bin/env python3
"""Turn mic_sdus.bin from mic-test.py into a Matroska file ffmpeg can decode.

Record format: <f64 t><u32 len><sdu>. SDU layout (LibrePods PR #655 aacp_audio.rs):
22-byte header, then N x [ts:u32 LE][len:u8][AAC-ELD access unit].
"""
import struct
import sys

ASC = bytes([0xF8, 0xE6, 0x30, 0x00])
FRAME_MS = 7.5


def ebml_id(i):
    return i.to_bytes((i.bit_length() + 7) // 8, "big")


def ebml_size(n):
    for length in range(1, 9):
        if n < (1 << (7 * length)) - 1:
            return ((1 << (7 * length)) | n).to_bytes(length, "big") if length > 1 else bytes([0x80 | n])
    raise ValueError


def el(i, payload):
    return ebml_id(i) + ebml_size(len(payload)) + payload


def uint(i, v):
    return el(i, v.to_bytes(max(1, (v.bit_length() + 7) // 8), "big"))


def read_aus(path):
    data = open(path, "rb").read()
    off, aus, sdus, bad = 0, [], 0, 0
    while off + 12 <= len(data):
        t, n = struct.unpack_from("<dI", data, off)
        sdu = data[off + 12: off + 12 + n]
        off += 12 + n
        sdus += 1
        p = 22
        while p + 5 <= len(sdu):
            ln = sdu[p + 4]
            if p + 5 + ln > len(sdu):
                bad += 1
                break
            aus.append(sdu[p + 5: p + 5 + ln])
            p += 5 + ln
    return aus, sdus, bad


def write_mkv(aus, out, rate):
    header = el(0x1A45DFA3, uint(0x4286, 1) + uint(0x42F7, 1) + uint(0x42F2, 4) + uint(0x42F3, 8)
                + el(0x4282, b"matroska") + uint(0x4287, 4) + uint(0x4285, 2))
    info = el(0x1549A966, uint(0x2AD7B1, 1000000) + el(0x4D80, b"mic-test") + el(0x5741, b"mic-test"))
    audio = el(0xE1, el(0xB5, struct.pack(">d", rate)) + uint(0x9F, 1))
    track = el(0xAE, uint(0xD7, 1) + uint(0x73C5, 1) + uint(0x83, 2) + el(0x86, b"A_AAC")
               + el(0x63A2, ASC) + audio)
    tracks = el(0x1654AE6B, track)
    clusters = b""
    i = 0
    while i < len(aus):
        base = int(i * FRAME_MS)
        blocks = b""
        while i < len(aus) and int(i * FRAME_MS) - base < 30000:
            rel = int(i * FRAME_MS) - base
            blocks += el(0xA3, b"\x81" + struct.pack(">h", rel) + b"\x80" + aus[i])
            i += 1
        clusters += el(0x1F43B675, uint(0xE7, base) + blocks)
    segment = el(0x18538067, info + tracks + clusters)
    open(out, "wb").write(header + segment)


if __name__ == "__main__":
    aus, sdus, bad = read_aus(sys.argv[1])
    print(f"sdus={sdus} access_units={len(aus)} truncated_sdus={bad} seconds~{len(aus) * FRAME_MS / 1000:.1f}")
    write_mkv(aus, sys.argv[2], float(sys.argv[3]) if len(sys.argv) > 3 else 48000.0)
