#!/usr/bin/env python3
"""heap-scan-strings.py — 在 full dump 的内存区里扫 UTF-16LE 字符串(布局循环警告上下文)。
用法: python heap-scan-strings.py <dmp> <substr1,substr2,...> [--ctx N]
"""
import struct, sys, bisect

path = sys.argv[1]
pats = [s.encode("utf-16-le") for s in sys.argv[2].split(",")]
ctx = 0
if "--ctx" in sys.argv:
    i = sys.argv.index("--ctx"); ctx = int(sys.argv[i+1])

data = open(path, "rb").read()
n = struct.unpack_from("<I", data, 8)[0]
dr = struct.unpack_from("<I", data, 12)[0]
streams = {}
for i in range(n):
    typ, size, rva = struct.unpack_from("<III", data, dr + i * 12)
    streams[typ] = (rva, size)

runs = []
b = streams[9][0]
cnt, base_off = struct.unpack_from("<QQ", data, b)
p = b + 16
descs = []
for _ in range(cnt):
    va, dsz = struct.unpack_from("<QQ", data, p)
    descs.append((va, dsz)); p += 16
cur = base_off
for va, dsz in descs:
    runs.append((va, cur, dsz)); cur += dsz
runs.sort()

def utf16(data, off, maxch=120):
    out = []
    for k in range(maxch):
        c = struct.unpack_from("<H", data, off + 2*k)[0]
        if c == 0:
            break
        if 0x20 <= c < 0xfffe:
            out.append(chr(c))
        else:
            break
    return "".join(out)

CHUNK = 8 << 20
for va, off, sz in runs:
    pos = 0
    while pos < sz:
        end = min(sz, pos + CHUNK + 64)
        blob = data[off+pos: off+end]
        for pi, pat in enumerate(pats):
            start = 0
            while True:
                i = blob.find(pat, start)
                if i < 0:
                    break
                # 字符串可能跨块边界,回退少量
                s = utf16(data, off + pos + i, 160)
                if s:
                    print(f"[0x{va + pos + i:x}] {s}")
                start = i + 2
        pos += CHUNK
