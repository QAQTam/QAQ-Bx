//! 历史记录与稍后再看客户端（登录态四页接线 U5）。
//!
//! 契约锚 `api/openapi/bilibili-user-v1.yaml`（x-contract-version 1），
//! 行为锚 `api/fixtures/user-history.json` / `user-toview.json`，
//! 真机样本 `.qaqh/research/samples/user/`（2026-11-20，登录态实测）。
//!
//! 实测定案：
//! - `GET /x/v2/history?pn&ps`：`data` 为**裸数组**（ArchiveCard 超集行 +
//!   `progress/view_at/kid/business`）；现行 PC 页所称 `history/cursor` 实测
//!   404 不存在，pn/ps 翻页可用。
//! - `GET /x/v2/history/toview?pn&ps`：`data{count,list[]}`，条目带 `add_at`。
//! - 行多态：archive 行含 `bvid/cid/owner`；`business=pgc/live` 等行可能缺
//!   `bvid/cid` —— 客户端按 `bvid` 非空过滤后消费（播放器只吃 archive）。
//! - `progress: -1` 表示看完（展示时钳到时长）。

use std::sync::Arc;

use serde::Deserialize;

use crate::credentials::CredentialProvider;
use crate::error::Result;

const HISTORY_URL: &str = "https://api.bilibili.com/x/v2/history";
const TOVIEW_URL: &str = "https://api.bilibili.com/x/v2/history/toview";

/// 历史行 = 稍后再看条目的共享展示模型（同一 ArchiveCard 超集行家族）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryItem {
    pub aid: u64,
    pub bvid: String,
    /// 观看分P 的 cid（播放可直接定点；多 P 视频即所看那 P）。
    pub cid: u64,
    pub title: String,
    pub cover: String,
    /// 视频总时长（秒）。
    pub duration_secs: i64,
    /// 观看进度（秒；`-1` = 看完，展示侧钳到时长）。
    pub progress_secs: i64,
    /// 最后观看时间戳（秒）。
    pub viewed_at: i64,
    /// 加入稍后再看时间戳（秒；仅 toview 端点返回，history 为 `None`）。
    pub added_at: Option<i64>,
    /// 分P 序号（1 起）与总 P 数（多 P 显示 `P2/5`）。
    pub page: i64,
    pub page_count: i64,
    pub up_name: String,
    pub up_mid: u64,
    pub views: u64,
    pub danmakus: u64,
}

impl HistoryItem {
    /// 是否已看完（含时长缺失的退化情形）。
    pub fn is_finished(&self) -> bool {
        self.progress_secs < 0
            || (self.duration_secs > 0 && self.progress_secs >= self.duration_secs)
    }
}

/// 稍后再看页（count 为服务端总数，过滤失效行后列表可能小于 count）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToViewPage {
    pub total: u64,
    pub items: Vec<HistoryItem>,
}

/// 历史记录/稍后再看列表客户端（登录态必需，未登录由凭据面报 `NotLoggedIn`）。
pub struct HistoryClient {
    http: reqwest::Client,
    creds: Arc<dyn CredentialProvider>,
}

impl HistoryClient {
    pub fn new(http: reqwest::Client, creds: Arc<dyn CredentialProvider>) -> Self {
        Self { http, creds }
    }

    /// 历史记录（`pn` 从 1 起，固定 `ps=20`）。
    pub async fn history(&self, pn: u32) -> Result<Vec<HistoryItem>> {
        let pn = pn.max(1);
        let url = format!("{HISTORY_URL}?pn={pn}&ps=20");
        let raw: Vec<RawHistoryRow> =
            crate::httpx::get_json_with_ticket(&self.http, &self.creds, None, &url).await?;
        Ok(parse_history_rows(raw))
    }

    /// 稍后再看（`pn` 从 1 起，固定 `ps=30`；`total` = 服务端 count）。
    pub async fn toview(&self, pn: u32) -> Result<ToViewPage> {
        let pn = pn.max(1);
        let url = format!("{TOVIEW_URL}?pn={pn}&ps=30");
        let raw: RawToViewData =
            crate::httpx::get_json_with_ticket(&self.http, &self.creds, None, &url).await?;
        Ok(raw.into_page())
    }
}

#[derive(Deserialize)]
pub(crate) struct RawToViewData {
    #[serde(default, deserialize_with = "crate::loose::u64_any_or_zero")]
    count: u64,
    #[serde(default)]
    list: Vec<RawHistoryRow>,
}

impl RawToViewData {
    fn into_page(self) -> ToViewPage {
        ToViewPage {
            total: self.count,
            items: parse_history_rows(self.list),
        }
    }
}

/// 宽松历史行：行多态（archive/pgc/live），数字实测存在字符串形态，
/// 全字段缺省安全；archive 判定交给 `parse_history_rows`。
#[derive(Deserialize)]
pub(crate) struct RawHistoryRow {
    #[serde(default, deserialize_with = "crate::loose::opt_u64_any")]
    pub(crate) aid: Option<u64>,
    #[serde(default)]
    pub(crate) bvid: Option<String>,
    #[serde(default, deserialize_with = "crate::loose::opt_u64_any")]
    pub(crate) cid: Option<u64>,
    #[serde(default)]
    pub(crate) title: Option<String>,
    #[serde(default)]
    pub(crate) show_title: Option<String>,
    #[serde(default)]
    pub(crate) pic: Option<String>,
    #[serde(default, deserialize_with = "crate::loose::opt_i64_any")]
    pub(crate) duration: Option<i64>,
    #[serde(default, deserialize_with = "crate::loose::opt_i64_any")]
    pub(crate) progress: Option<i64>,
    #[serde(default, deserialize_with = "crate::loose::opt_i64_any")]
    pub(crate) view_at: Option<i64>,
    #[serde(default, deserialize_with = "crate::loose::opt_i64_any")]
    pub(crate) add_at: Option<i64>,
    /// 实测为**分P 详情对象**（`{cid,page,part,duration,…}`），不是数字。
    #[serde(default)]
    pub(crate) page: Option<RawPageDetail>,
    #[serde(default, deserialize_with = "crate::loose::opt_i64_any")]
    pub(crate) videos: Option<i64>,
    #[serde(default)]
    pub(crate) owner: Option<RawOwner>,
    #[serde(default)]
    pub(crate) stat: Option<RawCardStat>,
}

#[derive(Deserialize)]
pub(crate) struct RawPageDetail {
    #[serde(default, deserialize_with = "crate::loose::opt_u64_any")]
    pub(crate) cid: Option<u64>,
    #[serde(default, deserialize_with = "crate::loose::opt_i64_any")]
    pub(crate) page: Option<i64>,
}

#[derive(Deserialize)]
pub(crate) struct RawOwner {
    #[serde(default, deserialize_with = "crate::loose::u64_any_or_zero")]
    pub(crate) mid: u64,
    #[serde(default)]
    pub(crate) name: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct RawCardStat {
    #[serde(default, deserialize_with = "crate::loose::u64_any_or_zero")]
    pub(crate) view: u64,
    #[serde(default, deserialize_with = "crate::loose::u64_any_or_zero")]
    pub(crate) danmaku: u64,
}

/// 宽松解析：只保留可播放的 archive 行（`bvid` 非空且 `aid` 有效），
/// pgc/live 行静默丢弃；标题回退 `show_title`（pgc 形态）。
pub(crate) fn parse_history_rows(raw: Vec<RawHistoryRow>) -> Vec<HistoryItem> {
    raw.into_iter()
        .filter(|row| {
            row.bvid.as_deref().map(|b| !b.trim().is_empty()).unwrap_or(false)
                && row.aid.unwrap_or(0) != 0
        })
        .map(|row| {
            let owner = row.owner;
            let stat = row.stat;
            let cid = row
                .cid
                .or_else(|| row.page.as_ref().and_then(|p| p.cid))
                .unwrap_or(0);
            HistoryItem {
                aid: row.aid.unwrap_or(0),
                bvid: row.bvid.unwrap_or_default(),
                cid,
                title: row
                    .title
                    .filter(|t| !t.trim().is_empty())
                    .or(row.show_title.filter(|t| !t.trim().is_empty()))
                    .unwrap_or_default(),
                cover: row.pic.unwrap_or_default(),
                duration_secs: row.duration.unwrap_or(0),
                progress_secs: row.progress.unwrap_or(0),
                viewed_at: row.view_at.unwrap_or(0),
                added_at: row.add_at,
                page: row.page.as_ref().and_then(|p| p.page).unwrap_or(1).max(1),
                page_count: row.videos.unwrap_or(1).max(1),
                up_name: owner.as_ref().and_then(|o| o.name.clone()).unwrap_or_default(),
                up_mid: owner.as_ref().map(|o| o.mid).unwrap_or(0),
                views: stat.as_ref().map(|s| s.view).unwrap_or(0),
                danmakus: stat.as_ref().map(|s| s.danmaku).unwrap_or(0),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feed::Envelope;

    fn row(json: &str) -> RawHistoryRow {
        serde_json::from_str(json).expect("raw row")
    }

    #[test]
    fn archive_row_parses_with_string_numbers() {
        let rows = parse_history_rows(vec![row(
            r#"{
                "aid": "117538121845794", "bvid": "BV1test", "cid": "1",
                "title": "KDE Plasma 6.9", "pic": "http://i0.hdslb.com/x.jpg",
                "duration": "131", "progress": "-1", "view_at": 1795100000,
                "page": {"cid": "1", "page": "1", "part": "P1"}, "videos": "3",
                "owner": {"mid": "85841389", "name": "QAQTam"},
                "stat": {"view": "1234", "danmaku": "56"}
            }"#,
        )]);
        assert_eq!(rows.len(), 1);
        let item = &rows[0];
        assert_eq!(item.aid, 117_538_121_845_794);
        assert_eq!(item.bvid, "BV1test");
        assert_eq!(item.cid, 1);
        assert_eq!(item.duration_secs, 131);
        assert_eq!(item.progress_secs, -1);
        assert!(item.is_finished(), "progress=-1 表示看完");
        assert_eq!(item.page_count, 3);
        assert_eq!(item.up_name, "QAQTam");
        assert_eq!(item.views, 1234);
        assert_eq!(item.added_at, None);
    }

    #[test]
    fn non_archive_rows_are_dropped() {
        let rows = parse_history_rows(vec![
            row(r#"{"aid":0,"kid":"live:1234","business":"live","title":"直播","view_at":1}"#),
            row(r#"{"aid":1,"bvid":"","title":"失效","view_at":2}"#),
            row(r#"{"aid":2,"bvid":null,"title":"pgc","business":"pgc","show_title":"番剧","view_at":3}"#),
        ]);
        assert!(rows.is_empty(), "live/失效/pgc 行全部过滤");
    }

    #[test]
    fn pgc_fallback_title_used_when_present() {
        // 防御性:若上游某天 archive 行缺 title 但给 show_title,仍可展示。
        let rows = parse_history_rows(vec![row(
            r#"{"aid":9,"bvid":"BV9","title":null,"show_title":"回退标题","view_at":7}"#,
        )]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "回退标题");
    }

    #[test]
    fn toview_page_keeps_count_and_filters_dead_rows() {
        let page: RawToViewData = serde_json::from_str(
            r#"{"count":208,"list":[
                {"aid":5,"bvid":"BV5","title":"ok","view_at":1,"add_at":99},
                {"aid":6,"bvid":"","title":"已失效","view_at":2}
            ]}"#,
        )
        .expect("raw page");
        let page = page.into_page();
        assert_eq!(page.total, 208, "count 原样透传(展示「共 N 个」)");
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].added_at, Some(99));
    }

    // ---- fixtures 行为锚(api/README.md:服务器行为变化 → 测试红灯 → 契约迁移) ----

    fn fixture_json(name: &str) -> serde_json::Value {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../api/fixtures/");
        let raw = std::fs::read_to_string(format!("{path}{name}.json"))
            .expect("fixture 缺失：api/fixtures 应随仓库提供");
        let v: serde_json::Value = serde_json::from_str(&raw).expect("fixture 不是合法 JSON");
        v["json"].clone()
    }

    /// 历史契约锚:data 裸数组、u64 aid(已溢出 u32)、行含 progress/view_at/bvid。
    #[test]
    fn history_fixture_matches_contract_v1() {
        let env: Envelope<Vec<RawHistoryRow>> =
            serde_json::from_value(fixture_json("user-history")).expect("history fixture 解析失败");
        assert_eq!(env.code, 0, "history fixture code=0");
        let rows = env.data.expect("history 缺 data");
        // 64 位口径:fixture 中至少一行 aid 溢出 u32(实测 1.17e14 量级)
        let max_aid = rows.iter().filter_map(|r| r.aid).max().unwrap_or(0);
        assert!(max_aid > u32::MAX as u64, "fixture 未含 64 位 aid 样本");
        let items = parse_history_rows(rows);
        assert!(!items.is_empty(), "history fixture 无可播放行");
        let first = &items[0];
        assert!(first.aid > 0 && first.bvid.starts_with("BV"), "首行 aid/bvid 异常");
        assert!(first.viewed_at > 1_000_000_000, "view_at 缺失");
        assert!(first.duration_secs > 0, "时长缺失");
        assert!(first.progress_secs >= -1, "progress 语义口径(-1=看完)");
        assert!(!first.up_name.is_empty(), "owner 缺失");
    }

    /// 稍后再看契约锚:data{count,list},条目带 add_at,失效行可被过滤。
    #[test]
    fn toview_fixture_matches_contract_v1() {
        let env: Envelope<RawToViewData> =
            serde_json::from_value(fixture_json("user-toview")).expect("toview fixture 解析失败");
        assert_eq!(env.code, 0, "toview fixture code=0");
        let page = env.data.expect("toview 缺 data").into_page();
        assert!(page.total > 0, "count 应为正(实测 208)");
        assert!(!page.items.is_empty(), "toview fixture 无可播放条目");
        assert!(
            page.items.iter().all(|i| i.added_at.is_some()),
            "toview 条目应带 add_at"
        );
        let first = &page.items[0];
        assert!(first.bvid.starts_with("BV") && first.aid > 0);
        assert!(first.cover.starts_with("http"));
    }

    /// 需真实网络+登录:历史与稍后再看列表冒烟(本地凭据 data/credentials.json,只读)。
    /// 跑法:`cargo test -p qaq-bx-core -- --ignored history_smoke --nocapture`
    #[test]
    #[ignore = "实网冒烟(登录态):cargo test -p qaq-bx-core -- --ignored history_smoke"]
    fn history_smoke() {
        let store = Arc::new(crate::credentials::FileCredentialStore::new(
            std::path::PathBuf::from(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../data/credentials.json"
            )),
        ));
        let client = HistoryClient::new(
            reqwest::Client::new(),
            Arc::new(crate::credentials::DefaultCredentialProvider::new(store)),
        );
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let items = rt.block_on(async { client.history(1).await }).expect("历史列表应 code=0");
        assert!(!items.is_empty(), "登录态历史首页应非空");
        assert!(items.iter().all(|i| !i.bvid.is_empty()), "历史卡不得有空 bvid");
        assert!(items.iter().all(|i| i.viewed_at > 0), "view_at 缺失");
        let page = rt.block_on(async { client.toview(1).await }).expect("稍后再看应 code=0");
        assert!(page.total > 0, "稍后再看 count 应为正");
    }
}

