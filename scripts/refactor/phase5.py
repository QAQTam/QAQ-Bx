# phase5.py — P5-lite:视图构建器迁入 shell/view/(逐字搬运,逐方法分段)
import re, pathlib, sys

ROOT = pathlib.Path("crates/qaq-bx-ui/src/shell")
mod = ROOT / "mod.rs"
lines = mod.read_text(encoding="utf-8").splitlines(keepends=True)
n = len(lines)

MOVE = ["peek_overlay_layer", "shell_root_view", "player_page_content", "half_pane_layer", "home_view"]

impl_i = next(i for i, l in enumerate(lines) if l.startswith("impl AppShell {"))
impl_j = next(i for i in range(impl_i + 1, n) if lines[i] == "}\n")
FNM = re.compile(r"^    (?:pub |pub\(crate\) |pub\(super\) )?(?:async )?fn (\w+)")
meth = []
k = impl_i
while k < impl_j:
    m = FNM.match(lines[k])
    if m:
        s = k
        while s > impl_i and re.match(r"^    (///|#!\[|#\[[a-z])", lines[s - 1]):
            s -= 1
        meth.append([s, k, m.group(1)])
    k += 1
meth.sort()
segs = []
for idx, (s, d, name) in enumerate(meth):
    e = meth[idx + 1][0] if idx + 1 < len(meth) else impl_j
    segs.append((s, e, name))

picked = {}
taken = [False] * n
for s, e, name in segs:
    if name in MOVE:
        picked[name] = "".join(lines[s:e])
        for x in range(s, e):
            taken[x] = True
missing = set(MOVE) - set(picked)
if missing:
    sys.exit(f"not found: {missing}")

ni = next(i for i, l in enumerate(lines) if re.match(r"^fn nav_item\(", l))
ns = ni
while ns > 0 and re.match(r"^(///|#!\[|#\[[a-z])", lines[ns - 1]):
    ns -= 1
ne = next(i for i in range(ni, n) if re.match(r"^(#\[cfg\(test\)\]|mod tests)", lines[i]))
nav_item_blk = "".join(lines[ns:ne])
for x in range(ns, ne):
    taken[x] = True

rest = "".join(l for x, l in enumerate(lines) if not taken[x])

VIS = "pub(in crate::shell)"
def bump(b):
    return re.sub(r"^    fn ", f"    {VIS} fn ", b, flags=re.M)

files = {
    "player_views": ("//! 播放页三态视图构建:主窗/全屏 HUD 的 `PlayerPage` 内容与半窗层。\n"
        "//!\n"
        "//! 拆解约定(见 docs/dev-notes/shell-decomposition-plan.md §3):渲染面始终由同一个\n"
        "//! keyed `player:{bvid}` 组件持有(E24/E25/E26 零重挂的前提),本文件只做属性驱动的\n"
        "//! 视图装配,不增删组件层级。\n\nuse super::super::*;\n\nimpl AppShell {\n",
        [bump(picked["player_page_content"]), bump(picked["half_pane_layer"])], "}\n"),
    "peek_overlay": ("//! 悬停预览卡覆盖层(#7):全局唯一 PeekCard 的视图装配,z 序在玻璃宿主之后、\n"
        "//! shell-root 之前;无背景不拦截命中。\n\nuse super::super::*;\n\nimpl AppShell {\n",
        [bump(picked["peek_overlay_layer"])], "}\n"),
    "shell_root": ("//! 根树装配视图:shell-root Grid(keyed 稳定键,MPE 不销毁)与首页网格视图。\n\n"
        "use super::super::*;\n\nimpl AppShell {\n",
        [bump(picked["shell_root_view"]), bump(picked["home_view"])], "}\n"),
    "chrome_nav": ("//! 导航菜单项构建(NAV_ROUTES 逐项 KeyedView;desktop/mobile 两侧共用)。\n\n"
        "use super::super::*;\n\n",
        [re.sub(r"^fn nav_item", f"{VIS} fn nav_item", nav_item_blk, flags=re.M)], ""),
}
vdir = ROOT / "view"
vdir.mkdir(exist_ok=True)
for name, (head, blocks, tail) in files.items():
    (vdir / f"{name}.rs").write_text(head + "\n".join(b.rstrip("\n") + "\n" for b in blocks) + tail, encoding="utf-8")

(vdir / "mod.rs").write_text(
    "//! view 区域构建(P5-lite):视图构建方法自 mod.rs 逐字迁移,`view()` 根装配留守\n"
    "//! (窗口视觉/尺寸泵/回调装配/页面分发/导航壳,待后续阶段进一步分拆)。\n\n"
    "mod chrome_nav;\nmod peek_overlay;\nmod player_views;\nmod shell_root;\n\n"
    "pub(in crate::shell) use chrome_nav::nav_item;\n",
    encoding="utf-8",
)

rest = rest.replace("mod handlers;\n", "mod handlers;\nmod view;\n", 1)
mod.write_text(rest, encoding="utf-8")
print(f"moved: {sorted(picked)} + nav_item; mod.rs -> {len(rest.splitlines())} lines")
print("OK")
