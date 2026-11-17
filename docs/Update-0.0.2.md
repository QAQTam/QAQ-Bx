# Update 0.0.2 — 消灭占位数据:热门页与相关推荐接入线上真实数据

> 记录时间:2026-11-17。Cargo workspace 版本 `0.0.1`,`just dev` 打包时末尾修订号自动递增。
> 目标:把仓库内剩余的 mock/占位样本数据全部替换为真实 Bilibili 线上接口数据。

## 新增

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

## 变更

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
  `bili-plugin-login` 7、`bili-ui` 27(新增 `archive_video_item_*`/`merge_popular_items_*`/
  `minimal_video_item_*`/`video_item_maps_optional_interaction_counts` 单测)、`bili-video` 1)。
- 实网冒烟:`cargo test -p bili-core -- --ignored live_related_smoke` 对 BV1GJ411x7h7
  通过(访客态 NoopCreds;断言卡非空、字段完整、不含当前稿件)。
- `just selftest all`:8 场景全绿(boot/nav/comments/rapid-switch/stress/search/codec/popular,
  真实网络 + 隔离数据目录)。

## 范围外(维持空态/未接入,非 mock 数据)

- 关注/直播/收藏/历史/稍后再看:空态页(未登录或无对应只读端点接入计划),不含任何假数据。
- 评论写接口(发评/点赞)仍为只读;输入栏为 UI 控件。
- 搜索建议词(`search/default`)未接入。
