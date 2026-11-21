# phase2.py — 纯函数/纯类型迁入 shell/util.rs(分段模型 v2)
# 模型:0 缩进顶层项 = [前置文档/属性起点, 下一项文档起点);按清单选取段并搬运。
import re, pathlib, sys

ROOT = pathlib.Path("crates/qaq-bx-ui/src/shell")
MOD = ROOT / "mod.rs"
lines = MOD.read_text(encoding="utf-8").splitlines(keepends=True)
n = len(lines)

ITEM_RES = [
    r"^pub fn thumbnail_url\b",
    r"^fn thumbnail_url_with\b",
    r"^fn video_item\b",
    r"^fn normalize_face_url\b",
    r"^fn title_bar_tier_signature\b",
    r"^fn resize_render_signature\b",
    r"^fn portrait_window_constraints\b",
    r"^pub struct ResolveProfileKey\b",
    r"^fn resolve_params_for\b",
    r"^fn resolve_profile_key\b",
    r"^fn video_config_from\b",
    r"^fn danmaku_filter_from\b",
    r"^fn danmaku_segment_count\b",
    r"^fn danmaku_segment_at\b",
    r"^fn next_danmaku_segment\b",
    r"^pub\(crate\) fn search_enter_submits\b",
    r"^fn engine_switch_plan\b",
    r"^fn search_video_item\b",
    r"^fn user_card_item\b",
    r"^fn archive_video_item\b",
    r"^fn minimal_video_item\b",
    r"^fn merge_popular_items\b",
]

# 1) 全部顶层项定义行(0 缩进)
DEF = re.compile(
    r"^(pub |pub\(crate\) |pub\(super\) )?(fn|struct|enum|impl|const|static|mod|trait|type)\b"
)
defs = [i for i, l in enumerate(lines) if DEF.match(l)]
if not defs:
    sys.exit("no top-level items found")

def doc_start(def_idx):
    s = def_idx
    while s > 0 and re.match(r"^(///|#!\[|#\[[a-z])", lines[s - 1]):
        s -= 1
    return s

# 2) 项 → 段 [doc_start_i, doc_start_{i+1})
segs = []  # (start, def_idx, end)
for k, d in enumerate(defs):
    nxt = doc_start(defs[k + 1]) if k + 1 < len(defs) else n
    segs.append((doc_start(d), d, nxt))

# 3) 清单命中项
hits = {}
for res in ITEM_RES:
    d = next((d for (s, d, e) in segs if re.match(res, lines[d])), None)
    if d is None:
        sys.exit(f"NOT FOUND: {res}")
    hits[d] = res

# 4) 命中段的连续区间(其余段留在 mod.rs;命中段之间的未命中段报错人工处理)
want = sorted(s for (s, d, e) in segs if d in hits)
ranges = []
cur_s, cur_e = None, None
for (s, d, e) in segs:
    if d in hits:
        if cur_s is not None and s == cur_e:
            cur_e = e
        else:
            if cur_s is not None:
                ranges.append((cur_s, cur_e))
            cur_s, cur_e = s, e
if cur_s is not None:
    ranges.append((cur_s, cur_e))

moved, rest, taken = [], [], [False] * n
for s, e in ranges:
    for x in range(s, e):
        taken[x] = True
for x in range(n):
    (moved if taken[x] else rest).append(lines[x])

# 5) 组装 util.rs 与 mod.rs
util_rs = (
    "//! shell 纯函数与纯类型:无状态、无 self、无 context 的值级变换。\n"
    "//!\n"
    "//! 归属约定:\n"
    "//! - URL/卡片映射(thumbnail_url 系、video_item 系)—— 数据桥接纯函数;\n"
    "//! - 布局签名(resize_render_signature/title_bar_tier_signature/portrait 约束)—— 窗口域判定;\n"
    "//! - 设置映射(resolve_params/video_config_from/danmaku_filter_from)—— AppSettings → 引擎参数;\n"
    "//! - 弹幕分段数学(danmaku_segment_*)—— 纯算术,单测锚点 6 分钟边界;\n"
    "//! - engine_switch_plan / search_enter_submits —— 引擎切换与搜索回车判定纯决策。\n"
    "//! 单元测试位于 shell/tests.rs(经 `use super::*` 可见,调用点零改动)。\n\n"
    "use super::*;\n\n"
) + "".join(moved)

out, inserted = [], False
for l in rest:
    out.append(l)
    if not inserted and l.startswith("mod message;"):
        out.append("mod util;\n")
        inserted = True
if not inserted:
    sys.exit("mod message; not found")
mod_text = "".join(out).replace(
    "pub use self::message::Message;",
    "pub use self::message::Message;\npub use self::util::*;",
    1,
)
MOD.write_text(mod_text, encoding="utf-8")
(ROOT / "util.rs").write_text(util_rs, encoding="utf-8")
print(f"moved {len(hits)} items in {len(ranges)} ranges:")
for s, e in ranges:
    print(f"  mod.rs L{s+1}..{e}")
print(f"mod.rs {n} -> {len(rest)} lines; util.rs ~{len(moved)} lines")
print("OK")
