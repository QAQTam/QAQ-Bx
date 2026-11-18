# Crash offset symbolizer (no Windows SDK / WinDbg required).
#
# 用途:Event ID 1000(Application Error)给出 FaultingModule + FaultingOffset(RVA)。
# 本脚本从模块的 CodeView 调试目录解析 PDB GUID,必要时从 msdl 微软符号服务器
# 下载对应 PDB,然后在 publics 流中定位包含该 RVA 的符号(最近的上下符号)。
#
# 用法:
#   python symbolize.py <模块dll路径> <FaultingOffset 十六进制,可带 0x 前缀>
#   python symbolize.py <dll> 359595 --disasm      # 额外反汇编故障点附近(Capstone 可选)
#
# 输出:目标 RVA 所在 section/offset,最近的 publics(下方与上方),以及可选反汇编。
# 注意:FaultingOffset 即模块内 RVA,无需再做 ASLR 换算。
from __future__ import annotations

import argparse
import struct
import sys
from pathlib import Path

MSDL = "https://msdl.microsoft.com/download/symbols/{name}/{guid}{age}/{name}"


def parse_sections(data: bytes):
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    nsec = struct.unpack_from("<H", data, pe + 6)[0]
    optsz = struct.unpack_from("<H", data, pe + 20)[0]
    opt = pe + 24
    magic = struct.unpack_from("<H", data, opt)[0]
    ddoff = opt + (112 if magic == 0x20B else 96)
    sec = opt + optsz
    sections = []
    for i in range(nsec):
        s = sec + i * 40
        va = struct.unpack_from("<I", data, s + 12)[0]
        vsz = struct.unpack_from("<I", data, s + 8)[0]
        raw = struct.unpack_from("<I", data, s + 20)[0]
        name = data[s : s + 8].rstrip(b"\0").decode(errors="replace")
        sections.append((name, va, vsz, raw))
    return sections, ddoff, opt


def rva_to_file(data: bytes, sections, rva):
    for _, va, vsz, ptr in sections:
        if va <= rva < va + vsz:
            return ptr + (rva - va)
    return None


def pdb_signature(dll: Path):
    data = dll.read_bytes()
    sections, ddoff, _ = parse_sections(data)
    dir_rva, dir_size = struct.unpack_from("<II", data, ddoff + 6 * 8)
    dir_off = rva_to_file(data, sections, dir_rva)
    if dir_off is None:
        sys.exit("调试目录不在任何 section 内")
    cv_raw = None
    cv_size = 0
    for i in range(dir_size // 28):
        ent = dir_off + i * 28
        (typ,) = struct.unpack_from("<I", data, ent + 12)
        (sz,) = struct.unpack_from("<I", data, ent + 16)
        (addr,) = struct.unpack_from("<I", data, ent + 20)
        if typ == 2:  # IMAGE_DEBUG_TYPE_CODEVIEW
            cv_raw = rva_to_file(data, sections, addr) or addr
            cv_size = sz
            break
    if cv_raw is None:
        sys.exit("未找到 CodeView 调试项")
    if data[cv_raw : cv_raw + 4] != b"RSDS":
        sys.exit("CodeView 签名不是 RSDS(旧 PDB 格式,本工具不支持)")
    guid = data[cv_raw + 4 : cv_raw + 20]
    (age,) = struct.unpack_from("<I", data, cv_raw + 20)
    name = data[cv_raw + 24 : cv_raw + 24 + cv_size].split(b"\0")[0].decode(errors="replace")
    d1, d2, d3 = struct.unpack_from("<IHH", guid, 0)
    guid_s = f"{d1:08x}{d2:04x}{d3:04x}" + guid[8:16].hex()
    return name, guid_s, age


def fetch_pdb(pdb_name: str, guid: str, age: int, cache: Path) -> Path:
    cache.mkdir(parents=True, exist_ok=True)
    file_name = Path(pdb_name.replace("\\", "/")).name
    pdb_path = cache / file_name
    if pdb_path.exists():
        return pdb_path
    url = MSDL.format(name=file_name, guid=guid, age=age)
    print(f"下载 PDB:{url}")
    import subprocess

    for attempt in range(3):
        result = subprocess.run(
            ["curl", "-sL", "-C", "-", "--retry", "3", "-o", str(pdb_path), url],
            capture_output=True,
        )
        if result.returncode == 0 and pdb_path.exists():
            data = pdb_path.read_bytes()
            bs = struct.unpack_from("<I", data, 0x20)[0]
            nblocks = struct.unpack_from("<I", data, 0x28)[0]
            if len(data) >= nblocks * bs:
                return pdb_path
        print(f"  第 {attempt + 1} 次下载不完整,重试…")
    sys.exit("PDB 下载失败;可手动放到缓存目录后重跑:" + str(cache))


class Msf:
    """MSF 7.00 读取器(目录块表在 BlockMapAddress@0x34,无前导计数)。"""

    def __init__(self, path: Path):
        self.data = path.read_bytes()
        d = self.data
        if d[:32] != b"Microsoft C/C++ MSF 7.00\r\n\x1aDS\0\0\0":
            sys.exit("不是 MSF 7.00 PDB")
        self.bs = struct.unpack_from("<I", d, 0x20)[0]
        dirb = struct.unpack_from("<I", d, 0x2C)[0]
        bmap = struct.unpack_from("<I", d, 0x34)[0]
        ndb = (dirb + self.bs - 1) // self.bs
        blocks = struct.unpack_from(f"<{ndb}I", d, bmap * self.bs)
        blob = b"".join(d[b * self.bs : (b + 1) * self.bs] for b in blocks)[:dirb]
        p = 0
        (n,) = struct.unpack_from("<I", blob, p)
        p += 4
        sizes = struct.unpack_from(f"<{n}I", blob, p)
        p += 4 * n
        self.sizes = sizes
        self.blocks = []
        for sz in sizes:
            cnt = (sz + self.bs - 1) // self.bs if sz not in (0, 0xFFFFFFFF) else 0
            self.blocks.append(struct.unpack_from(f"<{cnt}I", blob, p))
            p += 4 * cnt

    def stream(self, idx: int) -> bytes:
        sz = self.sizes[idx]
        if sz in (0, 0xFFFFFFFF):
            return b""
        bs = self.bs
        return b"".join(
            self.data[b * bs : (b + 1) * bs] for b in self.blocks[idx]
        )[:sz]


def rva_to_sec_off(sections, rva):
    for name, va, vsz, _ in sections:
        if va <= rva < va + vsz:
            return (sections.index((name, va, vsz, _)) + 1, rva - va, name)
    return None, None, None


def scan_publics(buf: bytes):
    """线性扫描一条 CV 记录链,收集 S_PUB32(sec, off, name)。"""
    pubs = []
    start = None
    for cand in range(0, min(len(buf), 0x20000), 2):
        if cand + 4 > len(buf):
            break
        ln, kind = struct.unpack_from("<HH", buf, cand)
        if 4 <= ln <= 0x1000 and cand + 2 + ln <= len(buf):
            start = cand
            break
    if start is None:
        return pubs
    o = start
    while o + 4 <= len(buf):
        ln, kind = struct.unpack_from("<HH", buf, o)
        if ln < 4 or o + 2 + ln > len(buf):
            break
        if kind == 0x110E:
            # 本 PDB 的 S_PUB32 payload = flags(4) off(4) sect(2),name 紧随其后
            flags, off, s = struct.unpack_from("<IIH", buf, o + 4)
            name = buf[o + 14 : o + 2 + ln].split(b"\0")[0].decode(errors="replace")
            if 0 < s < 0x100 and 1 <= len(name) <= 800:
                pubs.append((s, off, name))
        o += ln + 2
    return pubs


def nearest(pubs, tsec, toff):
    same = sorted([q for q in pubs if q[0] == tsec], key=lambda q: q[1])
    below = [q for q in same if q[1] <= toff]
    above = [q for q in same if q[1] > toff]
    return below[-5:], above[:3]


def demangle(name: str) -> str:
    """C++/WinRT consume/produce 模板名做最小可读化(仅截断常见长前缀)。"""
    return name


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("dll", type=Path)
    ap.add_argument("offset", type=str, help="FaultingOffset(RVA),十六进制")
    ap.add_argument("--disasm", action="store_true")
    ap.add_argument("--cache", type=Path, default=None)
    args = ap.parse_args()

    rva = int(args.offset, 16)
    data = args.dll.read_bytes()
    sections, _, _ = parse_sections(data)
    pdb_name, guid, age = pdb_signature(args.dll)
    print(f"模块:{args.dll.name}")
    print(f"PDB:{pdb_name}  GUID:{guid}  age:{age}")

    cache = args.cache or Path.home() / "AppData/Local/qaq-bx/crash-sym"
    pdb_path = fetch_pdb(pdb_name, guid, age, cache)
    msf = Msf(pdb_path)

    tsec, toff, secname = rva_to_sec_off(sections, rva)
    if tsec is None:
        sys.exit(f"RVA 0x{rva:x} 不在任何 section 内")
    print(f"目标 RVA 0x{rva:x} → section {tsec} ({secname}) offset 0x{toff:x}")

    best, best_len = [], 0
    for idx in range(len(msf.sizes)):
        if msf.sizes[idx] in (0, 0xFFFFFFFF) or msf.sizes[idx] < 4096:
            continue
        pubs = scan_publics(msf.stream(idx))
        if len(pubs) > best_len:
            best, best_len = pubs, len(pubs)
    print(f"publics 符号:{best_len} 条")
    if not best:
        sys.exit("未找到 publics(可能 PDB 是私有版本),无法定位符号")

    below, above = nearest(best, tsec, toff)
    for s, off, name in reversed(below):
        print(f"  ↓ 0x{off:08x} (+0x{toff - off:x})  {name}")
    for s, off, name in above:
        print(f"  ↑ 0x{off:08x} (-0x{off - toff:x})  {name}")

    if args.disasm:
        try:
            from capstone import Cs, CS_ARCH_X86, CS_MODE_64
        except ImportError:
            sys.exit("--disasm 需要 capstone:python -m pip install capstone")
        off = None
        for _, va, vsz, ptr in sections:
            if va <= rva < va + vsz:
                off = ptr + (rva - va)
                base = va
                break
        lo = off - 0x40
        blob = data[lo : off + 0x30]
        md = Cs(CS_ARCH_X86, CS_MODE_64)
        for ins in md.disasm(blob, rva - 0x40):
            mark = "  <<<< FAULT" if ins.address <= rva < ins.address + ins.size else ""
            print(f"0x{ins.address:x}: {ins.mnemonic} {ins.op_str}{mark}")


if __name__ == "__main__":
    main()
