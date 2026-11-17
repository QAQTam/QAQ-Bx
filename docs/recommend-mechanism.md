# 首页推流机制定案（网页版复刻蓝本）

> 来源纪律（红线内）：站点自带 JS 切片（`index-bundle.js` 1.67MB，切片脚本
> `.qaqh/research/dig-feed-mechanism{,2}.mjs`，切片输出 `feed-mechanism-slices*.txt`）
> + CDP 真机流量（`api-log.txt`）+ Node 重放对照实验（`exp-dedup.mjs`/`exp-refresh-dedup.mjs`）。
> 零第三方文档。采样 2026-11-16。

## 1. 客户端状态机（Vue store `qs("feed")` 完整还原）

```
常量（bundle 实证）:
  Ac = 3                     # fetch_row 步进（= 12卡 / 4列）
  Q4 = 1 - Ac = -2           # fetch_row 初始值
  ak = 1000*60*60            # 1h 池过期阈值 = 60 分钟
  tb = 1                     # homepage_ver
  tn = {Init:0, Change:3, DropDown:4, Refresh:5}   # fresh_type 枚举 ★
  ht = {av:"av", ad:"ad", ogv_season:"ogv_season", live:"live", login:"login_card"}
  yl = {V4..V13, SEO_VIDEO, BANNER_VIDEO, V_NO_BANNER_3, ...}  # feed_version 池
  U4 = {xs:1060, sm:1140, md:1300, lg:1400, xl:1560, xxl:2060} # 列数断点

状态:
  fresh_idx      初始 -1，每次请求 +1（全局页游标）
  fresh_idx_1h   初始 -1，每次请求 +1；距上次请求 > 60min 时归零再 +1（1h 池重新进入）
  fetch_row      行游标（初始 -2）；首屏显式传 1；滚动页 +3
  brush          双计数器 {dropDown:-1, refresh:-1}：Refresh/Change 请求用 refresh++，
                 DropDown 用 dropDown++（本次请求的 brush = 对应类别的计数）
  fresh_type     本次触发类型（Init/Change/DropDown/Refresh）

ps 双档:
  Init / Change / Refresh → ps=10（domestic_*_ps=10, overseas_other_ps=15）
  DropDown（滚动）        → ps=12（domestic_*_ps=12, overseas_other_ps=15）

请求组装（除上述外）:
  y_num        当前布局列数（SSR 场景传 0）；实测桌面 1277px 宽 = 3
  last_y_num   上一次列数（cookie/布局缓存）
  screen       视口 "宽-高"
  device       UA 解析（"win"）
  feed_version 配置下发（默认 "V8"）
  uniq_id      window.abtest.uniq_page_id —— 每次页面加载生成的 AB 实验页 id
  last_showlist   已曝光卡（见 §2）
  last_clicklist  已点击卡（goto_aid 格式，窗口 50 条，slice(-50)）
```

## 2. 曝光反馈 last_showlist（`Af`/`sk` 函数定案）

```js
Af = cards => {
  // 按 freshIdx 分组：页内 ids.join(",")，页间 ";"
  // 条目 = 卡片的 reportId（曝光上报时生成/回填）
  // sk: feedReqCardList 只保留最近 10 个 freshIdx 的卡（窗口滚动）
}
```

- 条目格式：`av_<aid>`、`ad_<creative_id>`、`av_n_<aid>`（后缀 `_n` 由曝光事件动态拼接）。
- **对照实验结论**（重要修正）：滚动与刷新场景下，带/不带 `last_showlist`
  与首屏的重复率都是 **0**（各测 10 卡）——服务器内容轮换自带动；
  上一轮批量重放出现的 10 条重复是**短时间窗口内无上下文连发**命中同一池快照。
  → `last_showlist` 对客户端表现为**软约束/推荐反馈信号**，不是硬去重开关。
  复刻策略：照网页版窗口规则发送（贴近行为），但**去重责任在客户端本地**（bvid set）。

## 3. 混排楼层（客户端组装）

首页可视流 ≠ 单一接口，客户端把多来源卡拼成楼层：

| 来源 | 接口 | 备注 |
|---|---|---|
| 视频卡 | `/x/web-interface/wbi/index/top/feed/rcmd`（WBI） | 主流，goto=av |
| 番剧/影视卡 | `/x/web-interface/index/ogv/rcmd`（无签名） | `{request_cnt:5, brush:0, fresh_type:0, season_type:1..7}` 轮换，逐行注入，失败静默空 |
| 直播楼层 | floor 配置开关（`floor/switch?load_type=1`）+ season 映射 | goto=live 卡 |
| 漫画楼层 | `POST manga.bilibili.com/twirp/comic.v1.MainStation/Feed` `{page_num, page_size:14}` | ★ 新发现端点 |
| 广告卡 | rcmd 流内嵌（goto=ad，business_info 40+ 字段，pos_num 排位） | 客户端照常渲染（我们过滤） |

响应里还有 `floor_info`（楼层布局服务端参与）、`preload_expose_pct`（预曝光比例）、
`business_card`（广告卡配置）——骨架期可忽略。

## 4. 卡片→上报 ID 映射

- 曝光/点击上报 spm：`333.1007.tianma.card`；点击事件带
  `content_id/cid/track_id/creative_id/request_id/line_location/column_location/location/fresh_type/feed_version`。
- 卡片 id 取值：`card.id || business_info.src_id`（广告卡 id=0 时用 src_id）。
- WBI 细节（bundle 新证据）：密钥优先级 = 调用方传入 > **cookie `wbi_img_urls`** >
  bundle 内嵌混淆 key（`charCodeAt-1` 解混淆）；签名参数可带 `w_webid`（= `_render_data_.access_id`，
  SSR 注入；实测不带也能过 → 可选）。

## 5. QAQ-Bx 复刻蓝本（首页 feed 实现 spec）

```
FeedSession（每应用启动）:
  uniq_id = 随机生成（会话内不变）
  fresh_idx = 0, fresh_idx_1h = 0, last_feed_time = None
  seen_bvids: HashSet（本地去重）

next_page()  // 滚动加载
  if last_feed_time 距今 > 60min { fresh_idx_1h = 0 }
  fresh_idx += 1; fresh_idx_1h += 1; brush.dropDown += 1
  GET rcmd { fresh_type:4(DropDown), ps:12, fresh_idx, fresh_idx_1h,
             brush: brush.dropDown, y_num:3, fetch_row: 上次+3, feed_version:"V8",
             homepage_ver:1, last_y_num:上次列数, screen:"宽-高", device:"win",
             last_showlist: 按窗口规则构造（可选）, uniq_id }
  合并 av 卡（过滤 goto!=av），本地按 bvid 去重
  客户端插入 ogv 行（season_type 轮询 1,2,3,4,5,7）

refresh()  // 换一换/刷新
  fresh_idx = 0（重置）; brush.refresh += 1; seen_bvids.clear()（新流去重窗口清零 ★）
  fresh_type=5, ps=10, 其余同上
```

★ 去重窗口必须随换一换清零：`fresh_idx` 归 0 后服务端回吐的卡与首屏/已滚动
内容高度重叠，若沿用旧 `seen_bvids` 会被整批去重丢弃，而 UI 又无条件清空旧
列表 → 推荐页空白且后续预取同样被挡（回归用例 `feed.rs::refresh_clears_dedup_window`）。

契约参数语义更新：`api/openapi/bilibili-feed-v1.yaml` 的 rcmd 路径 description
已由本轮实验补充（fresh_type 枚举 / ps 双档 / 60min 池）——见 §7 更新记录。

## 6. 未决项

- `reportId` 生成端（本地/服务器）未追——不影响消费面（last_showlist 软约束）。
- 直播楼层接口细节（floor/switch 配置结构）未深挖——骨架期不做直播楼。
- 漫画楼层（manga twirp）未实测——列为 P3。
- ogv 注入的行距节奏（每 N 行一行 ogv）未精确还原——从两次抓包看 ≤1 行/页，
  实现期按「每页 1 行 ogv」起步，UI 观感调优。

## 7. 变更记录

- 2026-11-16：状态机还原 + 去重对照实验 + 机制文档建立。
