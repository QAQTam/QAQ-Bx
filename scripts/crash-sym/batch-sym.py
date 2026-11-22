#!/usr/bin/env python3
"""batch-sym.py — 批量 RVA→符号(每个 PDB 只解析一次)。

用法:
  python batch-sym.py <模块dll/exe路径> <RVAhex> [<RVAhex>...] [--pdb <显式PDB路径>] [--disasm-fault]
输出每条 RVA 的最近 public(下方函数起点 + 偏移,上方邻居)。
"""
import argparse
import struct
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from symbolize import parse_sections, pdb_signature, fetch_pdb, Msf, rva_to_sec_off


def pdb_info_guid(pdb_path: Path):
    """PDB InfoStream(1): Version,Signature,Age,GUID…"""
    msf = Msf(pdb_path)
    info = msf.stream(1)
    if len(info) < 28:
        return None, None
    age = struct.unpack_from("<I", info, 8)[0]
    guid = info[12:28]
    d1, d2, d3 = struct.unpack_from("<IHH", guid, 0)
    return (f"{d1:08x}{d2:04x}{d3:04x}" + guid[8:16].hex(), age)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("dll", type=Path)
    ap.add_argument("offsets", nargs="+", type=str)
    ap.add_argument("--pdb", type=Path, default=None)
    args = ap.parse_args()

    data = args.dll.read_bytes()
    sections, _, _ = parse_sections(data)
    pdb_name, guid, age = pdb_signature(args.dll)
    print(f"模块:{args.dll.name}  PDB:{pdb_name}  GUID:{guid}  age:{age}")

    cache = Path.home() / "AppData/Local/qaq-bx/crash-sym"
    if args.pdb:
        pdb_path = args.pdb
        g, a = pdb_info_guid(pdb_path)
        print(f"显式 PDB:{pdb_path}  GUID:{g} age:{a} " + ("✔ 匹配" if g == guid and a == age else "✘ 不匹配!"))
    else:
        pdb_path = fetch_pdb(pdb_name, guid, age, cache)

    msf = Msf(pdb_path)
    best, best_len = [], 0
    for idx in range(len(msf.sizes)):
        sz = msf.sizes[idx]
        if sz in (0, 0xFFFFFFFF) or sz < 4096:
            continue
        pubs = []
        # scan_publics 复用
        from symbolize import scan_publics
        pubs = scan_publics(msf.stream(idx))
        if len(pubs) > best_len:
            best, best_len = pubs, len(pubs)
    print(f"publics 符号:{best_len} 条")
    if not best:
        sys.exit("未找到 publics")

    table = sorted(best, key=lambda q: (q[0], q[1]))

    def nearest(tsec, toff):
        import bisect
        lo_sec = [q for q in table if q[0] == tsec]
        offs = [q[1] for q in lo_sec]
        i = bisect.bisect_right(offs, toff) - 1
        below = lo_sec[i] if i >= 0 else None
        above = lo_sec[i + 1] if i + 1 < len(lo_sec) else None
        return below, above

    for rva_s in args.offsets:
        rva = int(rva_s, 16)
        tsec, toff, secname = rva_to_sec_off(sections, rva)
        if tsec is None:
            print(f"0x{rva:08x}: 不在任何 section")
            continue
        b, a = nearest(tsec, toff)
        if b:
            print(f"0x{rva:08x} [{secname}+0x{toff:x}] = {b[2]}+0x{toff - b[1]:x}"
                  + (f"   (next: {a[2]}@0x{a[1]:x})" if a else ""))
        else:
            print(f"0x{rva:08x} [{secname}+0x{toff:x}] = ? (无下方符号)")


if __name__ == "__main__":
    main()
