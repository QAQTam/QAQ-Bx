#!/usr/bin/env python3
"""minidump-stackscan.py — 全内存 dump 的启发式栈扫描(无 SDK/无网络)。

对崩溃线程栈范围逐 8 字节扫描,把落在模块 ImageBase..SizeOfImage 内的值列为
「候选返回地址」。启发式而非真栈行走(不做 DWARCH/折叠表),但对定位
「哪个子系统调用了故障指令」足够——噪声帧由人工过滤。

用法:
  python minidump-stackscan.py <dmp> [--thread <tid>] [--only <substr,...>]
"""
import struct
import sys


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
        name_rva = struct.unpack_from("<I", data, off + 20)[0]
        ln = struct.unpack_from("<I", data, name_rva)[0]
        name = data[name_rva + 4:name_rva + 4 + ln].decode("utf-16-le", "replace")
        mods.append((img, size, name))
    return mods


def memory_runs(data, streams):
    """合并 MemoryList(5)+Memory64List(9)为 [(va_start, file_off, size)] 有序表。"""
    runs = []
    if 5 in streams:
        b = streams[5][0]
        cnt = struct.unpack_from("<I", data, b)[0]
        p = b + 4
        for _ in range(cnt):
            va, dsz, rva = struct.unpack_from("<QII", data, p)
            runs.append((va, rva, dsz))
            p += 16
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
    runs.sort()
    return runs


def lookup(runs, addr, need=8):
    import bisect
    starts = [r[0] for r in runs]
    i = bisect.bisect_right(starts, addr) - 1
    if i >= 0:
        va, fo, sz = runs[i]
        if va <= addr and addr + need <= va + sz:
            return fo + (addr - va)
    return None


def main():
    args = sys.argv[1:]
    tid_want, only = None, None
    all_threads = "--all-threads" in args
    if all_threads:
        args.remove("--all-threads")
    if "--thread" in args:
        i = args.index("--thread"); tid_want = int(args[i + 1]); del args[i:i + 2]
    if "--only" in args:
        i = args.index("--only"); only = [s.lower() for s in args[i + 1].split(",")]; del args[i:i + 2]
    path = args[0]
    data, streams = load(path)
    mods = modules(data, streams)
    runs = memory_runs(data, streams)

    # 异常线程 + 地址
    exc = streams[6][0] + 8
    tid = struct.unpack_from("<I", data, streams[6][0])[0]
    rip = struct.unpack_from("<Q", data, exc + 16)[0]
    if tid_want:
        tid = tid_want
    # 线程栈范围
    tb = streams[3][0]
    tcnt = struct.unpack_from("<I", data, tb)[0]
    stack_lo = stack_hi = ctx = None
    for i in range(tcnt):
        off = tb + 4 + i * 48
        if struct.unpack_from("<I", data, off)[0] == tid:
            s0, ssz = struct.unpack_from("<QI", data, off + 24)
            stack_lo, stack_hi = s0, s0 + ssz
            ctx = struct.unpack_from("<I", data, off + 44)[0]
            break
    def mod_of(a):
        for addr, size, name in mods:
            if addr <= a < addr + size:
                return name, a - addr
        return None, None

    def scan_range(lo, hi):
        found = {}
        p = lookup(runs, lo)
        va = lo
        if p is None:
            return found
        while va < hi:
            (q,) = struct.unpack_from("<Q", data, p)
            name, rva = mod_of(q)
            if name is not None:
                found.setdefault(name.lower(), []).append(rva)
            p += 8
            va += 8
        return found

    if all_threads:
        for i in range(tcnt):
            off = tb + 4 + i * 48
            tid_i = struct.unpack_from("<I", data, off)[0]
            s0, ssz = struct.unpack_from("<QI", data, off + 24)
            hi = s0 + ssz
            fr = scan_range(s0, hi)
            if only:
                fr = {k: v for k, v in fr.items() if any(o in k for o in only)}
            summary = " ".join(f"{k.rsplit(chr(92),1)[-1].lower()}({len(v)})" for k, v in sorted(fr.items()))
            star = " <== FAULT" if tid_i == tid else ""
            print(f"thread {tid_i:6d} sp=[0x{s0:x},{hi:x}] {summary}{star}")
        return
    assert stack_lo, f"thread {tid} not found"
    if rip is not None:
        print(f"[thread {tid}] fault Rip=0x{rip:x} stack=[0x{stack_lo:x}..0x{stack_hi:x}]")
    else:
        print(f"[thread {tid}] stack=[0x{stack_lo:x}..0x{stack_hi:x}]")
    for addr, size, name in mods:
        if addr <= rip < addr + size:
            print(f"[fault] {name} RVA=0x{rip - addr:x}")

# 从栈顶(SP/范围下界)向上扫——返回地址在更高地址
    seen = []
    off0 = lookup(runs, stack_lo)
    if off0 is None:
        sys.exit("FATAL: 栈范围不在 dump 内存中")
    p = off0
    va = stack_lo
    # 把 Rip 也记为 frame 0
    hits = {}
    while va < stack_hi:
        (q,) = struct.unpack_from("<Q", data, p)
        name, rva = mod_of(q)
        if name is not None:
            key = (name.lower(), None)
            hits.setdefault(name, []).append((va, q, rva))
        p += 8
        va += 8
    # 输出:按模块聚合的候选返回地址
    for name, lst in sorted(hits.items()):
        if only and not any(o in name.lower() for o in only):
            continue
        print(f"\n== {name}  ({len(lst)} 个候选返回地址) ==")
        for stackaddr, absaddr, rva in lst:
            print(f"  [sp+0x{stackaddr - stack_lo:04x}] RVA=0x{rva:x}")


if __name__ == "__main__":
    main()
