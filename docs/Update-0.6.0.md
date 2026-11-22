# QAQ-Bx v0.6.0 更新说明:列表页翻页治理 + 图片缓存体系(磁盘 LRU/一键清空/并发 8)

> 状态:**已实施并验收**(逐项带证据)。含 0.6.0.1 修订(登录态列表首页死锁)
> 与本轮四功能(自动续页/并发 8/磁盘缓存/一键清空)、0.6.0.4 修订(首页卡文本列严格不变量+标题字号调小一档)。
> 前序 0.5.0.2/0.5.0.4(悬停预览卡不透明底 + ✕ 单卡静默)已随包发布,摘要见文末。

## L. 登录态列表页首页永久「加载中…」修复(0.6.0.1)✅

| 项 | 现象 | 根因 | 修复 | 验证 |
|---|---|---|---|---|
| L-1 历史/稍后再看/收藏有计数但内容空白 | 三页页头出数值,内容区永远「加载中…」 | `UserListState` derive Default 使 `next_page=0`;fetch 用 `next_page.max(1)=1` 请求,absorb 守卫 `page != next_page(0)` 把第 1 页结果当旧页丢弃,`loading` 永久卡死 | `pages/user_lists.rs` 手写 `Default`(`next_page: 1`,fetch/absorb 同口径);首页失败也渲染「重试」胶囊;`AuthProbed` LoggedIn 分支按当前路由补 ensure(启动探测晚于进页时不再漏拉) | 单测 `first_page_roundtrip_guard_and_advance`/`first_page_error_offers_retry`;selftest `user-lists` 门控收紧为 `folders>0 && resources_settled`;登录态实机由用户装包复核 |

## A. 列表页滚动自动续页(热门/直播/历史/稍后再看/收藏)✅

此前除首页外各页只加载首屏(热门/历史/收藏 20、稍后再看 30、直播 20),必须手点「加载更多」。

| 项 | 内容 | 落点 / 证据 |
|---|---|---|
| A-1 纯判定层 | 新模块 `paging.rs`:`near_bottom(ScrollEventInfo, lead=600px)`(extent/viewport 未就绪一律 false,防布局初期误触发;内容不足一屏但 has_more 时 offset=0 也触发自动填满)+ `should_auto_page(loaded_once, loading, reached_end, has_error)` 门控真值表 | `crates/qaq-bx-ui/src/paging.rs`;单测 `near_bottom_boundaries`/`near_bottom_rejects_unready_geometry`/`auto_page_gate_truth_table` |
| A-2 视图接线 | 五页 `ScrollViewer` 挂 `on_view_changed`:接近底部投递 `Message::ListAutoMore(AutoPage::{History,WatchLater,Favorites,Popular,Live})`;手动「加载更多/重试」胶囊保留 | `pages/{popular,live,user_lists}.rs`(签名新增 `on_auto_more`) |
| A-3 shell 门控 | 单消息统一门控:首页已成功 + 无在飞 + 未终态 + 无错误才发请求;错误态留给手动重试,避免滚动事件反复打失败请求;登录页额外 `lists_logged_in` 门控、收藏额外 `selected != 0` | `shell.rs` `Message::ListAutoMore` 分支(复用既有 fetch/absorb,零新数据通路) |

## B. 封面下载并发 4 → 8 ✅

| 项 | 内容 | 落点 / 证据 |
|---|---|---|
| B-1 信号量扩容 | `COVER_CONCURRENCY 4→8`。封面为小体积缩略图(@480w_270h),瓶颈在网络往返而非带宽/内存;8 路在 4c8T/8G 上仍轻量(共享 2-worker tokio runtime 不变,80ms 批量上屏与离屏驱逐不变) | `runtime.rs`;单测 `cover_semaphore_allows_eight_concurrent_downloads` |

## C. 封面/头像磁盘缓存(重启免重下)✅

| 项 | 内容 | 落点 / 证据 |
|---|---|---|
| C-1 磁盘 LRU | 新模块 `image_disk_cache.rs`:`<data>/cache/images/{fnv1a64(url):016x}.bin`,temp+rename 原子写;上限 200MB(≈8000 张缩略图),超限按 mtime 从最旧逐出,`get` 命中刷新 mtime(近似 LRU);进程内记账总字节(启动扫描一次,写入不重复全目录 stat);所有 IO best-effort,损坏/不可写静默降级纯网络,零 panic | 单测 6 项:roundtrip/记账替换、reopen 扫描恢复、超限逐出最旧、clear 白名单(credentials/settings/非 .bin 哨兵绝不动)、hash 稳定、空 url/超上限单条 no-op |
| C-2 单一 choke point 接入 | `Auth::fetch_image_async`(封面/头像/直播封面共用)三层取数:磁盘命中免网络 → 网络下载 → 成功回填磁盘;内存 LRU 仍由各调用方 put,分层解耦 | `auth.rs`;`Auth::image_disk_cache()` 供设置页读占用 |

## D. 设置页「存储」卡 + 一键清空缓存 ✅

| 项 | 内容 | 落点 / 证据 |
|---|---|---|
| D-1 存储卡 | 设置页第四卡(双栏右下/单列末尾):磁盘缓存占用(`fmt_bytes` B/KB/MB/GB)+「清空缓存」按钮(`automation_id=settings-clear-caches`)+ 清理范围明示 + 结果 InfoBar(「已清空缓存,释放 X」) | `pages/settings.rs`(`StorageCardState`);单测 `fmt_bytes_scales`/`storage_card_renders_clear_button_and_usage`/`storage_card_shows_clear_result_notice` |
| D-2 白名单清理 | `Message::ClearCaches`:内存封面/头像 LRU + 待 flush 批次立即清(已挂卡位图共享 Arc 继续显示);磁盘目录 + `ticket.json` 走后台线程(`Auth::clear_caches_blocking`,票据内存作废下次请求自动重签),回执 `CachesCleared(freed)`。**credentials.json / settings.json 绝不动**;缓存目录内非 .bin/.tmp 文件也不动 | `shell.rs` 两分支 + `auth.rs`;幂等(空缓存再清正常回报 0 B) |

## M. 首页横屏卡文本列严格不变量 + 标题字号调小一档(0.6.0.4)✅
症状:同一行卡片中,标题 2 行的卡把 UP 主/播放·弹幕统计行顶到贴底,标题 1 行的卡信息行整体上浮(用户截图)。根因:`widgets/video_card.rs` 文本列 StackPanel 顺序流式布局,标题 TextBlock 只有 `max_lines(2)` 无定高,实际占高 1 行/2 行随内容浮动,下方行位被内容驱动。
| 项 | 内容 | 落点 / 证据 |
|---|---|---|
| M-1 纯值层不变量 | 新 `layout::CardTextLayout`:标题定高槽 `ceil(2×font×1.48em)`、UP/统计单行槽 `ceil(font×1.48em)`、`author_top/stats_top` 为纯算术常量(标题长度无输入通道);字号常量桌面标题 14→13、紧凑 12.5→12(调小一档),槽值 39/36,列总高 104/71 仍落在 107/77 额度内,卡高不变 | `layout.rs`;单测 `card_text_rows_are_title_length_independent`(槽值锚点+跨 860..3840 卡宽曲线逐点 UP/统计顶边一致)、旧预算测试 `mobile_card_text_budget_fits_without_clipping` 改不变量驱动 |
| M-2 UI 接线 | `video_card.rs` populated 卡两分支:标题/UP/统计三行全部 `.line_height()`+`.height()` 定高槽(与播放页相关推荐卡 RELATED_TITLE_HEIGHT 同配方),1 行标题顶对齐余量留白,超长 `CharacterEllipsis` 封顶;统计行 StackPanel 定高 stats_slot | 复用组件路径全覆盖:首页虚拟化网格/热门/搜索静态网格 |
| M-3 自动化点击/几何测试 | ui-edge 新场景 **E28**:隔离访客 data 目录(预置 webp_hint_shown 压制首启提示模态窗),UIA 逐卡取标题/UP/▶/弹幕 Text 顶边,断言同一行带跨卡 spread≤1px + 行序 title<author<play + ▶/弹幕 同行;视口底缘裁切卡按采样纪律跳过 | `scripts/ui-edge.ps1` E28;真机 1296×808 首行 5 卡 tops=345/388/410/410 全 spread=0 PASS,截图 `frames-edge/e28-0-home.png` 可见 1 行标题卡信息行与邻卡同位 |
## F. 竖屏全屏:播放页/底栏/标题栏误入全屏层且不可操作修复 ✅(真机复核待办)
症状(用户截图 2026-11-22):竖屏模式下点视频全屏后,窗口转全屏横屏,但播放页单列内容(播控条/简介/评论)、底栏胶囊、标题栏全部原样留在窗口里,只有全屏 HUD 悬浮其上——UI 框架「被全屏进视频区」且全部点不动(透明 HUD 根层盖住整窗,下层元素看得见够不着)。
根因(三处叠加):① `pages/player.rs` 单列判定 `is_single_col` 不含 `input.fullscreen`,竖屏全屏仍 early-return 单列树,桌面分支的「Stretch 播放面 + 其余 opacity 收起」路径从未生效;② shell mobile 分支全屏时标题栏(Auto 行)与底栏(胶囊)不收起(桌面分支有 0 高行收起,mobile 没有);③ v0.6.0 换 `hud_only` 后画布命中层(`hit_overlay`)丢失——HUD 自动隐藏后鼠标移动无法唤醒、画布双击无法退全屏,全屏交互只剩 HUD 条与快捷键。
| 项 | 内容 | 落点 / 证据 |
|---|---|---|
| F-1 单列全屏属性切换集 | 新纯函数 `single_column_fullscreen_attrs`:全屏时视频面高度改用真实全屏窗口高度(边距清零、圆角保 1 DIP 合成裁剪,见 F-5),根行翻 `[Star, 0]` 让简介/评论滚动区零高,播控条/页签零高+透明收起——树形/keyed 路径不变(MPE 不销毁,单列 early-return 换树红线不触碰);高度未同步(enter 首帧 0/NaN)退回布局高度不得 0 高 | `pages/player.rs`;单测 `single_column_fullscreen_attrs_stretch_video_and_collapse_rows`(全屏/普通/退化三态逐字段)、`single_column_transport_keeps_identical_kind_tree_across_fullscreen`(kind 序列恒等 + debug 属性翻转) |
| F-2 全屏真实高度跟踪 | 新 `AppShell.fullscreen_window_height`(与 `fullscreen_window_width` 同款):init/enter/exit 取 `viewport_layout.window_height`,`WindowResized` 全屏冻结分支同步真实高度,settled 收敛后重绘喂给 `PlayerInput.fullscreen_window_height` | `shell/mod.rs`、`shell/handlers/{window,player_ctl}.rs`、`shell/view/player_views.rs` |
| F-3 mobile 壳层全屏收起 | 标题栏行全屏 0 高(与桌面分支同款,TitleBar 无显式高度行高压得住);底栏整块条件摘除——胶囊显式高(`capsule::HEIGHT`)+ 玻璃 composition 视觉,0 高行压不住溢出绘制;positional key 仅尾位增删,前两位(标题栏/内容层,内含 keyed 播放页)路径不变 | `shell/mod.rs` mobile 分支;摘除依据:底栏子树含 GlassBar 组件,XAML opacity 压不住 hand-off 视觉 |
| F-4 全屏画布命中层恢复 | hud_only 模式补 `keyed("fs-hit", hit_overlay)` 全窗命中层(透明可命中,位于 HUD 条之下):鼠标移动唤醒 HUD、单击收放、双击退全屏;HUD 条内控件命中不受影响(条为兄弟层,事件不跨分支) | `pages/fullscreen.rs`;行为对齐 v0.6.0 之前 `fs-canvas` 首层语义 |
| F-5 全屏视频区白屏修复(F 节验收中引入回归后实测闭环) | 首版 F-1 把全屏包围 Border 圆角设 0 触发存量引擎坑:**铺满整个客户区 + 纯色 backdrop + 包围 Border 圆角 0 → MPE presenter 的 HWCompMediaNode 被 DirectComposition 丢弃** → 全屏后白屏(弹幕/HUD 正常;E21 UIA 全屏帧复现截图)。修复:单列全屏与桌面全屏宿主 Border 统一保 1 DIP 圆角裁剪(v0.4.0 全屏页同款 workaround,视觉不可见)。验收:E21 全屏帧视频正常;E10-native `fs_hud mean=90.9/sd=30.7`、退出恢复 `rest=129.8/sd=72.6`、二次全屏自隐全 PASS;白屏/黑屏由 `Measure-ImageRegionStats` 亮度断言钉死(mean≤15 或 sd<6 即红)。注:桌面全屏 corner 0 不触发(条件组合差异),单列树必触发;单测锁定圆角 1.0 不得回退 | `pages/player.rs`(`single_column_fullscreen_attrs` + 桌面 `floating_surface` 宿主);单测 `single_column_fullscreen_attrs_stretch_video_and_collapse_rows` |
## 验收实测(2026-11-21)

- `cargo test --workspace`:**273 passed, 0 failed, 9 ignored**(core 93 + fixtures 6 + login 7 + ui 131 + video 36;ui 较上轮净增 18:磁盘缓存 6 + paging 3 + 存储卡 3 + 并发 1 + 列表/守卫回归等)。
- selftest:`popular`(含首页在飞门控拦截 + 自动续页真实第 2 页增长)/ `storage`(真实封面落盘 → 清空归零 + 提示 + 幂等二次清)/ `user-lists` / `live` 全 PASS,0 panic。
- 自动化点击 `scripts/ui-edge.ps1 -Scenario E27`(新增,真实滚轮 + UIA):热门首屏 20 → 滚轮一轮自动续至 **100 个视频**;磁盘缓存 **109 条 .bin** 一键清空归零 + 「缓存已清理」InfoBar 出现;白名单验证:settings.json SHA256 前后一致、缓存目录内非 .bin 哨兵文件完好;进程无退出无 panic。截图 `target/ui-demo/frames-edge/e27-0/1-*.png`。

- 0.6.0.4 本轮复测:`cargo test --workspace` exit 0(ui bin **132 passed**,净增 `card_text_rows_are_title_length_independent` 1 项);selftest **18/18 PASS** 0 panic;`ui-edge -Scenario E28`(新增,UIA 几何断言)首行 5 卡 标题/UP/▶/弹幕 顶边 345/388/410/410 跨卡 spread **全 0** PASS,截图 `target/ui-demo/frames-edge/e28-0-home.png`。出包 `package/debug/QAQ-Bx-Dev_0.6.0.4_x64.appx`。
## 已知限制(诚实清单)

- 登录态三页(历史/稍后再看/收藏)自动续页实网路径依赖真实凭据,selftest 环境为访客门控;selftest 已埋 `history_base >= 20` 时的续页断言,待登录环境跑到。
- 搜索页仍固定第 1 页 20 条(搜索接口语义与列表不同,未纳入本轮)。
- 磁盘缓存逐出依据为文件 mtime(近似 LRU);NTFS 时间分辨率下同秒批量写入的相对顺序不保证严格。
- 清空缓存不清 crash.log/ui-trace.log(日志文件句柄常开,Windows 下删除会失败;且属诊断数据非缓存)。

## 前序修订摘要(0.5.0.x,已随包发布)

- **0.5.0.2**:悬停预览卡透明穿底修复——Fluent Card 刷本身半透明,覆盖层下无底色;`peek.rs` 双层结构(SolidBackground 不透明底 + Card  tint),单测 `peek_body_paints_opaque_base_under_card_tint`。
- **0.5.0.4**:✕ 关闭预览后全 session 无法再弹——会话级静默位改 `PeekDismissal` 单卡静默(离开原卡/换卡即恢复);selftest peek ②.5 + E23 真实鼠标回归通过。
## DM. 弹幕引擎官方判据移植:tz 时长模型 + 智能屏蔽复活 + 密度/区域/倍速参数化 ✅(0.6.0.5,待 just dev 出包)

> 依据 2026-11-22 官方网页播放器抓取研究(`docs/dev-notes/bilibili-danmaku-engine-research.md`,
> 完整笔记 `D:/QAQ-Work/bili-dm-engine/notes/RESEARCH.md`)。判据=官方 npd.491 逐行移植,
> 渲染管线(D2D)不变。

| 项 | 内容 | 落点 / 证据 |
|---|---|---|
| DM-1 智能屏蔽复活(P1 bug) | weight 字段号修复:官方 schema 与真实 seg.so(331 条 wire 扫描)一致 weight=field 9(varint),field 12 是 dmid 字符串(wire 2);旧实现 `(12,0)` 永不命中 → weight 恒 0,`danmaku_weight_min` 智能屏蔽自上线起静默失效 | `qaq-bx-core/src/danmaku.rs` parse_elem;新增 `weight_comes_from_field9_not_field12`/`pool_defaults_to_zero_when_absent`,重写 `parses_minimal_segment`(字段号=官方 schema) |
| DM-2 tz 时长模型 | 官方屏宽分段表+Logistic 移植(`roll_base_secs`):[558,888]→7-13s … (2314,2560]→14-20s、>2560→16-22s(+1s/500px)、<558 兜底 4.5s;常规文本≈段内 minTime(1920 屏≈13.1s,替代恒 ROLL_SECONDS=10);分配/渲染/窗口三处共用,速度=(屏宽+字宽)/时长×speedPlus | `qaq-bx-video/src/engine.rs` `roll_base_secs`/`roll_travel`/`roll_window_secs`;单测 `roll_base_secs_follows_official_brackets`(含边界取前一段语义锚定)/`roll_travel_and_window_consistent` |
| DM-3 官方准入几何 | 双侧 50px 跑道(屏外起笔/过左缘 50px 退场)+ 密度容忍 `A=floor(时长×(1-density)/2)+0.1`(官方 spaceManager 口径);渲染窗口改为 `roll_window_secs` 上界,窗口裁剪不漏条 | 同上;单测 `allocate_lanes_same_lane_never_overlaps` 重写为官方判据独立重算 |
| DM-4 density/area/speed_plus 参数化 | VideoConfig 增 `danmaku_density`(0.1..=1.0,默认 1.0)/`danmaku_area`(10..=100%,默认 100)/`danmaku_speed_plus`(0.5..=2.0,默认 1.0);area 离散化为可用轨道数(高度感知:矮窗口轨道随高度收缩);字幕池(pool=1)豁免 area=官方特权口径 | engine.rs allocate_lanes;单测 `allocate_lanes_density_tolerance_orders_admission`/`allocate_lanes_area_limits_and_subtitle_pool_exempt`/`allocate_lanes_height_aware_lane_count` |
| DM-5 pool 解析贯通 | DanmakuElem(field 11)/OverlayDanmaku/UI 转换全链路补 `pool` | core→ui handlers/danmaku.rs;既有分配测试迁移到 VideoConfig 签名 |

**验证**:`cargo test --workspace` 全绿(core 95+6 / login 7 / ui 134 / video 47,0 失败);
ui 侧仅转换字段追加,无行为变化。**已知限制**:band packing(官方 3-band 2D packing)
未实施,当前=官方参数×本项目离散轨道,P2 迭代再做。density/area/speed_plus 设置面板
接线与 preventShade 随 §DM2 落地。

## DM2. 弹幕设置面板接线:AppSettings 4 参数 → 设置页控件 → DanmakuStyle 单源热更新 + preventShade ✅(0.6.0.5,同 DM 包待 just dev 出包)

> 批次 2:把 §DM 已参数化但无 UI 的弹幕设置接通全链路——settings 持久化 → 设置页控件 →
> `DanmakuStyle` 单一参数源热更新 → engine/stage 两管线。

| 项 | 内容 | 落点 / 证据 |
|---|---|---|
| DM2-1 设置持久化 | AppSettings 增 `danmaku_density`(0.1..=1.0,默认 1.0)/`danmaku_area`(10..=100,默认 100)/`danmaku_speed_plus`(0.5..=2.0,默认 1.0)/`danmaku_prevent_shade`(默认 false);`normalized()` 钳位(非有限值回退默认),容器 `#[serde(default)]` 旧配置自动补齐 | `qaq-bx-core/src/settings.rs`;`danmaku_filter_defaults_for_legacy_configs_and_weight_clamped` 扩展(legacy 默认+越界钳位),roundtrip 补 4 字段 |
| DM2-2 DanmakuStyle 单一参数源 | 9 字段结构体 + `from_config`/`normalized`/`apply_to`;`EngineCmd::SetDanmakuConfig`/`StageCmd::SetStyle` 双通道;引擎线程 handler:normalized→字体变化重建 DanmakuFonts→apply_to→重跑 realloc(热更新不重启管线) | `qaq-bx-video/src/engine.rs`/`stage.rs`;单测 `danmaku_style_normalizes_out_of_range_values` |
| DM2-3 preventShade(防挡字幕) | 官方口径:碰撞可用高度×0.85(底部 15% 让位),300px 矮窗 9 轨→8 轨 | `engine.rs allocate_lanes`;单测 `allocate_lanes_prevent_shade_shrinks_usable_height` |
| DM2-4 设置页 4 控件 | 弹幕速度滑条(50-200%,步进 10)/显示区域(10-100%)/密度(10-100%)/防挡字幕 ToggleSwitch,插在「弹幕轨道上限」与屏蔽四选之间;automation_id `settings-danmaku-speed-plus/-area/-density/-prevent-shade`;`danmaku_style_changed` 判据扩展 4 字段 → `danmaku_config(danmaku_style_from(&next))` 热下发 | `qaq-bx-ui/src/pages/settings.rs`/`shell/handlers/settings.rs`/`shell/util.rs` |

**验证**:`cargo test --workspace` 全绿 **291**(core 95+6 / login 7 / ui 134 / video 47→49,
+2 本批单测);ui-edge 新场景 E29 PASS——UIA SetValue/TogglePattern 操作 4 控件 → UI 回读
生效 → settings.json 防抖落盘 `speed_plus=1.6/area=70/density=0.4/prevent_shade=true`,
种子非本批字段白名单逐字段完好(`scripts/ui-edge.ps1 -Scenario E29`);`just selftest all`
12/15 PASS——half-pane/fullscreen/loading-state 3 项在 HEAD 基线(不含本两批改动)以相同
签名失败(`engine_attached:false`,mpv 通道未挂载),A/B 鉴定为环境遗留,与本批无关。
**已知限制**:band packing(官方 3-band 2D packing)与跳变治理随 §DM3 落地。

## DM3. band packing:官方 3-band 连续 y 打包 + 跳变治理 ✅(0.6.0.5,待真机复核手感)

> P2:把 D1 离散轨道替换为官方 spaceManager.add 的 2D packing
> (`pretty/491-danmaku-b.js` @12570 全算法提炼,只取思想不搬运代码)。

| 项 | 内容 | 落点 / 证据 |
|---|---|---|
| DM3-1 连续 y 打包 | band 0 为常规占用列(占用区间按 bottom 有序),候选 y 自 offsetTop 走查:空隙(S.y>候选底)直接放入;y 区间重叠时按追尾双判据(a 入场时前条尾已过右缘 / b 本条头部到左缘时前条已退场+A)放行同 y,否则 v=S.bottom+1 连续下移(1px 步进,非离散轨道);行盒高=1.125×字号(官方 DOM 口径),小字号可填大字号下方空隙 | `qaq-bx-video/src/engine.rs` allocate_lanes 重写 + `OverlayDanmaku.y_off`;draw 侧滚动 y=y_off(替换 8+lane×30 栅格) |
| DM3-2 高度/area/特权 | g=min(preventShade?0.85:1 ×高, offsetTop+轨道上限×行距);非特权候选 bottom ≤ m=高×area/100,越界整条丢弃(官方口径:不试下一 band);字幕池(pool=1)特权豁免 area 且可溢出 band 1/2;溢出策略只有丢弃、无强插路径(官方口径),`Force` 保留为项目扩展(落走查最深处钳回可用高度);「弹幕轨道上限」改钉 g 上限(官方无此帽,项目保留语义) | 同上;单测 `allocate_lanes_overlapping_pairs_satisfy_tailgate`(重叠对⇒双判据独立重算)/`allocate_lanes_area_limits_and_subtitle_pool_exempt`(band 1 溢出)/`allocate_lanes_mixed_sizes_pack_continuous_y`(非栅格连续 y) |
| DM3-3 跳变治理 | realloc 增 playhead 参数:progress < 播放头的条目保留原 lane/y_off 不重排,并以其占用播种 band 列表与驻留槽——分段到货/过滤变化/样式热更/resize 不再引起可见弹幕跳位;严格 < 边界防 playhead=0 时零时刻弹幕漏打包;全量替换(新视频/换 P)传 NEG_INFINITY 全量重排 | engine.rs 5 处 + stage.rs 5 处 realloc 调用点;`playhead_estimate`(只读外推,不重锚);单测 `realloc_preserves_flying_y_and_packs_around_them`/`allocate_lanes_packs_zero_progress_items_at_playhead_zero` |

**验证**:`cargo test --workspace` 全绿 **294**(core 95+6 / login 7 / ui 134 /
video 49→52,+3,预存链增量==全量断言保留且 y_off 无漂移)。**已知限制**:混排字号
与追尾复用的密度观感待真机复核(官方更"贴",同 y 复用更激进;低密度档 A 变大后
同 y 复用更保守,与官方一致)。
