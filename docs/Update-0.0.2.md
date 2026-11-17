# Update 0.0.2 — 消灭占位数据:热门页与相关推荐接入线上真实数据

> 记录时间:2026-11-17。Cargo workspace 版本已升 `0.0.2`,`just dev` 打自更新包时末尾修订号自 `.1` 起递增。
> 目标:把仓库内剩余的 mock/占位样本数据全部替换为真实 Bilibili 线上接口数据。

## 新增

- **全局内敛搜索框（标题栏唯一入口）**:原「TextBox + 外置放大镜按钮」组合改为
  一体化 `Border` 壳(圆角/描边/填充)+ 无边框透明 `TextBox` + 内嵌放大镜
  `Button`；`PreviewKeyDown` 隧道事件挂在外壳上,Enter 按下即提交
  (主/小键盘 Enter 均认,`search_enter_submits` 纯函数判定 + 单测锁定;
  释放与其他键放行不拦截,避免与评论输入框等抢键)。搜索页删除页内
  「TextBox + 搜索按钮」重复入口,只留标题与提示行——发起与改词统一走
  全局框。窄屏(compact)全局框收窄至 260px。自动化走查新增 E8 场景:
  前台句柄硬校验(ALT 解锁前台锁重试)→ 点击标题栏输入区 → 剪贴板粘贴
  关键词(绕开中文 IME 组合态吞字母/吞 Enter)→ SendKeys {ENTER} →
  断言进程存活;截图实测 e10 草稿进框、e11 回车直达结果页(标题栏副标题
  同步「搜索」,页内无任何输入框)。
- **相关视频真实端点定案**(`bili-core::feed::FeedClient::related`):
  `/x/web-interface/archive/related?bvid=<BV>`(无签名、访客可用,实测 code=0);
  `data` 为 ArchiveCard **裸数组**(无 list/items 包裹层),形状与 popular/precious/ranking 同族。
  此前猜测的 `/x/web-interface/wbi/related/all` 实测返回 HTML,已在契约与文档中纠正。
- **契约锚扩展**:`api/openapi/bilibili-feed-v1.yaml` 新增 `archiveRelated` 路径;
  `api/fixtures/feed-related.json`(真机捕获截断样本)入 `feed_fixtures` 行为锚测试。
- **热门页真实数据**(`bili-ui`):删除 12 条假种子,改走 `FeedClient::popular(pn, 20)`
  真实分页;`PopularFeedState` 由 AppShell 单源持有——加载中/错误重试(失败不推进页号,
  「加载更多」即重取同页)/空页终态「没有更多了」;跨页 bvid 去重合并。
- **播放页右栏「接下来播放 · 相关推荐」真实数据**:随 `playback_gen` 播放代次拉取,
  代次 + 目标 bvid 双校验丢弃快速切视频的迟到响应;防御性过滤当前稿件自身;
  卡片渲染真实标题/UP 主/播放数/时长/封面(封面复用同一有界 LRU `ImageCache`,
  `FlushCovers` 兜底回填热门与推荐卡);点击进入与首页卡同一条 `open_video` 管线
  (新增 `related_index` 回退链)。
- **selftest 场景 8 `popular`**:导航进热门 → 断言真实分页数据落定 → 点开首卡
  (经 `popular_index`)进播放页 → Back 回热门;`comments` 场景追加相关推荐就绪断言
  (`related_items > 0 && related_bvid == TEST_BV`),全部走真实网络。
- **自动化点击走查**(`scripts/ui-edge.ps1`):新增 E6(热门页直启 + 点首卡)与
  E7(`BILIPLAY_VIDEO` 直启 + 点右栏相关推荐卡),进程存活即无 panic 门禁。
- **播放页简介内联展示**:弃用 `Expander`(声明式重建会把 `is_expanded(false)`
  强制刷回,用户展开后自动缩回;正文还重复内嵌标题)——改为「简介」标签 +
  自动换行 `TextBlock`,无状态、重建免疫,长文本随页面滚动;空/占位简介
  (含 API 返回 `"-"` 的契约情形)落「暂无简介」,末行保留 BV 号。
- **视频加载环形指示器**(Fluent `ProgressRing`,indeterminate):解析/缓冲期间
  居中叠加在视频面上(全播放页与半屏抽屉共用),配「视频加载中…」文案;
  状态单源 `AppShell::video_loading`——开播点亮,首个 `has_media` 熄灭,
  解析失败/引擎错误/关播同步熄灭,视图纯映射不残留。
- **selftest 场景 9 `half-pane`**:半屏抽屉挂载脉冲 + 加载指示完整生命周期
  (点亮 → 脉冲挂载 swapchain → 熄灭);settings 改为按场景写入隔离目录。

## 变更

- **修复:播放页右栏相关推荐卡片长短不一(高)**:卡片高此前由内容驱动——封面列定高
  74px,文本列(标题 1~2 行 × UP 主 × 播放量)随行数浮动,网格行取两列最大值,
  标题占满两行的卡比一行的卡高约半行。改为文本列三槽定高(标题恒定预留两行
  40px、元信息各 18px,常量与 `max_lines` 封顶同源),所有卡严格等高且
  UP 主/播放量基线对齐;封面垂直居中。回归单测
  `pages::player::tests::related_card_slots_are_content_independent`
  锁定「卡高与数据无关 + 定高槽容得下最坏字体行高(Noto Sans SC ≈1.48em)」。
- **修复:播放页右栏相关推荐卡片宽窄不一(宽,同项续修)**:卡片宽同样由内容驱动,
  实测同列卡宽 290~311px 参差且不满栏。右栏 ScrollViewer 先改
  `HorizontalScrollBarVisibility::Disabled`(消除无限宽量测与横向滚动),但
  arrange 仍按各卡自然宽;再为卡片 Button 与状态行定宽
  `RELATED_CARD_WIDTH = 360 − 4 − 16`(与栅格列/缘距严格同源),无横向滚动时
  presenter 按视口宽统一定宽。实机截图像素复测:所有卡外沿严格 316px、
  右缘一致 x=1259,标题同列宽换行、时长徽标对齐。单测追加宽度算术不变量
  (卡宽==栏视口算式;边框内衬 18 + 封列 132 + 间距 10 + 文本保底 120 ≤ 定宽)。
- **修复:冷启动/关播放条后首个视频黑屏**(半屏抽屉挂载脉冲门控):swapchain
  挂载重试此前只认 `route == Player`;HalfPane 模式点首页卡 `route` 合法停在
  Home,面板 metrics 抢在引擎产出 swapchain 前触发唯一一次挂载即错过,再无重试;
  `ClosePlayer` 丢弃引擎后重开新引擎亦复发。门控改为
  `route == Player || home_half_pane`(未挂载才脉冲),真机 E2E 复核出画正常。
- **许可证定案 MIT**:补 `LICENSE` 文件(与 workspace `license = "MIT"` 声明对齐)。
  同类第三方客户端主流为宽松许可(BBDown/YesPlayMusic=MIT、lx-music-desktop=Apache-2.0、
  yt-dlp=Unlicense),不向下游施加 copyleft 强制义务。
- **fixture 脱敏**:`api/fixtures/feed-rcmd.json` 内真实抓包残留的广告跳转链接
  (含第三方 `ug_apk_token` 与 `track_id`)替换为占位 URL;历史提交已验证
  从未包含凭据值。
- **测试夹具/文档 PII 脱敏**:真实账号 `mid`/用户名从 `account.rs`、`notify.rs`
  合成夹具与两份研究文档中移除,替换为占位值(公开仓库推送前置检查)。
- **`data.rs` 去 mock 化**:原「真实 feed 接入前的一手样本」12 条假视频与
  `all()/find()/popular()` 全部删除;`data.rs` 现仅保留展示模型 `VideoItem`,
  内容一律来自 bili-core 卡片族(`video_item`/`archive_video_item`/`search_video_item`)。
- **播放页互动计数真实化**:`VideoItem` 新增 `like_count/coin_count/favorite_count/share_count`
  (`Option<u64>`,卡片未下发时为 `None`,UI 只显示标签不编造数字);rcmd/归档卡直接透传
  `stat.like/coin/favorite/share`,`absorb_video_meta` 经 view 补齐缺失计数
  (含 `play_count/danmaku_count`);原 4 个写死胶囊「点赞 1.2万/投币 3,420/收藏 5,890/分享 转发」
  删除——实机验证:同一 UI 对 Rick Astley MV 显示 298.2万/129.6万/156.9万/50.8万,
  对另一热门稿件显示 1.1万/2894/2992/443,不再恒等于假样本值。
- `open_video` 数据源链:首页卡 → 搜索索引 → 热门索引 → 相关推荐索引 → `minimal_video_item`
  最小模型(真实标题/UP 主/aid/cid 由 `video_meta` 解析管线回填);
  `BILIPLAY_VIDEO=<bvid>` 冒烟路由不再依赖种子表,任意真实 bvid 可直达播放页。
- `router.rs`:`Route::Popular` 改由 shell 直接组装(状态共享),`route()` 仅分发音量空态页。
- 文档口径同步:`docs/bilibili-api-inventory.md` §6 相关视频端点定案;
  `docs/ui-design.md` 右栏/目录结构/热门页三处去"占位数据"表述;
  `api/README.md` fixtures 目录与行为口径补 related 条目。

## 测试与脚本

- `cargo test --workspace` 全量通过(`bili-core` 44+6 fixture(含新增 related 契约锚)、
  `bili-plugin-login` 7、`bili-ui` 30(新增 `archive_video_item_*`/`merge_popular_items_*`/
  `minimal_video_item_*`/`video_item_maps_optional_interaction_counts`/
  `desc_body_*`/`mount_pulse_*` 单测)、`bili-video` 1)。
- 实网冒烟:`cargo test -p bili-core -- --ignored live_related_smoke` 对 BV1GJ411x7h7
  通过(访客态 NoopCreds;断言卡非空、字段完整、不含当前稿件)。
- `just selftest all`:9 场景全绿(boot/nav/comments/rapid-switch/stress/search/codec/popular/
  half-pane,真实网络 + 隔离数据目录)。
- 简介内联与挂载脉冲回归:negative 验证(临时改回旧门控)确认 `half-pane`
  场景可捕获旧 bug 现场(`mount_tick:0, engine_attached:false`),修复后转绿。

## 范围外(维持空态/未接入,非 mock 数据)

- 关注/直播/收藏/历史/稍后再看:空态页(未登录或无对应只读端点接入计划),不含任何假数据。
- 评论写接口(发评/点赞)仍为只读;输入栏为 UI 控件。
- 搜索建议词(`search/default`)未接入。
