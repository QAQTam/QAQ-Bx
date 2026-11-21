# phase3.py — update() giant match → shell/handlers/ 域分发(逐字搬运,零语义变化)
#
# 设计(详见 docs/dev-notes/shell-decomposition-plan.md §3):
# - handlers/mod.rs:dispatch 的路由 match 穷尽覆盖 Message 全部变体(无 _ 臂),
#   新增变体未路由时编译失败 —— 替代原 giant match 的编译期穷尽性。
# - handlers/<域>.rs:`impl AppShell { pub(super) fn handle(...) { match message { <臂逐字>, _ => {} } } }`
#   臂原为按值匹配,逐字搬运;域方法/结构体随迁,可见性提升 pub(in crate::shell)。
# - mod.rs 的 update() 缩为 3 行薄分发;view 构建器与 create 留守(P5 再拆)。
import re, pathlib, sys

ROOT = pathlib.Path("crates/qaq-bx-ui/src/shell")
mod_path = ROOT / "mod.rs"
msg_path = ROOT / "message.rs"
lines = mod_path.read_text(encoding="utf-8").splitlines(keepends=True)
mlines = msg_path.read_text(encoding="utf-8").splitlines(keepends=True)
n = len(lines)

# ───────────────────────── 域清单与文档 ─────────────────────────
DOMAINS = [
    ("settings",       "设置与存储域:UpdateSettings 全量设置写回(引擎/画质/弹幕/竖屏即时生效链)、\n//! 300ms 防抖落盘(FlushSettingsSave/SettingsSaved)、缓存一键清空(ClearCaches→CachesCleared,\n//! 白名单纪律:credentials/settings/非 .bin 绝不动,见 docs/Update-0.6.0.md D 项)。"),
    ("auth",           "认证/登录域:启动探测(AuthProbed)、扫码登录状态机(AuthStartLogin→LoginQrReady→\n//! LoginPollTick/LoginPolled)、登出(AuthLogout→AuthLoggedOut)与会话清理(clear_login_session/\n//! reset_account_data,登出后各列表/账号态归零)。"),
    ("account_notify", "账号/通知/私信/关系域:个人中心(AccountRefresh/AccountStatLoaded/UnreadLoaded)、\n//! 通知 Tab 与回复流(Replies/Ats/Likes/SystemsLoaded)、私信会话与线程(SessionsLoaded/\n//! WhisperThreadLoaded)、关注粉丝关系(OpenRelations→RelationsLoaded)。"),
    ("search",         "搜索域:草稿与提交(SearchDraftChanged/SearchSubmitted)、结果吸收与失败\n//! (SearchResults/SearchFailed)、热词默认填充(SearchDefaultReady/SearchPickDefault)。"),
    ("window",         "窗口/布局/竖屏域:WindowResized 渲染签名门控 + resize 风暴统计(ResizeStormTrack,\n//! 结构体同文件)+ settled 去抖收口、ShellInvalidateLayout、竖屏停靠(TogglePortraitDock/\n//! PortraitSynced,窗口约束见 util.rs portrait_window_constraints)。"),
    ("gamepad",        "手柄域:GamepadInput(Some/None) 事件流、handle_gamepad_action 焦点导航与快捷键、\n//! focused_home_video 焦点解析。"),
    ("smtc",           "SMTC 系统媒体控制域:SmtcAction 命令回环(播放/暂停/上一条/下一条)、sync_smtc\n//! 元数据/缩略图/进度同步、apply_playback_want 播放意愿仲裁、reset_smtc_writer 引擎切换时重挂。"),
    ("comments",       "评论与楼中楼域:CommentsSessionState/ReplyBucket 状态机(排序桶、头像引用计数、\n//! snapshot 版本)、主楼加载/排序切换/追加(CommentsLoaded/SetCommentSort/LoadMoreComments)、\n//! 子回复展开与加载(ToggleSubReplies/SubRepliesLoaded)。"),
    ("danmaku",        "弹幕域:分段加载(DanmakuPart/DanmakuFailed,分段数学见 util.rs danmaku_segment_*)、\n//! DanmakuStage 装卸与配置下发(danmaku_config/filter)、进度重播种(reseed_danmaku)。"),
    ("lists",          "列表页域:热门/直播/历史/稍后再看/收藏五页 ensure/fetch/absorb 全链、ListAutoMore\n//! 滚动自动续页门控(见 docs/Update-0.6.0.md A 项)、直播开播(start_live_playback)、\n//! 热门拉取批(spawn_popular_fetch)。"),
    ("peek",           "悬停预览卡域:PeekMachine 三拍定时(预取/弹出/宽限,见 #7)、hover 上报与撤销、\n//! 简介预取(fetch_peek_desc)、卡片动作(PeekPlay/PeekWatchLater/PeekClose)、媒体预解析\n//! 缓存(cache/take_fresh_preresolved,ResolveProfileKey 维度,供开播免等待)。"),
    ("home_feed",      "首页 feed 域:HomeLoaded 吸收(骨架槽分配/去重/合并)、行激活(HomeRowActive)、\n//! 刷新与重试(HomeRefresh/HomeRetry/HomeFailed)、预拉批(spawn_feed_batch/apply_feed_page)、\n//! 挂载脉冲(mount_pulse_active)。"),
    ("cover_pipeline", "图片管线域(封面+头像,首页/列表/相关推荐/账号多域共用):CoverReady 回填、\n//! FlushCovers 80ms 批合并、离屏驱逐(sync_offscreen_covers,±4 屏窗口)、头像批量下载\n//! (spawn_avatars 系)与 AvatarLoaded/FlushPlaybackAvatars 分发。"),
    ("nav",            "导航与全局浮层域:路由切换(NavTagSelected/NavigateCompact/Back)、更多菜单与\n//! 搜索浮层开关、抽屉 Pane(PaneOpenChanged/TogglePane)、toast 入口(ToastActivated)、\n//! 开播入口(open_video/open_video_target)、演示与 selftest 杂项(DemoTick/SelfTestSync)。"),
    ("player_ctl",     "播放器控制域:开播入口(OpenVideo/OpenVideoTarget)、开关/进度/音量/静音/弹幕开关、\n//! 全屏三态(Enter/Exit/Toggle + Synced/Probe 探测回环)、半窗三态(Expand/Close/Resize)、\n//! 引擎切换(switch_player_engine/fallback_native_to_mpv)、相关推荐(RelatedLoaded/Retry)、\n//! 停播与可见性仲裁(stop_playback/reconcile_playback_visibility)。"),
    ("player_media",   "播放器媒体域:媒体解析与流选择(MediaReady/SelectQuality/SelectPage/PageStreamReady/\n//! MediaFailed)、视频元数据(VideoMetaLoaded)、进度心跳(PositionTick 250ms)、\n//! native/mpv 装载与卡载门控(native_load_choice/mpv_load_* + *_gate_settled/should_show_loading\n//! 纯函数)、代理传输(ensure_stream_proxy/prepare_dash_transport_urls)。"),
]
DOMAIN_NAMES = [d[0] for d in DOMAINS]

# ───────────────────────── 变体 → 域 ─────────────────────────
V = {
    **{k: "nav" for k in ["Back","NavTagSelected","PaneOpenChanged","TogglePane","SelfTestSync","DemoTick","ToastActivated","ToggleMoreMenu","CloseMoreMenu","ToggleSearchOverlay","CloseSearchOverlay","NavigateCompact"]},
    **{k: "search" for k in ["SearchDraftChanged","SearchSubmitted","SearchResults","SearchFailed","SearchDefaultReady","SearchPickDefault"]},
    **{k: "player_ctl" for k in ["OpenVideo","OpenVideoTarget","TogglePlay","Seek","SetVolume","ToggleMute","ToggleDanmaku","EnterFullscreen","ExitFullscreen","ToggleFullscreen","FullscreenSynced","FullscreenProbeResult","PlayerHotkey","RelatedLoaded","RelatedRetry","ExpandHalfPane","CloseHalfPane","ResizeHalfPane"]},
    **{k: "player_media" for k in ["PositionTick","MediaReady","SelectQuality","SelectPage","PageStreamReady","VideoMetaLoaded","MediaFailed"]},
    **{k: "lists" for k in ["PopularMore","PopularLoaded","ListAutoMore","LiveRoomsLoaded","LiveCoverReady","LiveRoomSelected","LiveReady","LiveMore","HistoryLoaded","HistoryMore","WatchLaterLoaded","WatchLaterMore","FavFoldersLoaded","FavSelectFolder","FavResourcesLoaded","FavMore","UserCoverReady"]},
    **{k: "comments" for k in ["CommentDraftChanged","SubmitComment","CommentSubmitted","SetCommentSort","LoadMoreComments","RetryComments","CommentsLoaded","ToggleSubReplies","LoadMoreSubReplies","SubRepliesLoaded"]},
    **{k: "danmaku" for k in ["DanmakuPart","DanmakuFailed"]},
    **{k: "smtc" for k in ["SmtcAction"]},
    **{k: "gamepad" for k in ["GamepadInput"]},
    **{k: "home_feed" for k in ["HomeRefresh","HomeRowActive","HomeRetry","HomeLoaded","HomeFailed"]},
    **{k: "cover_pipeline" for k in ["CoverReady","FlushCovers","AvatarLoaded","FlushPlaybackAvatars"]},
    **{k: "peek" for k in ["PreResolveVideo","MediaPreResolved","CardHover","PeekPrefetchTick","PeekPopTick","PeekGraceTick","PeekAnimTick","PeekPointerEntered","PeekPointerExited","PeekDescReady","PeekPlay","PeekWatchLater","PeekWatchLaterDone","PeekClose"]},
    **{k: "window" for k in ["WindowResized","ShellInvalidateLayout","WindowResizeSettled","TogglePortraitDock","PortraitSynced"]},
    **{k: "settings" for k in ["ClearCaches","CachesCleared","UpdateSettings","FlushSettingsSave","SettingsSaved"]},
    **{k: "auth" for k in ["AuthProbed","AuthStartLogin","LoginQrReady","LoginStartFailed","LoginPollTick","LoginPolled","AuthCancelLogin","AuthLogout","AuthCancelLogout","AuthLoggedOut"]},
    **{k: "account_notify" for k in ["OpenNotifications","AccountRefresh","AccountStatLoaded","UnreadLoaded","SelectNotifyTab","LoadMoreNotifyTab","RetryNotifyTab","RepliesLoaded","AtsLoaded","LikesLoaded","SystemsLoaded","SessionsLoaded","SelectWhisperSession","CloseWhisperDetail","RefreshWhisperThread","WhisperThreadLoaded","OpenRelations","RefreshRelations","LoadMoreRelations","RelationsLoaded"]},
}

# ───────────────────────── 方法 → 域(4 缩进) ─────────────────────────
M = {
    "nav": ["pick_demo_bvid","handle_demo_tick","open_video","open_video_target","navigate_back","cycle_nav_route","listen_toast"],
    "search": ["submit_search"],
    "player_ctl": ["ensure_engine","effective_player_engine","native_enabled","current_is_live","native_active","switch_player_engine","fallback_native_to_mpv","switch_to_mpv_now","teardown_native_side","fetch_related","absorb_related","enter_fullscreen","exit_fullscreen","sync_window_fullscreen","clear_native_remount_throttle","stop_playback","reconcile_playback_visibility","is_player_visible","can_enter_fullscreen"],
    "player_media": ["ensure_stream_proxy","release_stream_proxy_session","prepare_dash_transport_urls","native_load_choice","native_position","mpv_load_choice","mpv_load_streams","current_choice","advance_playback_gen","start_playback","absorb_video_meta","kickoff_video_extras","apply_resolved_media","apply_video_fallback","apply_audio_fallback","quality_menu_view","pages_menu_view"],
    "danmaku": ["ensure_stage","danmaku_replace","danmaku_append","danmaku_config","danmaku_filter","stage_sink","native_sink","reseed_danmaku","release_stage","fetch_danmaku","fetch_danmaku_segment","next_danmaku_index"],
    "cover_pipeline": ["spawn_cover_fetch","sync_offscreen_covers","schedule_cover_flush","spawn_live_cover","spawn_user_cover","schedule_playback_avatar_flush","spawn_notify_avatars","spawn_playback_avatars","spawn_avatars"],
    "home_feed": ["current_row_revision","rebuild_home_source","maybe_prefetch_next_page","allocate_skeleton_slots","truncate_unfilled_skeletons","request_feed_batch","display_duration"],
    "lists": ["ensure_popular","ensure_live","fetch_live_rooms","absorb_live_rooms","lists_logged_in","ensure_history","fetch_history","absorb_history","ensure_watch_later","fetch_watch_later","absorb_watch_later","ensure_favorites","fetch_fav_folders","absorb_fav_folders","select_fav_folder","fetch_fav_resources","absorb_fav_resources","start_live_playback","fetch_popular","absorb_popular"],
    "comments": ["refresh_comments_snapshot","fetch_comments","fetch_sub_replies"],
    "peek": ["peek_allowed","peek_hide_now","handle_card_hover","handle_peek_pop","fetch_peek_desc","peek_mounted_bvid","take_fresh_preresolved","cache_preresolved"],
    "window": ["track_resize_storm","close_resize_storm","sync_portrait_maximizable"],
    "settings": ["schedule_settings_save"],
    "auth": ["schedule_login_tick","clear_login_session","reset_account_data"],
    "account_notify": ["fetch_account_summary","relation_slot","relation_slot_mut","fetch_relations","ensure_account_loaded","ensure_active_tab_loaded","fetch_notify_tab","fetch_whisper_thread"],
    "smtc": ["reset_smtc_writer","listen_smtc","apply_playback_want","ensure_window_smtc","sync_smtc"],
    "gamepad": ["listen_gamepad","focused_home_video","handle_gamepad_action"],
}
M_KEEP = {"refresh_probe","schedule_tick","sync_key_route_ctx","shell_root_view","player_page_content","half_pane_layer","home_view","peek_overlay_layer"}

# ───────────────────────── 0 缩进项 → 域 ─────────────────────────
I = {
    "window": ["ResizeStormTrack","ResizeStorm","impl ResizeStormTrack"],
    "comments": ["ReplyBucket","CommentsSessionState","impl CommentsSessionState"],
    "player_media": ["CachedMedia","missing_media_track","MissingTrack","native_load_gate_settled","mpv_load_gate_settled","native_should_show_loading","native_load_stalled","mpv_should_show_loading","missing_native_video_track","pages_menu_from","spawn_resolve_media"],
    "home_feed": ["apply_feed_page","mount_pulse_active","spawn_feed_batch"],
    "lists": ["spawn_popular_fetch"],
}
I_KEEP_NAMES = {"ShellInput","ShellProbe","AppShell","EngineSwitchAction","nav_item"}  # impl AppShell/impl Component/mod message/mod util/mod handlers/consts 另行判定

# ───────────────────────── 1. 解析 update() giant match ─────────────────────────
upd_i = next(i for i, l in enumerate(lines) if re.match(r"^    fn update\(&mut self, message: Message", l))
match_i = next(i for i in range(upd_i, n) if re.match(r"^        match message \{", lines[i]))
pre_match = lines[upd_i + 1 : match_i]  # update() 的前置语句(refresh_probe/probe 泵/竖屏同步)——保留
print(f"pre-match statements: {len(pre_match)} lines")

arms = []  # (start, end, [variant names]) — 前臂终点 = 下一臂(含属性行)起点,无重叠
raw = []
i = match_i + 1
while i < n:
    if re.match(r"^        \}\s*$", lines[i]):
        break
    m = re.match(r"^            (Message::[A-Za-z].*)", lines[i])
    if m:
        raw.append((i, re.findall(r"Message::(\w+)", lines[i])))
    i += 1
match_close = i
starts = []
for idx, vs in raw:
    s = idx
    while s > match_i + 1 and re.match(r"^            #\[", lines[s - 1]):
        s -= 1
    starts.append(s)
for k, (idx, vs) in enumerate(raw):
    e = starts[k + 1] if k + 1 < len(raw) else match_close
    arms.append([starts[k], e, vs])
print(f"match L{match_i+1}..{match_close+1}, {len(arms)} arms")
pre_first = [l for l in lines[match_i + 1 : arms[0][0]] if l.strip() and not re.match(r"^            #\[", l)]
if pre_first:
    sys.exit(f"pre-first-arm content: {pre_first!r}")

# 校验臂变体全覆盖
arm_variants = set()
for _, _, vs in arms:
    for v in vs:
        if v not in V:
            sys.exit(f"variant not mapped: {v}")
        arm_variants.add(v)

# ───────────────────────── 2. 解析 message.rs 变体(形态+cfg 属性) ─────────────────────────
enum_i = next(i for i, l in enumerate(mlines) if l.startswith("pub enum Message {"))
enum_j = next(i for i in range(enum_i, len(mlines)) if re.match(r"^\}", mlines[i]))
info = {}  # name -> (kind, attrs)
k = enum_i + 1
cur_attrs = []
while k < enum_j:
    l = mlines[k]
    if re.match(r"^    #\[", l):
        cur_attrs.append(l)
        k += 1
        continue
    m = re.match(r"^    ([A-Z]\w+)\s*(.*)", l)
    if m:
        name, rest = m.group(1), m.group(2)
        rs = rest.strip()
        if rs.endswith(","):
            # 单行完整变体:unit / tuple(...) / 内联结构体 {...},
            pass
        else:
            # 多行变体:推进到闭合行 `    },` 或 `    ),` 并消费之
            while not re.match(r"^    [\)}],?\s*$", mlines[k]):
                k += 1
                rest += mlines[k]
            rest += mlines[k]
        rs = rest.strip()
        c = rs[0] if rs and rs != "," else ""
        kind = "struct" if c == "{" else ("tuple" if c == "(" else "unit")
        info[name] = (kind, cur_attrs)
        cur_attrs = []
    else:
        cur_attrs = [] if l.strip() == "" else cur_attrs
    k += 1
print(f"enum variants: {len(info)}")
if set(info) != arm_variants:
    sys.exit(f"MISMATCH enum-vs-arms: only_in_enum={set(info)-arm_variants} only_in_arms={arm_variants-set(info)}")

# ───────────────────────── 3. 域 → 臂块 ─────────────────────────
per_domain = {d: {"arms": [], "methods": [], "items": []} for d, _ in DOMAINS}
for s, e, vs in arms:
    per_domain[V[vs[0]]]["arms"].append("".join(lines[s:e]))
    for x in range(s, e):
        lines[x] = None  # 标记删除

# ───────────────────────── 4. 方法段(4 缩进,impl AppShell 区间) ─────────────────────────
impl_i = next(i for i, l in enumerate(lines) if l == "impl AppShell {\n")
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
        meth.append((s, k, m.group(1)))
    k += 1
meth.sort()
segs = []
for idx, (s, d, name) in enumerate(meth):
    e = meth[idx + 1][0] if idx + 1 < len(meth) else impl_j
    segs.append((s, e, name))
moved_names = set()
for s, e, name in segs:
    if name in M_KEEP:
        continue
    dom = next((d for d, names in M.items() if name in names), None)
    if dom is None:
        sys.exit(f"method not mapped: {name}")
    per_domain[dom]["methods"].append("".join(lines[s:e]))
    moved_names.add(name)
    for x in range(s, e):
        lines[x] = None
all_m = {x for names in M.values() for x in names}
if all_m - moved_names:
    sys.exit(f"mapped methods not found in impl: {all_m - moved_names}")

# ───────────────────────── 5. 0 缩进项段 ─────────────────────────
DEF = re.compile(r"^(pub |pub\(crate\) |pub\(super\) )?(fn|struct|enum|impl|const|static|mod|trait)\s+(\w+|`)")
defs = [(i, m.group(2), m.group(3)) for i, l in enumerate(lines) if l is not None and (m := DEF.match(l))]
def dstart(i0):
    s = i0
    while s > 0 and re.match(r"^(///|#!\[|#\[[a-z])", lines[s - 1] or ""):
        s -= 1
    return s
items = []
for idx, (d, kw, name) in enumerate(defs):
    nxt = dstart(defs[idx + 1][0]) if idx + 1 < len(defs) else n
    items.append((dstart(d), nxt, kw, name))
found_i = set()
for s, e, kw, name in items:
    blk = lines[s:e]
    if blk and blk[0] is not None and kw in ("struct", "fn", "enum", "impl"):
        key = name if kw != "impl" else f"impl {name}"
        dom = next((d for d, names in I.items() if key in names), None)
        if dom:
            per_domain[dom]["items"].append("".join(b for b in blk if b is not None))
            found_i.add(key)
            for x in range(s, e):
                lines[x] = None

all_i = {k for names in I.values() for k in names}
if all_i - found_i:
    sys.exit(f"0-indent mapped items not found: {all_i - found_i}")

# ───────────────────────── 6. 组装 handlers/*.rs ─────────────────────────
VIS = "pub(in crate::shell)"
def bump_vis(block):
    out = []
    depth_struct = 0
    for l in block.splitlines(keepends=True):
        s = l
        # 顶层 fn/struct/enum 提升
        if re.match(r"^fn ", s):
            s = f"{VIS} " + s
        elif re.match(r"^(struct|enum) ", s):
            s = f"{VIS} " + s
        # 4 缩进方法提升(仅裸 fn;已有 pub 的不动)
        elif re.match(r"^    fn ", s):
            s = "    " + VIS + " " + s[4:].lstrip() if False else re.sub(r"^    fn ", f"    {VIS} fn ", s)
        # 结构体字段提升(仅当处于结构体体内:粗略以 `    name:` 形态判定)
        elif re.match(r"^    [a-z_]\w*: ", s) and not re.match(r"^    (pub|fn|//)", s):
            s = re.sub(r"^    ", f"    {VIS} ", s)
        out.append(s)
    return "".join(out)

outdir = ROOT / "handlers"
outdir.mkdir(exist_ok=True)
for dom, desc in DOMAINS:
    d = per_domain[dom]
    body = []
    body.append(f"//! {desc}\n//!\n")
    body.append("//! 本文件由 P3 拆解生成:match 臂与配套方法自原 update()/impl AppShell 逐字搬运,\n//! 消息路由见 handlers/mod.rs(穷尽 match)。单元测试锚点见 shell/tests.rs。\n\n")
    body.append("use super::super::*;\n\n")
    body.append("impl AppShell {\n")
    arm_count = sum(len(re.findall(r"Message::\w+", a)) for a in d["arms"])
    body.append(f"    /// {dom} 域消息臂({arm_count} 变体,与 message.rs 分组注释对应);未命中变体静默忽略。\n")
    body.append(f"    pub(super) fn handle(m: &mut Self, message: Message, context: &ComponentContext<AppShell>) {{\n")
    body.append("        match message {\n")
    body.extend(d["arms"])
    body.append("            _ => {}\n")
    body.append("        }\n")
    body.append("    }\n\n")
    for meth_blk in d["methods"]:
        body.append(bump_vis(meth_blk))
        body.append("\n")
    body.append("}\n\n")
    for item_blk in d["items"]:
        body.append(bump_vis(item_blk))
        body.append("\n")
    (outdir / f"{dom}.rs").write_text("".join(body), encoding="utf-8")
    print(f"handlers/{dom}.rs: {len(d['arms'])} arms, {len(d['methods'])} methods, {len(d['items'])} items")

# handlers/mod.rs(路由表:穷尽 match,无 _ 臂)
route_lines = []
for name, (kind, attrs) in sorted(info.items(), key=lambda kv: [d for d, _ in DOMAINS].index(V[kv[0]])):
    pat = f"Message::{name}" if kind == "unit" else (f"Message::{name}(..)" if kind == "tuple" else f"Message::{name} {{ .. }}")
    for a in attrs:
        route_lines.append("        " + a.strip() + "\n")
    route_lines.append(f"        {pat} => Domain::{ {'nav':'Nav','search':'Search','player_ctl':'PlayerCtl','player_media':'PlayerMedia','lists':'Lists','comments':'Comments','danmaku':'Danmaku','smtc':'Smtc','gamepad':'Gamepad','home_feed':'HomeFeed','cover_pipeline':'CoverPipeline','peek':'Peek','window':'Window','settings':'Settings','auth':'Auth','account_notify':'AccountNotify'}[V[name]] },\n")
handlers_mod = (
    "//! 域分发路由:将 Message 变体路由到各域的 handle()。本 match 穷尽覆盖全部变体且无\n"
    "//! `_` 兜底臂 —— Message 新增变体而未路由时在此编译失败,替代原 giant match 的穷尽性。\n"
    "//! 各域文件只含自己的臂(`_ => {}` 兜内层未命中,仅当路由表与域臂不同步时才可能触发)。\n\n"
    "use super::*;\n\n"
    "#[derive(Clone, Copy, Debug)]\nenum Domain {\n    " + ", ".join(d.capitalize().replace("_","") for d in DOMAIN_NAMES) + ",\n}\n\n"
    "pub(super) fn dispatch(m: &mut AppShell, message: Message, context: &ComponentContext<AppShell>) {\n"
    "    let target = match &message {\n"
    + "".join(route_lines)
    + "    };\n"
    "    match target {\n"
    + "".join(f"        Domain::{d.capitalize().replace('_','')} => {d}::handle(m, message, context),\n" for d in DOMAIN_NAMES)
    + "    }\n}\n"
)
(outdir / "mod.rs").write_text(handlers_mod, encoding="utf-8")

# ───────────────────────── 7. 重建 mod.rs ─────────────────────────
rest = [l for l in lines if l is not None]
rest_j = "".join(rest)
# 7a. update() 区域替换:保留前置语句,giant match 整体删除,换为 dispatch 调用
rest_lines = rest_j.splitlines(keepends=True)
u = next(i for i, l in enumerate(rest_lines) if re.match(r"^    fn update\(&mut self, message: Message", l))
u_end = next(i for i in range(u, len(rest_lines)) if re.match(r"^    \}\s*$", rest_lines[i]))
pre_lines = [l for l in rest_lines[u + 1 : u_end] if not re.match(r"^        match message \{$", l) and not re.match(r"^        \}\s*$", l)]
new_update = [rest_lines[u]]
new_update += pre_lines
new_update += [
    "        // ── 域分发(P3 拆解,计划见 docs/dev-notes/shell-decomposition-plan.md §3)──\n",
    "        // handlers/mod.rs 的路由 match 穷尽覆盖 Message 全部变体:新增变体未路由时\n"
    "        // 在该 match 编译失败,强制补路由(替代原 giant match 的编译期穷尽性);\n"
    "        // 各域 handle() 内 `_ => {}` 仅兜路由表与域臂不同步的极端情况。\n",
    "        handlers::dispatch(self, message, context);\n",
    "    }\n",
]
rest_lines[u : u_end + 1] = new_update
# 7b. mod 声明
text2 = "".join(rest_lines)
text2 = text2.replace("mod util;\n", "mod util;\nmod handlers;\n", 1)
# 7c. 顶部模块文档补架构说明
arch = (
    "//!\n"
    "//! ## 拆解布局(P3,见 docs/dev-notes/shell-decomposition-plan.md)\n"
    "//! - `message.rs` Message 枚举(163 变体按域分组);`util.rs` 纯函数/纯类型;\n"
    "//! - `handlers/` update 域分发:mod.rs 穷尽路由表 + 各域 handle(臂逐字自原 giant match 搬运);\n"
    "//! - `tests.rs` 单测;`mod.rs` 留守 AppShell 结构体、create()/view() 与少量共通方法\n"
    "//!   (refresh_probe/schedule_tick/视图构建器,P5 再拆 view/)。\n"
)
m = re.match(r"^(//![^\n]*\n)+", text2)
text2 = text2[: m.end()] + arch + text2[m.end():]
mod_path.write_text(text2, encoding="utf-8")
print(f"mod.rs -> {len(text2.splitlines())} lines")
print("OK")
