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