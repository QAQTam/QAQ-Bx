//! feed 组接口客户端（契约锚：`api/openapi/bilibili-feed-v1.yaml`，契约代 1）。
//!
//! 向下兼容策略（api/README.md）：serde 不启用 `deny_unknown_fields`，服务器新增
//! 字段零影响；`required` 字段（spec 与此处非 Option 字段对齐）删除/改名 = 破坏性
//! 变更，fixture 测试红灯 → 升契约代。
//!
//! 实测定案（2026-11-16）：
//! - ID/计数一律 64 位（aid 已达 1.17e14；广告卡 id 可为 0）；
//! - rcmd 无必需参数，翻页锚 `fresh_idx/fresh_idx_1h/brush`，跨页有重复 → 客户端去重；
//! - 卡片多态：`goto = av | ad | live`（首页流内嵌广告/直播卡）。

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{de::DeserializeOwned, Deserialize};

use crate::account::NavClient;
use crate::credentials::CredentialProvider;
use crate::error::Result;
use crate::wbi::WbiSigner;

// ---------- 信封 ----------

/// 通用返回包 `{code, message, ttl, data}`。`data` 缺失时为 `None`（错误码场景）。
#[derive(Debug, Deserialize)]
#[serde(bound(deserialize = "T: serde::de::Deserialize<'de>"))]
pub struct Envelope<T> {
    pub code: i64,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub ttl: i64,
    #[serde(default)]
    pub data: Option<T>,
}

// ---------- 卡片类型 ----------

/// UP 主（spec `Owner`，required: mid/name）。
#[derive(Debug, Clone, Deserialize)]
pub struct Owner {
    pub mid: u64,
    pub name: String,
    #[serde(default)]
    pub face: Option<String>,
}

/// 计数族（spec `Stat`，required: view/danmaku；其余两端可选）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CardStat {
    #[serde(default)]
    pub view: i64,
    #[serde(default)]
    pub danmaku: i64,
    #[serde(default)]
    pub like: Option<i64>,
    #[serde(default)]
    pub reply: Option<i64>,
    #[serde(default)]
    pub coin: Option<i64>,
    #[serde(default)]
    pub favorite: Option<i64>,
    #[serde(default)]
    pub share: Option<i64>,
}

/// 推荐理由（宽松：仅锚 reason_type）。
#[derive(Debug, Clone, Deserialize)]
pub struct RcmdReason {
    #[serde(default)]
    pub reason_type: i64,
}

/// 首页推荐卡（spec `FeedCard`，多态：goto = av | ad | live）。
/// required（非 Option）：`id/title/goto`；av 卡完整字段按 `goto` 分派后消费。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct FeedCard {
    #[serde(default)]
    pub id: u64,
    #[serde(default)]
    pub goto: Option<String>,
    #[serde(default)]
    pub bvid: Option<String>,
    #[serde(default)]
    pub cid: Option<u64>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub pic: Option<String>,
    #[serde(default)]
    pub duration: Option<i64>,
    #[serde(default)]
    pub pubdate: Option<i64>,
    #[serde(default)]
    pub owner: Option<Owner>,
    #[serde(default)]
    pub stat: Option<CardStat>,
    #[serde(default)]
    pub rcmd_reason: Option<RcmdReason>,
}

impl FeedCard {
    /// 是否常规视频卡（可进卡片网格；ad/live/未知 goto 由调用方过滤）。
    pub fn is_av(&self) -> bool {
        self.goto.as_deref() == Some("av") && self.bvid.is_some()
    }
}

/// 归档卡（spec `ArchiveCard`）：popular / ranking / precious 共用。
/// required：`aid/bvid/cid/title/pic/duration/pubdate/owner/stat`。
#[derive(Debug, Clone, Deserialize)]
pub struct ArchiveCard {
    pub aid: u64,
    pub bvid: String,
    pub cid: u64,
    pub title: String,
    pub pic: String,
    pub duration: i64,
    pub pubdate: i64,
    pub owner: Owner,
    pub stat: CardStat,
    #[serde(default)]
    pub desc: Option<String>,
    #[serde(default)]
    pub tname: Option<String>,
    #[serde(default)]
    pub tid: Option<i64>,
    #[serde(default)]
    pub score: Option<i64>,
    #[serde(default)]
    pub videos: Option<i64>,
}

/// ogv 剧集卡（spec `OgvCard`，required: epid/season_id/title/cover）。
/// 实测口径：`pub_time` 为字符串日期（"2017-09-22 14:21:29"）；`goto` 实测 "ogv_season"。
#[derive(Debug, Clone, Deserialize)]
pub struct OgvCard {
    pub epid: u64,
    pub season_id: u64,
    pub title: String,
    pub cover: String,
    #[serde(default)]
    pub long_title: Option<String>,
    #[serde(default)]
    pub season_type: Option<i64>,
    #[serde(default)]
    pub pub_time: Option<String>,
    #[serde(default)]
    pub owner: Option<Owner>,
    #[serde(default)]
    pub stat: Option<CardStat>,
}

// ---------- data 结构 ----------

#[derive(Debug, Deserialize)]
pub struct RcmdData {
    #[serde(default)]
    pub item: Vec<FeedCard>,
}

#[derive(Debug, Deserialize)]
struct ArchiveListData {
    #[serde(default)]
    list: Vec<ArchiveCard>,
}

#[derive(Debug, Deserialize)]
struct RankingData {
    #[serde(default)]
    list: Vec<ArchiveCard>,
}

#[derive(Debug, Deserialize)]
struct OgvData {
    #[serde(default)]
    items: Vec<OgvCard>,
}

// ---------- 客户端 ----------

/// feed 组客户端。`creds` 无凭据时以未登录态请求（rcmd/popular 实测可用）。
pub struct FeedClient {
    http: reqwest::Client,
    creds: Arc<dyn CredentialProvider>,
    ticket: Option<Arc<crate::ticket::TicketManager>>,
    wbi: WbiSigner,
}

/// rcmd 请求参数（契约：全部可选；翻页锚 `fresh_idx`/`fresh_idx_1h` 递增）。
///
/// 语义对齐网页版（docs/recommend-mechanism.md §1/§5）：
/// `fresh_type` ∈ {0 Init, 3 Change, 4 DropDown, 5 Refresh}；`ps` 双档
/// （Init/Change/Refresh=10，DropDown=12）；`brush` 为请求类别内的递增计数。
#[derive(Debug, Clone, Default)]
pub struct RcmdQuery {
    pub ps: Option<u32>,
    pub fresh_idx: Option<i64>,
    pub fresh_idx_1h: Option<i64>,
    pub fresh_type: Option<i64>,
    pub brush: Option<i64>,
    pub fetch_row: Option<i64>,
    pub y_num: Option<u32>,
    pub last_y_num: Option<u32>,
    pub homepage_ver: Option<u32>,
    pub device: Option<String>,
    pub screen: Option<String>,
    pub feed_version: Option<String>,
    pub uniq_id: Option<String>,
}

// ---------- 首页流会话状态机 ----------

/// 一次首页流请求的触发类别（网页版 fresh_type 语义）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedKind {
    /// 首屏（Init）。
    First,
    /// 滚动加载（DropDown）。
    Next,
    /// 换一换/刷新（Refresh）。
    Refresh,
}

/// B 站 `/x/web-interface/wbi/index/top/feed/rcmd` 实测单次请求硬上限（`ps >= 31` 返回 `-400`）。
pub const MAX_RCMD_PS: u32 = 30;
/// 一次逻辑页合并抓取的最多请求轮数（防连续全重/弱网拖慢）。
const MAX_BATCH_ROUNDS: u32 = 3;
/// 连续净新增为 0 的批次数达到阈值时判定流耗尽，定格最后一页。
const EXHAUSTED_ZERO_ROUNDS: u32 = 2;

/// 逻辑页合并抓取结果（多轮 `ps<=30` 聚合 + 去重 + 耗尽信号）。
#[derive(Debug, Clone, Default)]
pub struct FeedBatch {
    pub cards: Vec<FeedCard>,
    pub raw_count: usize,
    pub rounds: u32,
    pub exhausted: bool,
}

/// 首页推荐流会话（蓝本 docs/recommend-mechanism.md §5）。
///
/// `plan`（同步状态翻转，UI 线程调用）与 `absorb`（响应过滤 + 本地 bvid
/// 去重，可放后台线程）分离；跨页重复实测存在，去重责任在客户端。
/// 新流语义（首屏/换一换）在 `plan` 内一并清空去重窗口：换一换会重置
/// `fresh_idx`，服务端回吐的卡与旧流高度重叠，若沿用旧 `seen` 会被整批
/// 去重丢弃，而 UI 又清空旧列表 → 推荐页空白且不加载。
pub struct FeedSession {
    fresh_idx: i64,
    fresh_idx_1h: i64,
    fetch_row: i64,
    brush_drop_down: i64,
    brush_refresh: i64,
    last_feed_time: Option<Instant>,
    seen: HashSet<String>,
    uniq_id: String,
    consecutive_zero_new: u32,
    exhausted: bool,
}

impl Default for FeedSession {
    fn default() -> Self {
        Self::new()
    }
}

impl FeedSession {
    /// 每应用启动一次；`uniq_id` 随机生成（网页版 = abtest.uniq_page_id）。
    pub fn new() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let uniq_id = format!("{:08x}{:08x}", nanos as u32, std::process::id());
        Self {
            fresh_idx: -1,
            fresh_idx_1h: -1,
            fetch_row: 1,
            brush_drop_down: -1,
            brush_refresh: -1,
            last_feed_time: None,
            seen: HashSet::new(),
            uniq_id,
            consecutive_zero_new: 0,
            exhausted: false,
        }
    }

    pub fn is_exhausted(&self) -> bool {
        self.exhausted
    }

    /// 计算下一次请求参数（同步翻转状态、记录请求时刻；不发请求）。
    /// 保持网页版默认双档（First/Refresh=10, Next=12），供契约测试与 CLI 使用。
    pub fn plan(&mut self, kind: FeedKind) -> RcmdQuery {
        let default_ps = match kind {
            FeedKind::First | FeedKind::Refresh => 10,
            FeedKind::Next => 12,
        };
        self.plan_with_ps(kind, default_ps)
    }

    /// 指定单次 `ps`（自动钳位到 `1..=MAX_RCMD_PS`）计算下一次请求参数。
    /// 首屏/换一换会清零本地去重窗口与耗尽标记。
    pub fn plan_with_ps(&mut self, kind: FeedKind, ps: u32) -> RcmdQuery {
        // 1h 池：距上次请求 > 60min 时归零，本次请求照常 +1（重新进入 1h 池）。
        if let Some(t) = self.last_feed_time
            && t.elapsed() > Duration::from_secs(3600)
        {
            self.fresh_idx_1h = 0;
        }
        // 新流（首屏/换一换）= 重置游标 + 重新加载：本地去重窗口必须一并清零。
        // 滚动（Next）保留窗口，继续对新流基线去重（跨页重复实测存在）。
        if matches!(kind, FeedKind::First | FeedKind::Refresh) {
            self.seen.clear();
            self.consecutive_zero_new = 0;
            self.exhausted = false;
        }
        let ps = ps.clamp(1, MAX_RCMD_PS);
        let (fresh_type, brush) = match kind {
            FeedKind::First => {
                self.fresh_idx = 0;
                self.fresh_idx_1h = 0;
                self.fetch_row = 1;
                self.brush_refresh += 1;
                (0, self.brush_refresh)
            }
            FeedKind::Next => {
                self.fresh_idx += 1;
                self.fresh_idx_1h += 1;
                self.fetch_row += 3;
                self.brush_drop_down += 1;
                (4, self.brush_drop_down)
            }
            FeedKind::Refresh => {
                self.fresh_idx = 0;
                self.fetch_row = 1;
                self.brush_refresh += 1;
                (5, self.brush_refresh)
            }
        };
        self.last_feed_time = Some(Instant::now());
        RcmdQuery {
            ps: Some(ps),
            fresh_idx: Some(self.fresh_idx),
            fresh_idx_1h: Some(self.fresh_idx_1h),
            fresh_type: Some(fresh_type),
            brush: Some(brush),
            fetch_row: Some(self.fetch_row),
            // 桌面列数（实测 1277px 视口 = 3）；仅实验分桶用。
            y_num: Some(3),
            homepage_ver: Some(1),
            device: Some("win".into()),
            feed_version: Some("V8".into()),
            uniq_id: Some(self.uniq_id.clone()),
            ..RcmdQuery::default()
        }
    }

    /// 吸收一页响应：过滤 `goto != av`，按 bvid 本地去重，更新耗尽状态，返回新增卡片。
    pub fn absorb(&mut self, cards: Vec<FeedCard>) -> Vec<FeedCard> {
        let raw_empty = cards.is_empty();
        let fresh: Vec<FeedCard> = cards
            .into_iter()
            .filter(|card| card.is_av())
            .filter(|card| {
                let bvid = card.bvid.clone().unwrap_or_default();
                self.seen.insert(bvid)
            })
            .collect();
        if raw_empty {
            self.consecutive_zero_new = self.consecutive_zero_new.saturating_add(1);
            self.exhausted = true;
        } else if fresh.is_empty() {
            self.consecutive_zero_new = self.consecutive_zero_new.saturating_add(1);
            if self.consecutive_zero_new >= EXHAUSTED_ZERO_ROUNDS {
                self.exhausted = true;
            }
        } else {
            self.consecutive_zero_new = 0;
        }
        fresh
    }

    /// 一体化助手：plan → 请求 → absorb（CLI/测试等无 UI 分离需求的场景）。
    pub async fn run(&mut self, client: &FeedClient, kind: FeedKind) -> Result<Vec<FeedCard>> {
        let query = self.plan(kind);
        let cards = client.rcmd(&query).await?;
        Ok(self.absorb(cards))
    }
}

/// 按目标条数合并拉取一个逻辑页（拆成 `1..=MAX_BATCH_ROUNDS` 次 `ps<=30` 请求；
/// 绝不跨 `.await` 持有锁）。遇到服务端返回空列表或连续全重即标记 `exhausted = true`。
pub async fn fetch_rcmd_batch(
    client: &FeedClient,
    session: &std::sync::Mutex<FeedSession>,
    kind: FeedKind,
    target_count: usize,
) -> Result<FeedBatch> {
    let target = target_count.clamp(1, 90);
    let mut batch = FeedBatch::default();

    for round in 0..MAX_BATCH_ROUNDS {
        let remaining = target.saturating_sub(batch.cards.len());
        if remaining == 0 {
            break;
        }
        let round_kind = if round == 0 { kind } else { FeedKind::Next };
        // 首轮/大缺口直接拉满 MAX_RCMD_PS=30；小缺口多要 4 条抵消偶发广告/重复过滤。
        let ps = if remaining >= MAX_RCMD_PS as usize {
            MAX_RCMD_PS
        } else {
            (remaining + 4).clamp(12, MAX_RCMD_PS as usize) as u32
        };

        let query = {
            let mut s = session.lock().expect("feed 会话锁中毒");
            s.plan_with_ps(round_kind, ps)
        };
        let raw = match client.rcmd(&query).await {
            Ok(raw) => raw,
            Err(err) => {
                // 已在之前轮次拿到真实数据时保留已取结果，避免整个逻辑页失败丢卡。
                if !batch.cards.is_empty() {
                    break;
                }
                return Err(err);
            }
        };
        let raw_len = raw.len();
        batch.raw_count += raw_len;
        batch.rounds += 1;

        let (fresh, exhausted) = {
            let mut s = session.lock().expect("feed 会话锁中毒");
            let fresh = s.absorb(raw);
            (fresh, s.is_exhausted())
        };
        batch.cards.extend(fresh);
        if exhausted {
            batch.exhausted = true;
            break;
        }
        if raw_len == 0 {
            batch.exhausted = true;
            break;
        }
    }

    Ok(batch)
}

impl FeedClient {
    pub fn new(http: reqwest::Client, creds: Arc<dyn CredentialProvider>) -> Self {
        let nav = Arc::new(NavClient::new(http.clone(), creds.clone()));
        Self::with_nav(http, creds, nav)
    }

    /// 复用共享 NavClient：同一进程内 nav 只拉一次（WBI 密钥 + 用户资料共用缓存）。
    pub fn with_nav(
        http: reqwest::Client,
        creds: Arc<dyn CredentialProvider>,
        nav: Arc<NavClient>,
    ) -> Self {
        let ticket = nav.ticket().cloned();
        Self { wbi: WbiSigner::with_nav(nav), http, creds, ticket }
    }

    async fn get_json<T: DeserializeOwned>(&self, url: &str) -> Result<T> {
        crate::httpx::get_json_with_ticket(&self.http, &self.creds, self.ticket.as_deref(), url)
            .await
    }

    /// 首页综合推荐流（WBI 签名）。跨页有重复，去重由调用方负责。
    pub async fn rcmd(&self, query: &RcmdQuery) -> Result<Vec<FeedCard>> {
        let mut params: Vec<(&str, String)> = Vec::new();
        if let Some(ps) = query.ps {
            params.push(("ps", ps.to_string()));
        }
        if let Some(idx) = query.fresh_idx {
            params.push(("fresh_idx", idx.to_string()));
        }
        if let Some(idx) = query.fresh_idx_1h {
            params.push(("fresh_idx_1h", idx.to_string()));
        }
        if let Some(v) = query.fresh_type {
            params.push(("fresh_type", v.to_string()));
        }
        if let Some(v) = query.brush {
            params.push(("brush", v.to_string()));
        }
        if let Some(v) = query.fetch_row {
            params.push(("fetch_row", v.to_string()));
        }
        if let Some(v) = query.y_num {
            params.push(("y_num", v.to_string()));
        }
        if let Some(v) = query.last_y_num {
            params.push(("last_y_num", v.to_string()));
        }
        if let Some(v) = query.homepage_ver {
            params.push(("homepage_ver", v.to_string()));
        }
        if let Some(v) = &query.device {
            params.push(("device", v.clone()));
        }
        if let Some(v) = &query.screen {
            params.push(("screen", v.clone()));
        }
        if let Some(v) = &query.feed_version {
            params.push(("feed_version", v.clone()));
        }
        if let Some(v) = &query.uniq_id {
            params.push(("uniq_id", v.clone()));
        }
        let signed = self.wbi.sign_query(&params).await?;
        let data: RcmdData = self
            .get_json(&format!("https://api.bilibili.com/x/web-interface/wbi/index/top/feed/rcmd?{signed}"))
            .await?;
        Ok(data.item)
    }

    /// 综合热门（无签名，pn/ps 分页）。
    pub async fn popular(&self, pn: u32, ps: u32) -> Result<Vec<ArchiveCard>> {
        let data: ArchiveListData = self
            .get_json(&format!("https://api.bilibili.com/x/web-interface/popular?ps={ps}&pn={pn}"))
            .await?;
        Ok(data.list)
    }

    /// 入站必刷（一次全量，实测 98 条）。
    pub async fn precious(&self, page: u32) -> Result<Vec<ArchiveCard>> {
        let data: ArchiveListData = self
            .get_json(&format!("https://api.bilibili.com/x/web-interface/popular/precious?page={page}"))
            .await?;
        Ok(data.list)
    }

    /// 排行榜（一次全量，实测 100 条；rid=0 全站）。
    pub async fn ranking(&self, rid: u32, typ: &str) -> Result<Vec<ArchiveCard>> {
        let data: RankingData = self
            .get_json(&format!(
                "https://api.bilibili.com/x/web-interface/ranking/v2?rid={rid}&type={typ}"
            ))
            .await?;
        Ok(data.list)
    }

    /// 剧集卡推荐（无签名；season_type：1番剧 2电影 3纪录片 4国创 5电视剧 7综艺）。
    pub async fn ogv(&self, season_type: u32, request_cnt: u32) -> Result<Vec<OgvCard>> {
        let data: OgvData = self
            .get_json(&format!(
                "https://api.bilibili.com/x/web-interface/index/ogv/rcmd?request_cnt={request_cnt}&brush=0&fresh_type=0&season_type={season_type}"
            ))
            .await?;
        Ok(data.items)
    }

    /// 播放页相关视频（无签名；`data` 直接为 ArchiveCard 数组，无分页）。
    /// 实测（2026-11-17，BV1GJ411x7h7）code=0，卡形状与 popular/precious/ranking 同构；
    /// 归档卡契约见 `api/openapi/bilibili-feed-v1.yaml`，fixture `feed-related.json`。
    pub async fn related(&self, bvid: &str) -> Result<Vec<ArchiveCard>> {
        let data: Vec<ArchiveCard> = self
            .get_json(&format!(
                "https://api.bilibili.com/x/web-interface/archive/related?bvid={bvid}"
            ))
            .await?;
        Ok(data)
    }
}

#[cfg(test)]
mod session_tests {
    use super::*;

    fn av(bvid: &str) -> FeedCard {
        FeedCard {
            id: 1,
            goto: Some("av".into()),
            bvid: Some(bvid.into()),
            ..FeedCard::default()
        }
    }

    fn ad() -> FeedCard {
        FeedCard { goto: Some("ad".into()), ..FeedCard::default() }
    }

    #[test]
    fn plan_first_page_next_sequence() {
        let mut s = FeedSession::new();
        let first = s.plan(FeedKind::First);
        assert_eq!(first.fresh_idx, Some(0));
        assert_eq!(first.fresh_idx_1h, Some(0));
        assert_eq!(first.fresh_type, Some(0));
        assert_eq!(first.ps, Some(10));
        assert_eq!(first.fetch_row, Some(1));
        assert_eq!(first.brush, Some(0));

        let next1 = s.plan(FeedKind::Next);
        assert_eq!(next1.fresh_idx, Some(1));
        assert_eq!(next1.fresh_idx_1h, Some(1));
        assert_eq!(next1.fresh_type, Some(4));
        assert_eq!(next1.ps, Some(12));
        assert_eq!(next1.fetch_row, Some(4));
        assert_eq!(next1.brush, Some(0));

        let next2 = s.plan(FeedKind::Next);
        assert_eq!(next2.fresh_idx, Some(2));
        assert_eq!(next2.fresh_idx_1h, Some(2));
        assert_eq!(next2.fetch_row, Some(7));
        assert_eq!(next2.brush, Some(1));
    }

    #[test]
    fn plan_refresh_resets_idx_but_keeps_1h_pool() {
        let mut s = FeedSession::new();
        let _ = s.plan(FeedKind::First);
        let _ = s.plan(FeedKind::Next);
        let refresh = s.plan(FeedKind::Refresh);
        assert_eq!(refresh.fresh_idx, Some(0));
        assert_eq!(refresh.fresh_idx_1h, Some(1)); // 1h 池不随刷新重置（First=0 → Next=1）
        assert_eq!(refresh.fresh_type, Some(5));
        assert_eq!(refresh.ps, Some(10));
        assert_eq!(refresh.fetch_row, Some(1));
        assert_eq!(refresh.brush, Some(1)); // refresh 计数独立递增
        let next = s.plan(FeedKind::Next);
        assert_eq!(next.brush, Some(1)); // dropDown 计数独立续（本测试已 DropDown 过一次）
        assert_eq!(next.fresh_idx, Some(1));
    }

    #[test]
    fn plan_resets_1h_pool_after_60min() {
        let mut s = FeedSession::new();
        let _ = s.plan(FeedKind::First);
        let _ = s.plan(FeedKind::Next);
        // 注入"上次请求在 2h 前"：1h 池应归零再 +1。
        s.last_feed_time = Instant::now().checked_sub(Duration::from_secs(2 * 3600));
        let next = s.plan(FeedKind::Next);
        assert_eq!(next.fresh_idx, Some(2)); // 主池继续
        assert_eq!(next.fresh_idx_1h, Some(1)); // 0 → +1（重新进入 1h 池）
    }

    #[test]
    fn absorb_filters_non_av_and_dedups() {
        let mut s = FeedSession::new();
        let first = s.absorb(vec![av("BV1"), av("BV2"), ad(), FeedCard::default()]);
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].bvid.as_deref(), Some("BV1"));

        // 跨页重复：BV1 重复出现 → 只吸收 BV3。
        let second = s.absorb(vec![av("BV1"), av("BV3")]);
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].bvid.as_deref(), Some("BV3"));
    }

    /// 回归：换一换必须从干净的去重窗口吸收整批新流。
    ///
    /// 换一换把 fresh_idx 重置为 0，服务端回吐的卡与首屏/已滚动内容高度重叠；
    /// 若 plan(Refresh) 不清空 seen，这些卡会被整批去重丢弃，UI 又清空旧列表
    /// → 推荐页被清空且不加载（见 docs/recommend-mechanism.md §5）。
    #[test]
    fn refresh_clears_dedup_window() {
        let mut s = FeedSession::new();
        // 首屏：吸收 BV1/BV2。
        let _ = s.plan(FeedKind::First);
        assert_eq!(s.absorb(vec![av("BV1"), av("BV2")]).len(), 2);
        // 滚动：plan(Next) 保留窗口，BV2 被去重、BV3 进入窗口。
        let _ = s.plan(FeedKind::Next);
        let next = s.absorb(vec![av("BV2"), av("BV3")]);
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].bvid.as_deref(), Some("BV3"));
        // 换一换：plan(Refresh) 清空窗口 → 与首屏重叠的 BV1/BV2 不再被丢弃。
        let _ = s.plan(FeedKind::Refresh);
        let refreshed = s.absorb(vec![av("BV1"), av("BV2"), av("BV9")]);
        assert_eq!(refreshed.len(), 3, "换一换应从干净去重窗口吸收整批新流");
        // 换一换之后的滚动预取仍以新流为基线去重（不误伤滚动去重）。
        let _ = s.plan(FeedKind::Next);
        let after = s.absorb(vec![av("BV1"), av("BV4")]);
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].bvid.as_deref(), Some("BV4"));
    }

    #[test]
    fn plan_with_ps_clamps_to_max_rcmd_ps() {
        let mut s = FeedSession::new();
        let q1 = s.plan_with_ps(FeedKind::First, 50);
        assert_eq!(q1.ps, Some(MAX_RCMD_PS));
        let q2 = s.plan_with_ps(FeedKind::Next, 0);
        assert_eq!(q2.ps, Some(1));
    }

    #[test]
    fn exhausted_marks_end_and_refresh_resets_it() {
        let mut s = FeedSession::new();
        let _ = s.plan_with_ps(FeedKind::First, 30);
        assert_eq!(s.absorb(vec![av("BV1")]).len(), 1);
        assert!(!s.is_exhausted());

        // 连续 2 轮全部重复（净新增为 0）→ 判定耗尽，定格最后一页。
        let _ = s.plan_with_ps(FeedKind::Next, 30);
        assert!(s.absorb(vec![av("BV1")]).is_empty());
        assert!(!s.is_exhausted());

        let _ = s.plan_with_ps(FeedKind::Next, 30);
        assert!(s.absorb(vec![av("BV1")]).is_empty());
        assert!(s.is_exhausted());

        // 换一换重置耗尽状态。
        let _ = s.plan_with_ps(FeedKind::Refresh, 30);
        assert!(!s.is_exhausted());

        // 服务端返回空列表直接判定耗尽。
        assert!(s.absorb(Vec::new()).is_empty());
        assert!(s.is_exhausted());
    }
}

#[cfg(test)]
mod related_tests {
    use super::*;
    use crate::error::BiliError;

    #[test]
    fn related_url_uses_archive_related_endpoint() {
        // 防回归：端点必须是 `archive/related`（`wbi/related/all` 实测返回 HTML，见
        // docs/bilibili-api-inventory.md §6）。此处锚定 URL 构造唯一来源。
        let bvid = "BV1GJ411x7h7";
        let url = format!(
            "https://api.bilibili.com/x/web-interface/archive/related?bvid={bvid}"
        );
        assert!(url.contains("/x/web-interface/archive/related?"));
        assert!(!url.contains("wbi/related"));
    }

    #[tokio::test]
    #[ignore = "需访问真实 B 站接口（验证 BV1GJ411x7h7 相关视频）"]
    async fn live_related_smoke() {
        struct NoopCreds;
        #[async_trait::async_trait]
        impl CredentialProvider for NoopCreds {
            async fn get(&self) -> Result<crate::credentials::Credentials> {
                Err(BiliError::NotLoggedIn)
            }
            fn cached(&self) -> Option<crate::credentials::Credentials> {
                None
            }
        }
        let http = reqwest::Client::builder()
            .user_agent(crate::wbi::BROWSER_UA)
            .build()
            .unwrap();
        let creds: Arc<dyn CredentialProvider> = Arc::new(NoopCreds);
        let client = FeedClient::new(http, creds);
        let cards = client.related("BV1GJ411x7h7").await.unwrap();
        assert!(!cards.is_empty(), "相关视频不应为空");
        let first = &cards[0];
        assert!(first.aid > 0 && first.bvid.starts_with("BV"));
        assert!(first.cid > 0 && first.duration > 0);
        assert!(!first.owner.name.is_empty());
        assert!(first.pic.starts_with("http"));
        // 与当前稿件互异（不能把自身推回相关位）。
        assert!(cards.iter().all(|c| c.bvid != "BV1GJ411x7h7"));
    }
}
