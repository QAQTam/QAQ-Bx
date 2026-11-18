#!/usr/bin/env python3
"""minidump-info.py — 纯 Python 读取 .dmp(Minidump)关键流,无第三方依赖。

用途:不装 WinDbg 也能先验三件事——
  1) dump 结构完整可解析;
  2) 异常码/故障地址/崩溃线程(与 WER 报告的 c0000005 对齐);
  3) 故障地址落在哪个模块、模块内 RVA 多少(交给 symbolize.py 进一步符号化)。

用法:
  python minidump-info.py <file.dmp> [--modules-filter <substr>]
"""
import struct
import sys

STREAM_NAMES = {
    3: "ThreadListStream", 4: "ModuleListStream", 5: "MemoryListStream",
    6: "ExceptionStream", 7: "SystemInfoStream", 15: "MemoryInfoListStream",
    16: "HandleOperationListStream", 17: "TokenStream",
}


class Reader:
    def __init__(self, data: bytes):
        self.data = data

    def u32(self, off): return struct.unpack_from("<I", self.data, off)[0]
    def u64(self, off): return struct.unpack_from("<Q", self.data, off)[0]

    def wstr(self, off):
        # MINIDUMP_STRING: u32 长度(字节数, UTF-16LE) + 数据
        n = self.u32(off)
        return self.data[off + 4: off + 4 + n].decode("utf-16-le", "replace")


def parse(path: str, mod_filter):
    with open(path, "rb") as f:
        data = f.read()
    if data[:4] != b"MDMP":
        sys.exit(f"FATAL: 不是 minidump (magic={data[:4]!r})")
    r = Reader(data)
    n_streams = r.u32(8)
    dir_rva = r.u32(12)
    print(f"[header] streams={n_streams} dir@0x{dir_rva:x} size={len(data):,}B")

    streams = {}
    for i in range(n_streams):
        e = dir_rva + i * 12
        typ, size, rva = struct.unpack_from("<III", data, e)
        streams[typ] = (rva, size)
        name = STREAM_NAMES.get(typ, f"type{typ}")
        print(f"[stream] {name:26s} size={size}")

    if 7 not in streams:
        sys.exit("FATAL: 无 SystemInfoStream,转储不完整")
    si = streams[7][0]
    arch = struct.unpack_from("<H", data, si)[0]
    print(f"[system] arch={arch} (9=amd64) cpus={data[si + 8]}")

    # 模块表:先建,供异常地址归属。
    # MINIDUMP_MODULE = BaseOfImage8 + SizeOfImage4 + CheckSum4 + TimeDateStamp4
    #   + ModuleNameRVA4 + VsFixedInfo52 + CvRecord8 + MiscRecord8 + Reserved16 = 108B
    modules = []
    if 4 not in streams:
        print("[warn] 无 ModuleListStream")
    else:
        base, size = streams[4]
        count = r.u32(base)
        for i in range(count):
            off = base + 4 + i * 108
            img_base = r.u64(off)
            img_size = r.u32(off + 8)
            ts = r.u32(off + 16)
            name_rva = r.u32(off + 20)
            modules.append((img_base, img_size, r.wstr(name_rva), ts))
        print(f"[modules] {len(modules)} 个")
        for b, s, n, t in modules:
            if mod_filter is None or mod_filter.lower() in n.lower():
                print(f"  {n}  base=0x{b:x} size=0x{s:x} ts=0x{t:x}")

    if 6 not in streams:
        print("[info] 无 ExceptionStream(可能不是崩溃转储)")
        return
    base = streams[6][0]
    thread_id = r.u32(base)
    exc = base + 8  # ThreadId4 + Align4 + MINIDUMP_EXCEPTION
    # 实测字段序(EXCEPTION_RECORD 序,而非 MS 头文件声明的顺序——本 dump 校验):
    # code4 flags4 record8 addr8 np4 pad4 info[15]*8
    code = r.u32(exc)
    addr = r.u64(exc + 16)
    params = r.u32(exc + 24)
    info0 = r.u64(exc + 32)
    info1 = r.u64(exc + 40)
    print(f"[exception] thread={thread_id} code=0x{code:08x} addr=0x{addr:x} "
          f"params={params} info[0]=0x{info0:x} info[1]=0x{info1:x}")
    hit = next((m for m in modules if m[0] <= addr < m[0] + m[1]), None)
    if hit:
        _, _, name, ts = hit
        print(f"[fault-module] {name} RVA=0x{addr - hit[0]:x} PE_ts=0x{ts:x}")
    else:
        print("[fault-module] (不在任何模块内——可能落在匿名/已卸载内存)")

    # 崩溃线程:MINIDUMP_THREAD = id4 susp4 prioClass4 prio4 Teb8
    #   + Stack descriptor(Start8+Size4+Rva4=16) + Context descriptor(8) = 48B
    if 3 in streams:
        tbase = streams[3][0]
        tcount = r.u32(tbase)
        for i in range(tcount):
            off = tbase + 4 + i * 48
            tid = r.u32(off)
            if tid == thread_id:
                stack_start, stack_size = r.u64(off + 24), r.u32(off + 32)
                ctx_rva = r.u32(off + 44)
                rip = r.u64(ctx_rva + 0xF8)  # CONTEXT(x64): Rip @ 0xF8
                rsp = r.u64(ctx_rva + 0x98)  # CONTEXT(x64): Rsp @ 0x98
                print(f"[thread {tid}] stack=[0x{stack_start:x},+0x{stack_size:x}] "
                      f"Rip=0x{rip:x} Rsp=0x{rsp:x}")
                break


if __name__ == "__main__":
    args = sys.argv[1:]
    filt = None
    if "--modules-filter" in args:
        i = args.index("--modules-filter")
        filt = args[i + 1]
        del args[i:i + 2]
    if not args:
        sys.exit(__doc__)
    parse(args[0], filt)
