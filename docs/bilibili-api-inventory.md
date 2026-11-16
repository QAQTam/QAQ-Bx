# 哔哩哔哩 Web API 清单（CDP 实测挖掘）

> 全部来自 CDP 被动观察真实页面 + 主动重放验证（Node 复刻 Cookie/WBI 签名），零第三方文档。
> 测试视频：BV1GJ411x7h7（aid=80433022, cid=137649199），登录测试账号。
> 采样与证据：`.qaqh/research/api-log.txt`（84 条真实请求）、`.qaqh/research/samples/`（重放样本）。

## 0. 通用规则（实测）

- **鉴权**：Cookie 头（`SESSDATA` + `bili_jct` + `DedeUserID` + `buvid3`）即可表达登录态；多数只读接口未登录也能用。
- **WBI 签名**：URL 带 `w_rid`/`wts` 的接口必须签名。算法（§4，已验证：search 端 code=0）：
  `w_rid = md5(排序后query过滤!'()* + mixin32)`，`wts` 参与排序；mixin = nav 的 `wbi_img` 两文件名按固定置换表重排取前 32。
- **Referer**：视频类接口（playurl/弹幕）带 `Referer: https://www.bilibili.com/` 更稳（防盗链体系）。
- **UA**：务必携带浏览器同款 UA。
- **返回包**：`{code: 0, message, data}`；非 0 即错误（-400 参数、-403 权限/风控、-404 不存在、-101 未登录、-412 风控拦截）。

## 1. 视频信息 —— `GET /x/web-interface/view?bvid=<BV>` ✅ 实测 code=0

无签名、无 Cookie 要求（公开）。`data` 关键字段：
`bvid/aid/title/desc/duration/pic`（封面）、`owner{mid,name,face}`、
`stat{view,danmaku,reply,favorite,coin,share,like}`、
`pages[]`（**分 P 列表，每页含 `cid`** —— playurl/弹幕都靠它）、`cid`（默认 P）、`tname`（分区）。

## 2. 播放地址 —— `GET /x/player/wbi/playurl` ✅ 实测 code=0（fnval=16）

**必须 WBI 签名**。参数矩阵试验结论：

| 参数组合 | 结果 |
|---|---|
| `bvid&cid&qn=0&fnval=16&fnver=0` | **code=0，dash: video×10 + audio×3** |
| `fnval=1` | code=0（旧版 MP4 直链 durl） |
| `仅 bvid&cid&qn` | code=0（MP4 固定低清晰度） |
| `fnval=4044`（含 HDR/8K/杜比位） | **-400 请求错误**（多出的能力位会拒绝） |
| aid 替代 bvid | -400 |

可用参数：`bvid`、`cid`（来自 view.pages）、`qn=0`、`fnval=16`（纯 DASH）、`fnver=0`。
响应 `data.dash`：`video[]`（每项 `id`=清晰度代号(116=1080P60 等)、`codecs`、`bandwidth`、`base_url`/`backup_url`（**m4s 流，需带 Referer 请求**）、`size`）、`audio[]`；未登录/非大会员时 `dash.video` 里不含高清晰度档。另有 `support_formats[]`（清晰度描述）。

> web 端首屏播放信息由 HTML 内嵌（`window.__playinfo__`），DASH 全量下发后切清晰度不发新请求——客户端实现直接调本 API 即可。

## 3. 弹幕 ✅ 实测（protobuf）

- 元信息：`GET /x/v2/dm/web/view?type=1&oid=<cid>&pid=<aid>&duration=<秒>`（**WBI 签名**）
  → `data` 含弹幕总数、分段大小（6 分钟/段）、分段数。
- 分段内容：`GET /x/v2/dm/wbi/web/seg.so?type=1&oid=<cid>&pid=<aid>&segment_index=<1..N>&pull_mode=1`（**WBI 签名**）
  → **protobuf 二进制**（实测 338KB/段，magic `0a…`）。结构为 DmSegMobileReply 反复嵌套
  `DmSegMobileReply.elems[]`：`id/progress(毫秒)/mode(1滚动 4底部 5顶部)/fontsize/color/content/mid`。
  Rust 解析建议 `prost` 自定义 message，或先手写字段探测。

## 4. 评论 ✅ 实测 code=0（replies=20）

- **可用**：`GET /x/v2/reply/main?type=1&oid=<aid>&mode=3`（**无需 WBI**；带 Cookie）
  → `data.replies[]`（`rpid/member.uname/content.message/like/rcount`）+ `data.cursor`（`pagination_reply.next_offset` 翻页）
- `GET /x/v2/reply?type=1&oid=<aid>&pn=1&ps=20&sort=1`（旧版分页）→ code=0
- ⚠️ `/x/v2/reply/wbi/main`（页面现用版）实测一律 `-403`：有独立风控（疑似校验
  `dm_img_*` 设备指纹参数，见下），老接口 `reply/main` 完全够用。
- 页面真实请求里出现的指纹参数（player/wbi/v2 等接口也带）：
  `dm_img_list=[]`、`dm_img_str=<base64(OpenGL 字符串)>`、`dm_cover_img_str=<base64(渲染器)>`、
  `dm_img_inter={"ds":[],"wh":[x,y,z],"of":[x,y,z]}` —— 风控指纹，wbi/main 可能强校验。

## 5. 搜索 ✅ 实测 code=0（WBI 签名成立的决定性验证）

`GET /x/web-interface/wbi/search/all/v2?keyword=<词>&page=1&page_size=20`（**WBI 签名** + Cookie）
→ `data.result[]` 聚合多类型（`result_type: video|bili_user|…`），video 项含 `bvid/title(带<em>高亮)/author/duration/pubdate/play/danmaku`。
另：`GET /x/web-interface/wbi/search/default`（搜索框默认词）。

> 搜索接口历史上强依赖 `buvid3`（无 buvid 时被风控）——重放时已带 buvid3。

## 6. 首页推荐 ✅（批量实测 2026-11-16，契约见 api/openapi/bilibili-feed-v1.yaml）

`GET /x/web-interface/wbi/index/top/feed/rcmd`（**WBI 签名** + Cookie）：

- **无必需参数**（仅 `ps=12` 也 code=0；连 `fresh_idx` 都可省）。页面携带的
  `y_num/last_showlist/uniq_id/x-bili-device-req-json/x-bili-locale-json/screen/fetch_row`
  等全是可选业务上下文。
- 翻页锚：`fresh_idx`/`fresh_idx_1h` 递增 + `brush` 计数（1..4 实测逐页成功）。
- **翻页跨页有重复**（实测 48 项 38 唯一）→ 客户端去重。
- 卡片多态：`goto = av | ad | live`（首页流内嵌广告/直播卡）；
  ad 卡 `id` 可为 0 且无 `bvid`，带 40+ 字段的 `business_info`。
- **ID 已溢出 u32**（实测 `117619373907551`）——Rust 侧一律 u64。
- `data.item[]` av 卡核心：`id/bvid/cid/title/pic/duration/pubdate/owner{mid,name,face}/stat{view,danmaku,like,vt}/goto`。
- 番剧区另有 `GET /x/web-interface/index/ogv/rcmd`（见下）。

`GET /x/web-interface/index/ogv/rcmd`（无签名）—— `season_type` 实测全集 `{1,2,3,4,5,7}`
（番剧/电影/纪录片/国创/电视剧/综艺）→ `data.items[]`：`epid/season_id/title/cover/stat/owner/season_type`。
**实测口径坑**：`pub_time` 是字符串日期（"2017-09-22 14:21:29"）；`goto="ogv_season"`；
`trackid` 字符串、`ogv_creative_id` 数值（类型两态，不进消费面）。

`GET /x/web-interface/wbi/related/all?bvid=&aid=` → **返回 HTML（端点不存在）**，
播放页"相关视频"的真实端点未定（待从播放页流量抓取）。

## 7. 热门/榜单 ✅（批量实测 2026-11-16）

- `GET /x/web-interface/popular?ps=20&pn=1..2`（无签名）→ `data.list[]`（完整 archive 结构，
  比 rcmd 富：`rights/dimension/desc/tname/videos` 等）。
- `GET /x/web-interface/popular/precious?page=1`（入站必刷）→ 一次全量 98 条。
- `GET /x/web-interface/ranking/v2?rid=0&type=all`（排行榜）→ 一次全量 100 条，卡带 `score`。
- 三者结构一致（ArchiveCard 族），与 rcmd 卡不同 schema —— 契约分立：`FeedCard` vs `ArchiveCard`。

## 8. 其他捕获到的真实端点（按需实现）

| 端点 | 用途 | 签名/鉴权 |
|---|---|---|
| `GET /x/player/wbi/v2?aid&cid&dm_img_*` | 播放器配置（弹幕开关/推荐卡/字幕开关） | WBI |
| `GET /x/v2/subtitle/web/view?oid&pid` | 字幕列表（CC） | WBI |
| `GET /x/player/online/total?aid&cid&bvid` | 实时在线人数 | 无 |
| `GET /x/web-interface/archive/relation?bvid` | 点赞/投币/收藏状态 | Cookie |
| `GET /x/web-interface/history/continuation` | 续播（上次看到哪） | Cookie |
| `POST /x/click-interface/web/heartbeat?w_*&w_rid&wts` | 播放心跳上报（`w_*` 参数也参与 WBI 签名） | WBI + Cookie |
| `POST /x/click-interface/click/web/h5` | 播放开始上报 | WBI + Cookie |
| `GET /x/web-interface/dynamic/entrance` | 动态入口红点 | Cookie |
| `GET /x/relation/relations?fids=` | 关注状态 | Cookie |
| `GET /x/v2/subtitle/…`、`/x/note/*`（笔记）、`/x/sunflower/*` | 低优先 | — |
| `GET /x/kv-frontend/namespace/data?appKey=333.1339&…` | 页面配置下发（与播放无关） | 无 |
| `api.live.bilibili.com/xlive/…`、`api.vc.bilibili.com/…`（私信未读） | 直播/私信 | Cookie |

风控上报类（可忽略或后续研究）：`/x/internal/gaia-gateway/ExClimbWuzhi|ExClimbCongLing|ExGetAxe`（设备指纹上报，POST）。

## 9. 第三方客户端最小实现路线（bili-plugin-parse）

1. **视频详情**：`view`（无签名）→ 拿 `cid/title/stat/pages`
2. **播放**：`playurl(bvid,cid,fnval=16)`（WBI）→ DASH m4s + 音频分离（播放器需 demux，如 `mp4`/`fmp4` 处理或对接 ffmpeg）
3. **弹幕**：`dm/web/view` + `seg.so`（WBI）→ protobuf 解析（prost）
4. **评论**：`reply/main`（无签名）→ cursor 翻页
5. **搜索/首页/热门**：search/rcmd/popular
6. **WBI 签名模块**（bili-core）：nav → wbi_img → mixin → md5；Node 参考实现见 `.qaqh/research/replay-apis.mjs`（已验证）

## 10. 本次实测速记

- `search/all/v2` 用原生签名重放 → code=0（WBI 算法复现正确性定案）
- **Rust 侧 WBI 签名 2026-11-16 真机通过**（`bili-core::wbi` + `biliplay feed`：rcmd code=0）
- `dm/seg.so` 338KB protobuf，`0a…` 起始（len-delim field 1 = 弹幕元素数组）
- `playurl` 的 `fnval=4044` 拒绝、`fnval=16` 通过 —— 客户端按需请求 DASH
- `reply/wbi/main` 风控（-403），`reply/main`（无签名）可用
- 视频页首屏播放信息内嵌 HTML（`__playinfo__`），切清晰度不发 playurl（DASH 全量）

## 11. 商单视频标识挖掘（2026-11-16，结论：web 数据面不可区分）

Ground truth：`BV1amaH6aE2m`（用户确认的商单视频；手机 App 显示火箭🚀角标）。

| 数据面 | 结果 |
|---|---|
| view / view detail / player wbi/v2 | `rights/state/argue_info/desc_v2` 与普通视频无差异，无商业字段 |
| 播放页 HTML（桌面+手机 UA）+ m.bilibili.com | 无"商业推广/声明"文本 |
| search/all/v2 卡 | `corner/card_status/rec_reason/biz_data` 全空；仅 `copyright=3`（转载属性，非标记） |
| rcmd feed 卡（81 张） | 无任何商业字段 |

- **手机端火箭角标只在 App 专属数据面**：`app.bilibili.com/x/v2/view`（gRPC
  `bilibili.app.archive.v1.View`，需 appkey+sign，实测 web cookie 调用 -400）。
  推测角标为服务器下发图片（web popular 卡存在 `rcmd_reason.corner_mark.img_url`
  同类机制），非客户端硬编码。
- APK（iBiliPlayer，36 dex）字符串池全量提取（`.qaqh/research/dex-strings.mjs` →
  `apk-model-strings.txt`）证据：`argue_icon/argue_msg/argue_uri` = 播放页声明条体系
  （该视频未用）；arsc 中"商业推广/创作声明"文案全部为**投稿侧**（花火平台流程，
  `activity://uper/commercial_declare`）；proto rights 为位掩码无字段名常量。
- **biliplay 决策**：不区分（也无法区分）。影响面为零——广告卡已过滤（`goto=ad`），
  商单视频本身可正常播放，角标位置我们不渲染。
- 复用资产：`dex-strings.mjs` 可提取任意 APK 字符串池，后续挖 app 机制可复用。
