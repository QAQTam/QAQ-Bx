# phase1.py — shell.rs 目录化: shell.rs -> shell/{mod,message,tests}.rs
# 机械移动,零语义变化;枚举/测试按行块原样搬运(仅缩进-4 用于 tests 内层)。
import re, pathlib, sys

ROOT = pathlib.Path("crates/qaq-bx-ui/src")
SRC = ROOT / "shell.rs"
text = SRC.read_text(encoding="utf-8")
lines = text.splitlines(keepends=True)
n = len(lines)

def find(pred, start=0, end=n, desc=""):
    for i in range(start, end):
        if pred(lines[i]):
            return i
    sys.exit(f"NOT FOUND: {desc}")

# ---- 1. 定位 pub enum Message 块(含前置文档/属性行) ----
enum_start = find(lambda l: re.match(r"^pub enum Message \{", l), desc="pub enum Message")
s = enum_start
while s > 0 and re.match(r"^(///|#!\[)", lines[s - 1]):
    s -= 1
enum_start = s
enum_end = find(lambda l: re.match(r"^\}\s*$", l), enum_start, desc="enum close")
enum_block = lines[enum_start : enum_end + 1]

# ---- 2. 定位 mod tests 块(到文件尾,应为最后一项) ----
tests_attr = find(lambda l: re.match(r"^#\[cfg\(test\)\]", l), enum_end, desc="#[cfg(test)]")
mod_tests = find(lambda l: re.match(r"^mod tests \{", l), tests_attr, desc="mod tests")
if mod_tests != tests_attr + 1:
    sys.exit(f"unexpected gap between cfg(test)({tests_attr+1}) and mod tests({mod_tests+1})")
# 内层 = mod tests { 的下一行 .. 文件末行(应为 `}`) 之前
if not re.match(r"^\}\s*$", lines[-1]):
    sys.exit("file does not end with `}` — mod tests is not the last item?")
inner = lines[mod_tests + 1 : n - 1]
dedented = []
for l in inner:
    if l.startswith("    "):
        dedented.append(l[4:])
    else:
        dedented.append(l)
tests_body = ["use super::*;\n\n"] + dedented

# ---- 3. 组装 message.rs(带域分组注释,注释插入点由 VARIANTS 校验) ----
# 枚举内部变体列表(供人工核对分组注释边界)
variants = []
for i, l in enumerate(lines):
    m = re.match(r"^    ([A-Z]\w+)", l)
    if m and enum_start < i < enum_end:
        variants.append((i + 1, m.group(1)))
print(f"Message variants ({len(variants)}):")
for ln, name in variants:
    print(f"  {ln}: {name}")

# 域分组注释:插在指定变体首次出现行之前(变体名 -> 注释)
SECTIONS = [
    ("NavTagSelected",        " ── 导航与全局浮层 ──"),
    ("SearchDraftChanged",    " ── 搜索 ──"),
    ("HomeRefresh",           " ── 首页 feed ──"),
    ("CoverReady",            " ── 封面管线(首页/列表/相关推荐共用) ──"),
    ("PopularMore",           " ── 列表页(热门/直播/历史/稍后再看/收藏) ──"),
    ("OpenVideo",             " ── 播放器控制(开关/进度/音量/画质/全屏/半窗) ──"),
    ("PositionTick",          " ── 播放器媒体/引擎协商 ──"),
    ("CommentDraftChanged",   " ── 评论与楼中楼 ──"),
    ("DanmakuPart",           " ── 弹幕分段加载 ──"),
    ("CardHover",             " ── 悬停预览卡与媒体预解析 ──"),
    ("WindowResized",         " ── 窗口/布局/竖屏 ──"),
    ("ClearCaches",           " ── 设置与存储 ──"),
    ("AuthProbed",            " ── 认证/登录 ──"),
    ("OpenNotifications",     " ── 账号/通知/私信/关系 ──"),
    ("GamepadInput",          " ── 手柄 ──"),
    ("SmtcAction",            " ── SMTC 系统媒体控制 ──"),
]
msg_out = []
enum_block_ext = list(enum_block)
for name, cmt in SECTIONS:
    hit = [i for i, l in enumerate(enum_block_ext) if re.match(rf"^    {re.escape(name)}\b", l)]
    if not hit:
        sys.exit(f"section anchor variant not found in enum: {name}")
    enum_block_ext[hit[0]] = f"    // ====={cmt}\n" + enum_block_ext[hit[0]]

message_rs = (
    "//! `Message` 枚举 —— AppShell 的全部输入消息(163 变体,按域分组)。\n"
    "//!\n"
    "//! 拆解约定:每个域的 match 臂与配套方法位于 `shell/handlers/<域>.rs`,\n"
    "//! 本文件只做定义;分组注释与 handlers 文件一一对应。\n"
    "//! 变体保持平铺(阶段 P6 子枚举化默认不做,见 docs/dev-notes/shell-decomposition-plan.md)。\n\n"
    "use super::*;\n\n"
) + "".join(enum_block_ext)

# ---- 4. 组装 mod.rs ----
mod_rs = (
    lines[:enum_start]
    + ["mod message;\n", "\n", "pub use self::message::Message;\n", "\n"]
    + lines[enum_end + 1 : tests_attr]
    + ["#[cfg(test)]\n", "mod tests;\n", "\n"]
)

# ---- 5. 落盘 ----
outdir = ROOT / "shell"
outdir.mkdir(exist_ok=True)
(outdir / "mod.rs").write_text("".join(mod_rs), encoding="utf-8")
(outdir / "message.rs").write_text(message_rs, encoding="utf-8")
(outdir / "tests.rs").write_text("".join(tests_body), encoding="utf-8")
print(f"enum block: lines {enum_start+1}..{enum_end+1}")
print(f"mod tests : lines {mod_tests+1}..{n}")
print(f"mod.rs    : {len(mod_rs)} lines")
print(f"message.rs: {message_rs.count(chr(10))} lines")
print(f"tests.rs  : {len(tests_body)} lines")
print("OK — shell.rs 原文件保留待验证后删除")
