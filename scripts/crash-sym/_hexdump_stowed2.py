#!/usr/bin/env python3
"""follow SE02 record embedded pointers to find message/context blobs."""
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
count = struct.unpack_from("<Q", data, er + 48)[0]
arr = mem(p0, 8 * min(count, 64))

def showptr(tag, ptr, size=0x80):
    blk = mem(ptr, size)
    if blk is None:
        print(f"  {tag} @0x{ptr:x}: NOT IN DUMP")
        return
    print(f"  {tag} @0x{ptr:x}:")
    for j in range(0, size, 16):
        row = blk[j:j+16]
        print(f"    +{j:02x}: " + " ".join(f"{b:02x}" for b in row))

for i in range(min(count, 64)):
    ptr, = struct.unpack_from("<Q", arr, i * 8)
    blk = mem(ptr, 0x58)
    if blk is None or struct.unpack_from("<I", blk, 4)[0] != 0x53453032:
        print(f"elem[{i}] @0x{ptr:x}: bad")
        continue
    rc = struct.unpack_from("<I", blk, 8)[0]
    t2 = struct.unpack_from("<Q", blk, 0x20)[0]
    t3 = struct.unpack_from("<Q", blk, 0x30)[0]
    tag4 = blk[0x28:0x2c].decode("ascii", "replace")
    print(f"\nelem[{i}] @0x{ptr:x} rc=0x{rc:08x} tag='{tag4}'")
    showptr("p+20", t2, 0x40)
    showptr("p+30", t3, 0x40)
    if i >= 7:
        print("\n(stopped at 8)")
        break
