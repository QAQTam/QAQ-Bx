# recon.py — 列出 mod.rs 顶层项与 impl AppShell 方法清单
import re, pathlib

lines = pathlib.Path("crates/qaq-bx-ui/src/shell/mod.rs").read_text(encoding="utf-8").splitlines()
print("=== 0-indent items ===")
for i, l in enumerate(lines):
    m = re.match(r"^(pub |pub\(crate\) |pub\(super\) )?(fn|struct|enum|impl|const|static|mod|trait)\s+(\w+)?", l)
    if m:
        print(f"{i+1} {m.group(2)} {m.group(3) or '?'}")
print("=== impl AppShell methods (4-indent) ===")
impl_start = next(i for i, l in enumerate(lines) if l.startswith("impl AppShell {"))
impl_end = next(i for i in range(impl_start + 1, len(lines)) if lines[i] == "}")
print(f"[impl AppShell: L{impl_start+1}..L{impl_end+1}]")
for i in range(impl_start, impl_end):
    m = re.match(r"^    (?:pub |pub\(crate\) |pub\(super\) )?(?:async )?fn (\w+)", lines[i])
    if m:
        print(f"{i+1} {m.group(1)}")
