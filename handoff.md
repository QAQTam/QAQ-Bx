# Handoff — mock 数据清零:热门页 + 相关推荐接入线上真实数据

> 记录时间:2026-11-17
> 工作区:`d:\QAQ-Bx`(基线版本 `0.0.1`,`just dev` 自更新末尾修订号)

---

## 1. 已完成(Completed)

### 1.1 `qaq-bx-core` 相关视频真实端点定案
- 新增 [feed.rs](crates/qaq-bx-core/src/feed.rs) `FeedClient::related(bvid)`:
  `GET https://api.bilibili.com/x/web-interface/archive/related?bvid=<BV>`(无签名,访客可用)。
  实测 BV1GJ411x7h7 `code=0`,`data` 为 **ArchiveCard 裸数组**(无 `list/items` 包裹,与
  popular/precious/ranking 同 schema);旧猜测 `wbi/related/all` 返回 HTML(不存在)。
- 契约面:`api/openapi/bilibili-feed-v1.yaml` 新增 `archiveRelated` 路径;
  新 fixture [feed-related.json](api/fixtures/feed-related.json)(真机截断样本,3 卡)。
- 测试:`feed_fixtures::related_fixture_matches_contract_v1`(契约锚)、
  `feed::related_tests::related_url_uses_archive_related_endpoint`(端点防回归)、
  `feed::related_tests::live_related_smoke`(`--ignored` 实网通过:非空/字段完整/不含自身 bvid)。

### 1.2 `qaq-bx-ui` 占位数据全部删除
- [data.rs](crates/qaq-bx-ui/src/data.rs):12 条假视频与 `seed/all/find/popular` 删除,
  仅保留展示模型 `VideoItem`;内容一律来自 `video_item(FeedCard)` /
  `archive_video_item(ArchiveCard)`(新增)/ `search_video_item(SearchVideo)`。
- `open_video` 数据源链:首页 → 搜索 → **热门索引** → **相关推荐索引** →
  `minimal_video_item`(真实标题/aid/cid/UP 主由 `video_meta` 解析管线回填);
  `open_video_target` 复用同一最小模型;`QAQ_BX_VIDEO=<bvid>` 冒烟不再依赖种子表。
- `ShellProbe` 新增 `popular_items/popular_first_bvid/related_items/related_bvid/popular_error`。

### 1.3 热门页真实数据([popular.rs](crates/qaq-bx-ui/src/pages/popular.rs))
- `PopularFeedState`(AppShell 单源,`next_pn` 从 **1** 起——实测 `pn=0` 返回 `-400`):
  `FeedClient::popular(pn, 20)` 真实分页;底栏状态机 加载中/重试(失败不推进页号)/
  「没有更多了」(空页终态)/「加载更多」;跨页 bvid 去重 `merge_popular_items`。
- 路由改造:`Route::Popular` 由 shell view 直接组装(共享状态),`router.rs` 仅剩空态页分发。

### 1.4 播放页右栏相关推荐真实数据([player.rs](crates/qaq-bx-ui/src/pages/player.rs))
- `RelatedPane`(bvid+loading+loaded_once+error+items)随 `playback_gen` 在
  `start_playback` 拉取;吸收端 代次+目标 bvid 双校验丢弃切视频迟到响应;防御性过滤自身 bvid。
- 卡片渲染真实 封面/标题/UP 主/播放数/时长,点击 → `on_open_video`(`Message::OpenVideo`,
  经 `related_index` 与首页卡同管线);加载中/失败重试/暂无三态齐备。
- 封面管线:`FlushCovers` 兜底回填扩展到 热门卡 与 相关推荐卡(同一有界 LRU)。
- **互动计数真实化**:播放页信息区原写死的「点赞 1.2万/投币 3,420/收藏 5,890/分享 转发」
  胶囊删除;`VideoItem` 增 `like/coin/favorite/share_count: Option<u64>`,
  rcmd/归档卡透传 + `absorb_video_meta` 经 view 回填(None 时只显示标签不编数字)。
  实机对比:Rick Astley MV 298.2万/129.6万/156.9万/50.8万 vs 另一稿件 1.1万/2894/2992/443。

### 1.5 测试与自动化
- `cargo test --workspace` exit 0:`qaq-bx-core` 44+6 fixture、`qaq-bx-plugin-login` 7、
  `qaq-bx-ui` 27(新增 `archive_video_item_*`/`merge_popular_items_*`/`minimal_video_item_*`/`video_item_maps_optional_interaction_counts`)、`qaq-bx-video` 1。
- `just selftest all`:**8 场景全绿**(新增 `popular`:导航→真实分页→点首卡→Back;
  `comments` 场景新增相关推荐就绪断言)。
- `scripts/ui-edge.ps1` 新增 E6(热门直启+点首卡)与 E7(`QAQ_BX_VIDEO` 直启+点右栏推荐卡),
  实机跑通 exit 0;截图证据:热门页 20 条真实视频、右栏真实相关推荐(含 2026-11-17 新鲜稿件)、
  点推荐卡打开 Rick Astley 官方 MV(真实弹幕流)无 panic。

---

## 2. 当前状态(Current)

- 改动文件(未 commit):`crates/qaq-bx-core/src/feed.rs`、`crates/qaq-bx-core/tests/feed_fixtures.rs`、
  `crates/qaq-bx-ui/src/{data.rs,shell.rs,router.rs,selftest.rs,pages/popular.rs,pages/player.rs}`、
  `api/fixtures/feed-related.json`(新)、`api/openapi/bilibili-feed-v1.yaml`、`api/README.md`、
  `docs/{bilibili-api-inventory.md,ui-design.md,Update-0.0.2.md(新)}`、`scripts/ui-edge.ps1`、`handoff.md`。
- `just dev` 自更新 AppX:v0.0.1.6(debug+自签)打包中/已完成(见本文件末尾追加记录)。

---

## 3. 待决事项(Pending)

1. **空态页(非 mock,维持现状)**:关注/直播/收藏/历史/稍后再看仍是空态文案,
   未接对应登录态端点;如需接入按 docs/bilibili-api-inventory.md §8 端点表推进。
2. **评论写接口**(发评/点赞)与搜索建议词(`search/default`)仍未接入。
3. 热门页「换一换」类刷新语义未做(API 为固定轮播分页,现仅 加载更多/重试)。
4. `just demo` 主脚本 8 场景未纳入热门/推荐点击(已由 `ui-edge.ps1` E6/E7 覆盖,
   若要把截图宣传片并入 promo 录制再迁一次)。

---

## 4. 下一步(Next Steps)

1. 若需登录态页面真实化:先 `qaq-bx-plugin-login` 走扫码 → 按 AGENTS.md #7 测试后清理令牌。
2. 版本号:用户未提版本提升,后续继续 `just dev` 自增末尾修订号;功能面大版本再开 `Update-0.1.0.md`。
