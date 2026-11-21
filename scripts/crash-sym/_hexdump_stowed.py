#!/usr/bin/env python3
"""hexdump stowed exception raw bytes to identify record layout."""
import struct, sys, bisect

path = sys.argv[1]
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
starts = [r[0] for r in runs]

def lookup(addr, need):
    i = bisect.bisect_right(starts, addr) - 1
    if i >= 0:
        va, fo, sz = runs[i]
        if va <= addr and addr + need <= va + sz:
            return fo + (addr - va)
    return None

def mem(addr, size):
    off = lookup(addr, size)
    return data[off:off+size] if off is not None else None

er = streams[6][0]
p0 = struct.unpack_from("<Q", data, er + 40)[0]
p1 = struct.unpack_from("<Q", data, er + 48)[0]
print(f"param0=0x{p0:x} param1=0x{p1:x}")

hdr = mem(p0, 0x100)
print(f"\n== array @0x{p0:x} (first 0x80 bytes) ==")
for i in range(0, 0x80, 16):
    row = hdr[i:i+16]
    print(f"  +{i:02x}: " + " ".join(f"{b:02x}" for b in row))

# dump raw bytes at each of first 6 element pointers
for i in range(6):
    ptr, = struct.unpack_from("<Q", hdr, i*8)
    blk = mem(ptr, 0x80)
    if blk is None:
        print(f"\n== elem[{i}] @0x{ptr:x}: NOT IN DUMP ==")
        continue
    print(f"\n== elem[{i}] @0x{ptr:x} ==")
    for j in range(0, 0x60, 16):
        row = blk[j:j+16]
        q1, q2 = struct.unpack_from("<QQ", blk, j)
        print(f"  +{j:02x}: " + " ".join(f"{b:02x}" for b in row) + f"   q={q1:#x} {q2:#x}")
