#!/usr/bin/env python3
"""minidump-stowed.py — 解析 0xC000027B(stowed exception)full dump,无 SDK/无网络。

Win11 24H2+ 新记录格式(实测本 dump,签名在 +4):
  +00 ULONG Size
  +04 ULONG Sig 'SE02'
  +08 HRESULT ResultCode
  +0C ULONG ThreadId
  +20 PVOID FrameArray(完整 VA 的返回地址数组)
  +28 CHAR[4] OriginTag('XAML'/'XAMW'…)
  +30 PVOID StackContext{hash,tid,count,frames[]}

用法:
  python minidump-stowed.py <dmp> [--all] [--cv <模块名子串>] [--mod <模块名子串>]
  --all  打印全部记录完整帧;默认打印前 8 条 + 按模块聚合
  --cv   打印匹配模块的 PDB CV(GUID+age)
  --mod  只打印命中模块的帧
"""
import struct
import sys
import bisect
from collections import Counter

SE02 = 0x53453032  # 'SE02'


def load(path):
    data = open(path, "rb").read()
    assert data[:4] == b"MDMP", "not a minidump"
    n = struct.unpack_from("<I", data, 8)[0]
    dr = struct.unpack_from("<I", data, 12)[0]
    streams = {}
    for i in range(n):
        typ, size, rva = struct.unpack_from("<III", data, dr + i * 12)
        streams[typ] = (rva, size)
    return data, streams


def modules(data, streams):
    base = streams[4][0]
    cnt = struct.unpack_from("<I", data, base)[0]
    mods = []
    for i in range(cnt):
        off = base + 4 + i * 108
        img, size = struct.unpack_from("<QI", data, off)
        ts = struct.unpack_from("<I", data, off + 16)[0]
        name_rva = struct.unpack_from("<I", data, off + 20)[0]
        cv_size, cv_rva = struct.unpack_from("<II", data, off + 76)
        ln = struct.unpack_from("<I", data, name_rva)[0]
        name = data[name_rva + 4:name_rva + 4 + ln].decode("utf-16-le", "replace")
        cv = None
        if cv_size >= 24 and data[cv_rva:cv_rva + 4] == b"RSDS":
            guid = data[cv_rva + 4:cv_rva + 20]
            age = struct.unpack_from("<I", data, cv_rva + 20)[0]
            pth = data[cv_rva + 24:cv_rva + cv_size].split(b"\x00")[0].decode("utf-8", "replace")
            cv = (guid, age, pth)
        mods.append((img, size, name, ts, cv))
    mods.sort()
    return mods


def memory_runs(data, streams):
    runs = []
    if 9 in streams:
        b = streams[9][0]
        cnt, base_off = struct.unpack_from("<QQ", data, b)
        p = b + 16
        descs = []
        for _ in range(cnt):
            va, dsz = struct.unpack_from("<QQ", data, p)
            descs.append((va, dsz))
            p += 16
        cur = base_off
        for va, dsz in descs:
            runs.append((va, cur, dsz))
            cur += dsz
    if 5 in streams:
        b = streams[5][0]
        cnt = struct.unpack_from("<I", data, b)[0]
        p = b + 4
        for _ in range(cnt):
            va, dsz, rva = struct.unpack_from("<QII", data, p)
            runs.append((va, rva, dsz))
            p += 16
    runs.sort()
    return runs


def lookup(runs, addr, need=8):
    starts = [r[0] for r in runs]
    i = bisect.bisect_right(starts, addr) - 1
    if i >= 0:
        va, fo, sz = runs[i]
        if va <= addr and addr + need <= va + sz:
            return fo + (addr - va)
    return None


def read_mem(data, runs, addr, size):
    off = lookup(runs, addr, size)
    if off is None:
        return None
    return data[off:off + size]


class ModResolver:
    def __init__(self, mods):
        self.mods = mods
        self.starts = [m[0] for m in mods]

    def of(self, a):
        i = bisect.bisect_right(self.starts, a) - 1
        if i >= 0:
            img, size, name, ts, cv = self.mods[i]
            if img <= a < img + size:
                short = name.rsplit("\\", 1)[-1]
                return short, a - img
        return None, None


def main():
    args = sys.argv[1:]
    show_all = "--all" in args
    if show_all:
        args.remove("--all")
    cv_want = None
    if "--cv" in args:
        i = args.index("--cv"); cv_want = args[i + 1].lower(); del args[i:i + 2]
    mod_want = None
    if "--mod" in args:
        i = args.index("--mod"); mod_want = args[i + 1].lower(); del args[i:i + 2]
    path = args[0]
    data, streams = load(path)
    mods = modules(data, streams)
    runs = memory_runs(data, streams)
    res = ModResolver(mods)

    if cv_want:
        for img, size, name, ts, cv in mods:
            if cv_want in name.lower() and cv:
                guid, age, pth = cv
                g = "{%08X-%04X-%04X-%s-%s}" % (
                    struct.unpack_from("<I", guid, 0)[0],
                    struct.unpack_from("<H", guid, 4)[0],
                    struct.unpack_from("<H", guid, 6)[0],
                    guid[8:10].hex().upper(), guid[10:16].hex().upper())
                print(f"[cv] {name}\n     age={age} guid={g} pdb={pth}")
                return
        sys.exit(f"no module matching {cv_want} with CV record")

    er = streams[6][0]
    tid = struct.unpack_from("<I", data, er)[0]
    code = struct.unpack_from("<I", data, er + 8)[0]
    addr = struct.unpack_from("<Q", data, er + 24)[0]
    nparams = struct.unpack_from("<I", data, er + 32)[0]
    params = [struct.unpack_from("<Q", data, er + 40 + 8 * k)[0] for k in range(nparams)]
    fmod, frva = res.of(addr)
    print(f"[exception] thread={tid} code=0x{code:08x} addr=0x{addr:x}"
          + (f" ({fmod}+0x{frva:x})" if fmod else ""))
    print(f"[params] " + " ".join(f"0x{p:x}" for p in params))
    if code != 0xC000027B or len(params) < 2:
        print("not a stowed-exception crash")
        return
    arr_ptr, count = params[0], params[1]
    arr = read_mem(data, runs, arr_ptr, 8 * min(count, 64))
    if arr is None:
        sys.exit(f"FATAL: stowed array @0x{arr_ptr:x} 不在 dump 内存中")
    print(f"[stowed] count={count}")

    agg = Counter()
    shown = 0
    for i in range(min(count, 64)):
        ptr, = struct.unpack_from("<Q", arr, i * 8)
        blk = read_mem(data, runs, ptr, 0x58)
        if blk is None or struct.unpack_from("<I", blk, 4)[0] != SE02:
            print(f"elem[{i}] @0x{ptr:x}: unreadable/bad")
            continue
        size = struct.unpack_from("<I", blk, 0)[0]
        rc = struct.unpack_from("<I", blk, 8)[0]
        stid = struct.unpack_from("<I", blk, 0xC)[0]
        fa = struct.unpack_from("<Q", blk, 0x20)[0]
        tag = blk[0x28:0x2C].decode("ascii", "replace")
        sc = struct.unpack_from("<Q", blk, 0x30)[0]
        cnt2 = 0
        if sc:
            hdr2 = read_mem(data, runs, sc, 0x20)
            if hdr2:
                cnt2 = struct.unpack_from("<I", hdr2, 0xC)[0]
        print(f"\n=== stowed[{i}] @0x{ptr:x} size={size} rc=0x{rc:08x} tid={stid} origin='{tag}' frames={cnt2}")
        if not show_all and shown >= 7:
            continue
        shown += 1
        farr = read_mem(data, runs, fa, 8 * cnt2) if fa and cnt2 else None
        if farr is None:
            print("  frames: NOT IN DUMP")
            continue
        for k in range(cnt2):
            f, = struct.unpack_from("<Q", farr, k * 8)
            m, rva = res.of(f)
            agg[m] += 1
            if m and (not mod_want or mod_want in m.lower()):
                print(f"  [{k:02d}] {m}+0x{rva:x}")
            elif not m:
                print(f"  [{k:02d}] ??? 0x{f:x}")
    print(f"\n[summary] 帧模块分布: " + ", ".join(f"{m or '?'}({n})" for m, n in agg.most_common(12)))


if __name__ == "__main__":
    main()
