# fix_returns.py — 保持臂内顶层 return 的"跳过尾后语句"语义
# 原语义:臂内非闭包 return 直接退出 update() → 跳过 reconcile/peek 兜底/refresh_probe 尾巴。
# 新结构:handle 返回 bool(skip_tail);dispatch 在 !skip_tail 时执行尾巴;update 只剩 pre + dispatch。
# 变换:臂块内「非闭包嵌套」的 return; → return true;(闭包内 return 不动)。
import re, pathlib, sys

HDIR = pathlib.Path("crates/qaq-bx-ui/src/shell/handlers")
DOMAINS = ["settings","auth","account_notify","search","window","gamepad","smtc","comments",
           "danmaku","lists","peek","home_feed","cover_pipeline","nav","player_ctl","player_media"]

CLOSURE_PAT = re.compile(r"move\s*\||\|\s*[^{]*\|\s*\{\s*$|\|\|\s*\{\s*$|async\s+move")

def strip_code(l):
    """去掉字符串字面量与行注释,返回用于括号计数的安全文本"""
    out, i, n = [], 0, len(l)
    in_str = False
    while i < n:
        c = l[i]
        if in_str:
            if c == "\\":
                i += 2
                continue
            if c == '"':
                in_str = False
            i += 1
            continue
        if c == '"':
            in_str = True
            i += 1
            continue
        if c == "/" and i + 1 < n and l[i + 1] == "/":
            break
        out.append(c)
        i += 1
    return "".join(out)

def classify(lines, ret_idx, region_start):
    """回溯块栈:遇到闭包头 → 闭包局部;到达 match 基线 → 逃逸到 update"""
    stack = []  # (line_idx, is_closure)
    for j in range(region_start, ret_idx):
        safe = strip_code(lines[j])
        opens = safe.count("{")
        closes = safe.count("}")
        for _ in range(closes):
            if stack:
                stack.pop()
        if opens > closes:
            stack.append((j, bool(CLOSURE_PAT.search(safe))))
        elif opens == closes and opens > 0 and safe.rstrip().endswith("{"):
            # 同一行先闭后开:栈上正确弹出后压入(上面循环已处理弹出)
            stack.append((j, bool(CLOSURE_PAT.search(safe))))
    for j, is_cl in reversed(stack):
        if is_cl:
            return False, j  # 闭包局部
        # 非闭包块:继续向外;若栈空 → 已到 match 基线 → 逃逸
    return True, -1

total_esc, total_cl = 0, 0
report = []
for dom in DOMAINS:
    p = HDIR / f"{dom}.rs"
    lines = p.read_text(encoding="utf-8").splitlines(keepends=True)
    m_i = next(i for i, l in enumerate(lines) if re.match(r"^        match message \{", l))
    m_j = next(i for i in range(m_i, len(lines)) if re.match(r"^    \}\s*$", lines[i]))  # handle 收尾
    # 本文件臂头列表(供报告)
    arm_heads = {i for i, l in enumerate(lines) if re.match(r"^            Message::", l)}
    changed = False
    for i in range(m_i, m_j):
        if re.match(r"^\s*return;", lines[i]):
            esc, enc = classify(lines, i, m_i)
            if esc:
                lines[i] = lines[i].replace("return;", "return true;", 1)
                changed = True
                total_esc += 1
                ah = max((a for a in arm_heads if a < i), default=None)
                report.append(f"  {dom}.rs L{i+1}: ESC  arm={lines[ah].strip()[:60] if ah is not None else '?'}")
            else:
                total_cl += 1
                report.append(f"  {dom}.rs L{i+1}: clos(innermost L{enc+1}) {lines[i].strip()[:50]}")
    if changed:
        # handle 签名 -> bool,match 收尾补 false
        text = "".join(lines)
        text = text.replace(
            "pub(super) fn handle(m: &mut Self, message: Message, context: &ComponentContext<AppShell>) {",
            "pub(super) fn handle(m: &mut Self, message: Message, context: &ComponentContext<AppShell>) -> bool {",
            1,
        )
        text = text.replace("            _ => {}\n        }\n    }\n", "            _ => {}\n        }\n        false\n    }\n", 1)
        p.write_text(text, encoding="utf-8")

print(f"escaped(→ return true): {total_esc};  closure-local(不动): {total_cl}")
print("\n".join(report))
