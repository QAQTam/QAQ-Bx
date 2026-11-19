//! 收藏夹客户端（登录态四页接线 U5：夹列表 → 内容列表两级）。
//!
//! 契约锚 `api/openapi/bilibili-user-v1.yaml`（x-contract-version 1），
//! 行为锚 `api/fixtures/user-fav-folders.json` / `user-fav-resources.json`，
//! 真机样本 `.qaqh/research/samples/user/`（2026-11-20，登录态实测）。
//!
//! 实测定案：
//! - 夹列表 `GET /x/v3/fav/folder/created/list-all?up_mid=<mid>`：
//!   `data{count,list[],season}`，一次全量（默认夹 + 自建夹，默认夹在首）。
//!   `list[].id` = `media_id`（资源列表入参）；`fid` 为老版夹 id，两者不同。
//! - 资源列表 `GET /x/v3/fav/resource/list?media_id&pn&ps&keyword&order&type=0&tid=0&platform=web`：
//!   `data{info,medias[],has_more}`；`has_more=false` 为末页。
//! - `medias[].type=2` 为视频；音频/课程/合集等其余类型与失效条目 `bvid`
//!   为空 —— 客户端按 `bvid` 非空过滤后消费。

use std::sync::Arc;

use serde::Deserialize;

use crate::credentials::CredentialProvider;
use crate::error::Result;

const FOLDER_ALL_URL: &str = "https://api.bilibili.com/x/v3/fav/folder/created/list-all";
const RESOURCE_LIST_URL: &str = "https://api.bilibili.com/x/v3/fav/resource/list";

/// 收藏夹（展示模型；`id` 即资源列表的 `media_id`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FavFolder {
    pub id: u64,
    pub title: String,
    pub media_count: u64,
}

/// 收藏内容条目（视频行；失效/非视频行已过滤）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FavMedia {
    pub id: u64,
    pub bvid: String,
    pub title: String,
    pub cover: String,
    /// 视频总时长（秒）。
    pub duration_secs: i64,
    pub up_name: String,
    pub up_mid: u64,
    pub plays: u64,
    pub collects: u64,
    pub danmakus: u64,
    /// 收藏时间戳（秒）。
    pub fav_at: i64,
}

/// 收藏夹内容页（夹信息来自 `data.info`；翻页锚 `has_more`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FavResourcePage {
    pub folder_id: u64,
    pub folder_title: String,
    pub folder_media_count: u64,
    pub items: Vec<FavMedia>,
    pub has_more: bool,
}

/// 收藏夹客户端（登录态必需；默认「按收藏时间倒序」，与本轮页面一致）。
pub struct FavClient {
    http: reqwest::Client,
    creds: Arc<dyn CredentialProvider>,
}

impl FavClient {
    pub fn new(http: reqwest::Client, creds: Arc<dyn CredentialProvider>) -> Self {
        Self { http, creds }
    }

    /// 我创建的收藏夹（一次全量；顺序即服务端序，默认夹在前）。
    pub async fn created_folders(&self, mid: u64) -> Result<Vec<FavFolder>> {
        let url = format!("{FOLDER_ALL_URL}?up_mid={mid}");
        let raw: RawFolderListAll =
            crate::httpx::get_json_with_ticket(&self.http, &self.creds, None, &url).await?;
        Ok(parse_folders(raw.list))
    }

    /// 收藏夹内容（`pn` 从 1 起，固定 `ps=20` + `order=mtime`）。
    pub async fn resources(&self, media_id: u64, pn: u32) -> Result<FavResourcePage> {
        if media_id == 0 {
            return Err(crate::error::BiliError::InvalidData("缺少收藏夹 id".into()));
        }
        let pn = pn.max(1);
        let url = format!(
            "{RESOURCE_LIST_URL}?media_id={media_id}&pn={pn}&ps=20&keyword=&order=mtime&type=0&tid=0&platform=web"
        );
        let raw: RawResourceData =
            crate::httpx::get_json_with_ticket(&self.http, &self.creds, None, &url).await?;
        Ok(FavResourcePage {
            folder_id: raw.info.id,
            folder_title: raw.info.title.unwrap_or_default(),
            folder_media_count: raw.info.media_count,
            items: parse_medias(raw.medias),
            has_more: raw.has_more.unwrap_or(false),
        })
    }
}

// ---- 宽松原始行(形状与历史组一致:数字双形态、字段可缺省) ----

#[derive(Deserialize)]
pub(crate) struct RawFolderListAll {
    #[serde(default)]
    list: Vec<RawFolderRow>,
}

#[derive(Deserialize)]
pub(crate) struct RawFolderRow {
    #[serde(default, deserialize_with = "crate::loose::u64_any_or_zero")]
    id: u64,
    #[serde(default)]
    title: Option<String>,
    #[serde(default, deserialize_with = "crate::loose::u64_any_or_zero")]
    media_count: u64,
}

#[derive(Deserialize)]
pub(crate) struct RawResourceData {
    #[serde(default)]
    info: RawFolderInfo,
    #[serde(default)]
    medias: Vec<RawMediaRow>,
    #[serde(default, deserialize_with = "crate::loose::opt_bool_any")]
    has_more: Option<bool>,
}

#[derive(Deserialize, Default)]
pub(crate) struct RawFolderInfo {
    #[serde(default, deserialize_with = "crate::loose::u64_any_or_zero")]
    id: u64,
    #[serde(default)]
    title: Option<String>,
    #[serde(default, deserialize_with = "crate::loose::u64_any_or_zero")]
    media_count: u64,
}

#[derive(Deserialize)]
pub(crate) struct RawMediaRow {
    #[serde(default, deserialize_with = "crate::loose::u64_any_or_zero")]
    id: u64,
    #[serde(default)]
    bvid: Option<String>,
    #[serde(default)]
    bv_id: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    cover: Option<String>,
    #[serde(default, deserialize_with = "crate::loose::opt_i64_any")]
    duration: Option<i64>,
    #[serde(default)]
    upper: Option<crate::history::RawOwner>,
    #[serde(default)]
    cnt_info: Option<RawCntInfo>,
    #[serde(default, deserialize_with = "crate::loose::opt_i64_any")]
    fav_time: Option<i64>,
}

#[derive(Deserialize)]
pub(crate) struct RawCntInfo {
    #[serde(default, deserialize_with = "crate::loose::u64_any_or_zero")]
    play: u64,
    #[serde(default, deserialize_with = "crate::loose::u64_any_or_zero")]
    collect: u64,
    #[serde(default, deserialize_with = "crate::loose::u64_any_or_zero")]
    danmaku: u64,
}

/// 宽松可选布尔：`has_more` 实测为 bool，缺省按 `None`（客户端兜底末页）。
pub(crate) fn parse_folders(raw: Vec<RawFolderRow>) -> Vec<FavFolder> {
    raw.into_iter()
        .filter(|f| f.id != 0)
        .map(|f| FavFolder {
            id: f.id,
            title: f.title.unwrap_or_default(),
            media_count: f.media_count,
        })
        .collect()
}

pub(crate) fn parse_medias(raw: Vec<RawMediaRow>) -> Vec<FavMedia> {
    raw.into_iter()
        .filter(|m| {
            let bvid = m.bvid.as_deref().or(m.bv_id.as_deref());
            bvid.map(|b| !b.trim().is_empty()).unwrap_or(false) && m.id != 0
        })
        .map(|m| {
            let upper = m.upper;
            let cnt = m.cnt_info;
            FavMedia {
                id: m.id,
                bvid: m
                    .bvid
                    .filter(|b| !b.trim().is_empty())
                    .or(m.bv_id.filter(|b| !b.trim().is_empty()))
                    .unwrap_or_default(),
                title: m.title.unwrap_or_default(),
                cover: m.cover.unwrap_or_default(),
                duration_secs: m.duration.unwrap_or(0),
                up_name: upper.as_ref().and_then(|u| u.name.clone()).unwrap_or_default(),
                up_mid: upper.as_ref().map(|u| u.mid).unwrap_or(0),
                plays: cnt.as_ref().map(|c| c.play).unwrap_or(0),
                collects: cnt.as_ref().map(|c| c.collect).unwrap_or(0),
                danmakus: cnt.as_ref().map(|c| c.danmaku).unwrap_or(0),
                fav_at: m.fav_time.unwrap_or(0),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feed::Envelope;

    fn folder(json: &str) -> RawFolderRow {
        serde_json::from_str(json).expect("raw folder")
    }
    fn media(json: &str) -> RawMediaRow {
        serde_json::from_str(json).expect("raw media")
    }

    #[test]
    fn folders_keep_order_and_counts() {
        let folders = parse_folders(vec![
            folder(r#"{"id":105925889,"fid":1059258,"mid":85841389,"title":"TEMP","media_count":"153"}"#),
            folder(r#"{"id":42,"title":"默认收藏夹","media_count":7}"#),
            folder(r#"{"id":0,"title":"无效行"}"#),
        ]);
        assert_eq!(folders.len(), 2, "id=0 的行丢弃");
        assert_eq!(folders[0].id, 105_925_889);
        assert_eq!(folders[0].media_count, 153);
        assert_eq!(folders[1].title, "默认收藏夹");
    }

    #[test]
    fn medias_filter_non_video_and_dead_rows() {
        let medias = parse_medias(vec![
            media(
                r#"{"id":117621907326313,"type":2,"title":"视频","cover":"http://i0.hdslb.com/c.jpg",
                    "duration":"14","upper":{"mid":"17543422","name":"UP"},
                    "cnt_info":{"collect":"134645","play":"2961590","danmaku":"17981"},
                    "fav_time":1795000000,"bvid":"BV1test"}"#,
            ),
            media(r#"{"id":1,"type":12,"title":"音频","bvid":""}"#),
            media(r#"{"id":2,"type":2,"title":"已失效","bvid":""}"#),
            media(r#"{"id":0,"type":2,"title":"无id","bvid":"BVx"}"#),
        ]);
        assert_eq!(medias.len(), 1, "失效/音频/无 id 行全部过滤");
        let m = &medias[0];
        assert_eq!(m.bvid, "BV1test");
        assert_eq!(m.up_name, "UP");
        assert_eq!(m.plays, 2_961_590);
        assert_eq!(m.collects, 134_645);
        assert_eq!(m.danmakus, 17_981);
        assert_eq!(m.fav_at, 1_795_000_000);
    }

    #[test]
    fn medias_fall_back_to_bv_id_field() {
        let medias = parse_medias(vec![media(
            r#"{"id":9,"type":2,"title":"冗余形态","bv_id":"BV9form"}"#,
        )]);
        assert_eq!(medias.len(), 1);
        assert_eq!(medias[0].bvid, "BV9form");
    }

    #[test]
    fn resource_data_deserializes_with_defaults() {
        let raw: RawResourceData = serde_json::from_str(
            r#"{"info":{"id":7,"title":"默认收藏夹","media_count":"99"},"medias":[],"has_more":true}"#,
        )
        .expect("raw resource data");
        assert_eq!(raw.info.id, 7);
        assert_eq!(raw.info.media_count, 99);
        assert_eq!(raw.has_more, Some(true));
        // info/medias 缺省 → 全默认
        let empty: RawResourceData = serde_json::from_str("{}").expect("empty");
        assert_eq!(empty.info.id, 0);
        assert_eq!(empty.medias.len(), 0);
        assert_eq!(empty.has_more, None);
    }

    // ---- fixtures 行为锚(api/README.md:服务器行为变化 → 测试红灯 → 契约迁移) ----

    fn fixture_json(name: &str) -> serde_json::Value {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../api/fixtures/");
        let raw = std::fs::read_to_string(format!("{path}{name}.json"))
            .expect("fixture 缺失：api/fixtures 应随仓库提供");
        let v: serde_json::Value = serde_json::from_str(&raw).expect("fixture 不是合法 JSON");
        v["json"].clone()
    }

    /// 收藏夹列表契约锚:data{count,list[],season};id=media_id、fid 并存。
    #[test]
    fn fav_folders_fixture_matches_contract_v1() {
        let env: Envelope<RawFolderListAll> = serde_json::from_value(fixture_json("user-fav-folders"))
            .expect("fav-folders fixture 解析失败");
        assert_eq!(env.code, 0, "fav-folders fixture code=0");
        let data = env.data.expect("fav-folders 缺 data");
        let folders = parse_folders(data.list);
        assert!(!folders.is_empty(), "夹列表不应为空(至少有默认夹)");
        let first = &folders[0];
        assert!(first.id > 0, "夹 id(media_id) 缺失");
        assert!(!first.title.is_empty(), "夹标题缺失");
    }

    /// 收藏资源契约锚:data{info,medias[],has_more};视频行 bvid 非空、cnt_info 齐全。
    #[test]
    fn fav_resources_fixture_matches_contract_v1() {
        let env: Envelope<RawResourceData> = serde_json::from_value(fixture_json("user-fav-resources"))
            .expect("fav-resources fixture 解析失败");
        assert_eq!(env.code, 0, "fav-resources fixture code=0");
        let data = env.data.expect("fav-resources 缺 data");
        assert!(data.info.id > 0 && data.info.media_count > 0, "info 夹信息异常");
        let items = parse_medias(data.medias);
        assert!(!items.is_empty(), "medias 无可播视频行");
        let first = &items[0];
        assert!(first.bvid.starts_with("BV"), "视频行 bvid 异常");
        assert!(first.duration_secs > 0 && first.fav_at > 1_000_000_000);
        assert!(first.plays > 0, "cnt_info.play 缺失");
        assert!(!first.up_name.is_empty());
        assert_eq!(data.has_more, Some(true), "翻页锚 has_more 应保留原值");
    }

    /// 需真实网络+登录:收藏夹两级冒烟(本地凭据 data/credentials.json,只读)。
    /// 跑法:`cargo test -p qaq-bx-core -- --ignored fav_smoke --nocapture`
    #[test]
    #[ignore = "实网冒烟(登录态):cargo test -p qaq-bx-core -- --ignored fav_smoke"]
    fn fav_smoke() {
        let store = Arc::new(crate::credentials::FileCredentialStore::new(
            std::path::PathBuf::from(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../data/credentials.json"
            )),
        ));
        let creds = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../data/credentials.json"
        ))
        .map(|raw| serde_json::from_str::<crate::credentials::Credentials>(&raw).expect("credentials.json 解析"))
        .expect("data/credentials.json 应存在");
        let mid = creds.dede_user_id;
        let client = FavClient::new(
            reqwest::Client::new(),
            Arc::new(crate::credentials::DefaultCredentialProvider::new(store)),
        );
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let folders = rt
            .block_on(async { client.created_folders(mid).await })
            .expect("收藏夹列表应 code=0");
        assert!(!folders.is_empty(), "至少应有默认收藏夹");
        let first = folders.first().unwrap().clone();
        let page = rt
            .block_on(async { client.resources(first.id, 1).await })
            .expect("收藏资源列表应 code=0");
        assert_eq!(page.folder_id, first.id);
        // 夹内可能无视频(空夹):只锚 info 回读与翻页字段语义。
        if !page.items.is_empty() {
            assert!(page.items.iter().all(|m| !m.bvid.is_empty()));
        }
    }
}
