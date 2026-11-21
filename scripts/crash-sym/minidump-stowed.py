#!/usr/bin/env python3
"""minidump-stowed.py — 解析 0xC000027B(stowed exception)full dump,无 SDK/无网络。

0xC000027B 的 ExceptionInformation:
  [0] = STOWED_EXCEPTION_INFORMATION* 数组指针
  [1] = 元素个数
每条记录(V1 form=0 / V2 form=1)携带原始 HRESULT 与可选栈回溯
(ULONG32 帧,为模块内 RVA;按模块 ImageBase 排序归属)。

用法:
  python minidump-stowed.py <dmp> [--all] [--cv <模块名子串>]
  --all    打印全部 stowed 记录(默认只打印前 5 条 + 汇总)
  --cv     额外打印匹配模块的 PDB CV 记录(GUID+age,供 msdl 下载)
"""
import struct
import sys
import bisect


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


def mod_of(mods, a):
    starts = [m[0] for m in mods]
    i = bisect.bisect_right(starts, a) - 1
    if i >= 0:
        img, size, name, ts, cv = mods[i]
        if img <= a < img + size:
            return name, a - img
    return None, None


def parse_record(data, runs, mods, ptr):
    hdr = read_mem(data, runs, ptr, 0x2000)
    if hdr is None:
        return None
    sig, size = struct.unpack_from("<II", hdr, 0)
    if sig != 0x53455347:  # 'SESG'
        return {"bad_sig": hex(sig)}
    result_code = struct.unpack_from("<i", hdr, 8)[0]
    form = struct.unpack_from("<I", hdr, 12)[0]
    rec = {"sig": "SESG", "size": size, "result": result_code, "form": form}
    if form == 0:
        rec["address"] = struct.unpack_from("<Q", hdr, 16)[0]
    else:
        tid, flags = struct.unpack_from("<II", hdr, 16)
        rec["thread"] = tid
        rec["flags"] = flags
    nested_type, = struct.unpack_from("<I", hdr, 24)
    nested_ptr, = struct.unpack_from("<Q", hdr, 32)
    rec["nested_type"] = nested_type
    rec["nested_ptr"] = nested_ptr
    # V2(form=1) 栈回溯:ULONG BackTraceSize + ULONG32 frames
    if form == 1:
        bt_size, = struct.unpack_from("<I", hdr, 40)
        bt_size = min(bt_size, 32)
        frames = []
        for k in range(bt_size):
            f, = struct.unpack_from("<I", hdr, 44 + k * 4)
            frames.append(f)
        rec["frames"] = frames
    # nested PSK1 = 打包的 EXCEPTION_RECORD64
    if nested_ptr:
        nr = read_mem(data, runs, nested_ptr, 0x60)
        if nr:
            code, flags2 = struct.unpack_from("<II", nr, 0)
            rec["nested_code"] = code
            rec["nested_flags"] = flags2
            addr64, = struct.unpack_from("<Q", nr, 16)
            rec["nested_addr"] = addr64
            nparams, = struct.unpack_from("<II", nr, 24)
            rec["nested_params"] = [struct.unpack_from("<Q", nr, 32 + 8 * k)[0] for k in range(min(nparams, 2))]
    return rec


def main():
    args = sys.argv[1:]
    show_all = "--all" in args
    if show_all:
        args.remove("--all")
    cv_want = None
    if "--cv" in args:
        i = args.index("--cv"); cv_want = args[i + 1].lower(); del args[i:i + 2]
    path = args[0]
    data, streams = load(path)
    mods = modules(data, streams)
    runs = memory_runs(data, streams)

    if cv_want:
        for img, size, name, ts, cv in mods:
            if cv_want in name.lower() and cv:
                guid, age, pth = cv
                g = "{%08X-%04X-%04X-%s-%s}" % (
                    struct.unpack_from("<I", guid, 0)[0],
                    struct.unpack_from("<H", guid, 4)[0],
                    struct.unpack_from("<H", guid, 6)[0],
                    guid[8:10].hex().upper(), guid[10:16].hex().upper())
                print(f"[cv] {name} age={age} guid={g} pdb={pth}")
                return

    # 异常流
    er, esz = streams[6]
    tid = struct.unpack_from("<I", data, er)[0]
    code = struct.unpack_from("<I", data, er + 8)[0]
    addr = struct.unpack_from("<Q", data, er + 24)[0]
    nparams = struct.unpack_from("<I", data, er + 32)[0]
    params = [struct.unpack_from("<Q", data, er + 40 + 8 * k)[0] for k in range(nparams)]
    fmod, frva = mod_of(mods, addr)
    print(f"[exception] thread={tid} code=0x{code:08x} addr=0x{addr:x}")
    print(f"[fault-module] {fmod} RVA=0x{frva:x}" if fmod else f"[fault-module] ?")
    print(f"[params] {len(params)}: " + " ".join(f"0x{p:x}" for p in params))
    if code != 0xC000027B or len(params) < 2:
        print("not a stowed-exception crash (or params missing)")
        return
    arr_ptr, count = params[0], params[1]
    arr = read_mem(data, runs, arr_ptr, 8 * min(count, 64))
    if arr is None:
        sys.exit(f"FATAL: stowed array @0x{arr_ptr:x} 不在 dump 内存中")
    print(f"[stowed] count={count}")
    from collections import Counter
    codes = Counter()
    shown = 0
    for i in range(min(count, 64)):
        ptr, = struct.unpack_from("<Q", arr, i * 8)
        if not ptr:
            continue
        rec = parse_record(data, runs, mods, ptr)
        if not rec:
            continue
        codes[rec.get("result", rec.get("bad_sig"))] += 1
        if not show_all and shown >= 5:
            continue
        shown += 1
        print(f"\n--- stowed[{i}] @0x{ptr:x} ---")
        print(f"  ResultCode=0x{rec['result'] & 0xffffffff:08x}" if "result" in rec else f"  bad_sig={rec.get('bad_sig')}")
        if "thread" in rec:
            print(f"  form=1 thread={rec['thread']} flags=0x{rec['flags']:x}")
        if "address" in rec:
            nm, rva = mod_of(mods, rec["address"])
            print(f"  form=0 address=0x{rec['address']:x}" + (f" ({nm}+0x{rva:x})" if nm else ""))
        if "nested_code" in rec:
            nm, rva = mod_of(mods, rec["nested_addr"]) if rec.get("nested_addr") else (None, None)
            print(f"  nested type=0x{rec['nested_type']:08x} code=0x{rec['nested_code'] & 0xffffffff:08x} addr=0x{rec['nested_addr']:x}"
                  + (f" ({nm}+0x{rva:x})" if nm else "")
                  + f" params={[hex(p) for p in rec['nested_params']]}")
        for f in rec.get("frames", []):
            nm, rva = mod_of(mods, f)
            print(f"    frame 0x{f:08x}" + (f"  -> {nm}+0x{rva:x}" if nm else "  -> ?"))
    print(f"\n[summary] ResultCode 分布: " + ", ".join(f"0x{(c & 0xffffffff):08x}×{n}" for c, n in codes.most_common()))


if __name__ == "__main__":
    main()
