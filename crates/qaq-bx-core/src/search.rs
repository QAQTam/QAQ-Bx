//! B 站综合搜索客户端(`GET /x/web-interface/wbi/search/all/v2`,**WBI 签名**)。
//!
//! 端点口径(docs/bilibili-api-inventory.md §5,实测 code=0):
//! - 强依赖 `buvid3` 风控;未登录亦可用,凭据/Cookie 注入由 [`crate::httpx`] 统一处理;
//! - `data.result[]` 多类型聚合(`result_type: video|bili_user|…`),本客户端仅消费
//!   `video` 组,其余组跳过;
//! - `title` 内嵌 `<em class="keyword">…</em>` 高亮标记,剥除后返回;
//! - `duration` 为 `"H:MM:SS"` / `"M:SS"` 文本,解析为秒;
//! - `pic` 常为协议相对(`//i0.hdslb.com/…`),归一化为 `https://`。

use std::sync::Arc;

use serde::Deserialize;

use crate::account::NavClient;
use crate::credentials::CredentialProvider;
use crate::error::Result;
use crate::ticket::TicketManager;
use crate::wbi::WbiSigner;

const SEARCH_ALL_URL: &str = "https://api.bilibili.com/x/web-interface/wbi/search/all/v2";
/// 搜索框默认词（U2 接线；WBI 签名,契约锚 inventory §5）。
const SEARCH_DEFAULT_URL: &str = "https://api.bilibili.com/x/web-interface/wbi/search/default";

/// 搜索结果视频卡(仅 `result_type == "video"` 组)。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SearchVideo {
    pub bvid: String,
    pub aid: u64,
    /// 默认分 P cid(部分搜索卡缺失,缺失时为 0,播放解析回退 view 接口)。
    pub cid: u64,
    /// 标题(高亮标记与 HTML 实体已剥除)。
    pub title: String,
    pub author: String,
    pub author_mid: u64,
    pub duration_secs: u32,
    pub pubdate: i64,
    pub play_count: u64,
    /// 搜索卡的弹幕数口径(`video_review` 字段)。
    pub danmaku_count: u64,
    /// 封面 URL(`https://` 归一化后)。
    pub pic_url: Option<String>,
}

/// 一页搜索结果(video 组)。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SearchPage {
    pub query: String,
    pub videos: Vec<SearchVideo>,
    pub total: Option<u64>,
}

/// 综合搜索客户端。`creds` 无凭据时以未登录态请求(实测可用)。
pub struct SearchClient {
    http: reqwest::Client,
    creds: Arc<dyn CredentialProvider>,
    ticket: Option<Arc<TicketManager>>,
    wbi: WbiSigner,
}

impl SearchClient {
    pub fn new(http: reqwest::Client, creds: Arc<dyn CredentialProvider>) -> Self {
        let nav = Arc::new(NavClient::new(http.clone(), creds.clone()));
        Self::with_nav(http, creds, nav)
    }

    /// 复用共享 NavClient:与 feed/播放共用同一份 nav 缓存(WBI 密钥只拉一次)。
    pub fn with_nav(
        http: reqwest::Client,
        creds: Arc<dyn CredentialProvider>,
        nav: Arc<NavClient>,
    ) -> Self {
        let ticket = nav.ticket().cloned();
        Self {
            wbi: WbiSigner::with_nav(nav),
            http,
            creds,
            ticket,
        }
    }

    /// 综合搜索一页(`page` 从 1 起,`page_size` 实测上限 50)。
    pub async fn all_v2(&self, keyword: &str, page: u32, page_size: u32) -> Result<SearchPage> {
        let keyword = keyword.trim();
        if keyword.is_empty() {
            return Err(crate::error::BiliError::InvalidData("搜索词为空".into()));
        }
        let params = vec![
            ("keyword", keyword.to_string()),
            ("page", page.max(1).to_string()),
            ("page_size", page_size.clamp(1, 50).to_string()),
        ];
        let signed = self.wbi.sign_query(&params).await?;
        let data: RawSearchData = crate::httpx::get_json_with_ticket(
            &self.http,
            &self.creds,
            self.ticket.as_deref(),
            &format!("{SEARCH_ALL_URL}?{signed}"),
        )
        .await?;
        Ok(parse_search_page(keyword, data))
    }

    /// 搜索框默认词（U2）：失败返回 `Err`,调用方静默降级到通用占位文案。
    pub async fn default_word(&self) -> Result<String> {
        let signed = self.wbi.sign_query(&[]).await?;
        #[derive(Deserialize)]
        struct RawDefault {
            #[serde(default)]
            name: String,
            #[serde(default)]
            show_name: String,
        }
        let data: RawDefault = crate::httpx::get_json_with_ticket(
            &self.http,
            &self.creds,
            self.ticket.as_deref(),
            &format!("{SEARCH_DEFAULT_URL}?{signed}"),
        )
        .await?;
        let word = if data.show_name.trim().is_empty() {
            data.name
        } else {
            data.show_name
        };
        let word = word.trim().trim_start_matches('#').trim_end_matches('#').trim().to_string();
        if word.is_empty() {
            return Err(crate::error::BiliError::InvalidData("默认词为空".into()));
        }
        Ok(word)
    }
}

// ---------- 线格式 ----------

#[derive(Debug, Default, Deserialize)]
struct RawSearchData {
    #[serde(default)]
    result: Option<Vec<RawResultGroup>>,
    #[serde(
        default,
        rename = "numResults",
        deserialize_with = "crate::loose::u64_any_or_zero"
    )]
    num_results: u64,
}

/// `result[]` 分组:video 组的 `data` 为视频卡数组,其余类型形状不一,
/// 统一先落 `serde_json::Value` 再按需解析,避免异形组拖垮整页。
#[derive(Debug, Default, Deserialize)]
struct RawResultGroup {
    #[serde(default)]
    result_type: Option<String>,
    #[serde(default)]
    data: Option<serde_json::Value>,
}

#[derive(Debug, Default, Deserialize)]
struct RawVideoItem {
    #[serde(default)]
    bvid: Option<String>,
    #[serde(default, deserialize_with = "crate::loose::u64_any_or_zero")]
    aid: u64,
    #[serde(default, deserialize_with = "crate::loose::u64_any_or_zero")]
    cid: u64,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    author: Option<String>,
    #[serde(default, deserialize_with = "crate::loose::u64_any_or_zero")]
    mid: u64,
    #[serde(default)]
    duration: Option<String>,
    #[serde(default, deserialize_with = "crate::loose::i64_any_or_zero")]
    pubdate: i64,
    #[serde(default, deserialize_with = "crate::loose::u64_any_or_zero")]
    play: u64,
    #[serde(default, deserialize_with = "crate::loose::u64_any_or_zero")]
    video_review: u64,
    #[serde(default)]
    pic: Option<String>,
}

// ---------- 解析 ----------

fn parse_search_page(query: &str, data: RawSearchData) -> SearchPage {
    let mut videos = Vec::new();
    if let Some(groups) = data.result {
        for group in groups {
            if group.result_type.as_deref() != Some("video") {
                continue;
            }
            let Some(serde_json::Value::Array(items)) = group.data else {
                continue;
            };
            for item in items {
                let Ok(raw) = serde_json::from_value::<RawVideoItem>(item) else {
                    continue;
                };
                let Some(bvid) = raw.bvid.filter(|bvid| !bvid.is_empty()) else {
                    continue;
                };
                videos.push(SearchVideo {
                    bvid,
                    aid: raw.aid,
                    cid: raw.cid,
                    title: raw
                        .title
                        .as_deref()
                        .map(strip_highlight)
                        .unwrap_or_default(),
                    author: raw.author.unwrap_or_default(),
                    author_mid: raw.mid,
                    duration_secs: raw
                        .duration
                        .as_deref()
                        .and_then(parse_duration_hms)
                        .unwrap_or(0),
                    pubdate: raw.pubdate,
                    play_count: raw.play,
                    danmaku_count: raw.video_review,
                    pic_url: raw.pic.as_deref().map(normalize_media_url),
                });
            }
        }
    }
    SearchPage {
        query: query.to_string(),
        total: (data.num_results > 0).then_some(data.num_results),
        videos,
    }
}

/// 剥除搜索标题里的 `<em …>关键词</em>` 高亮标记(保留其间的关键词文本)
/// 与常见 HTML 实体;残缺闭标记时,其后内容按普通文本保留。
fn strip_highlight(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    let mut rest = title;
    while let Some(pos) = rest.find("<em") {
        out.push_str(&rest[..pos]);
        rest = &rest[pos..];
        // 开标记以 '>' 结束;找不到 '>' 视为普通文本,原样保留避免死循环。
        let Some(open_end) = rest.find('>') else {
            break;
        };
        rest = &rest[open_end + 1..];
        match rest.find("</em>") {
            Some(close) => {
                out.push_str(&rest[..close]);
                rest = &rest[close + "</em>".len()..];
            }
            None => break,
        }
    }
    out.push_str(rest);
    out.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

/// `"8:42"` → 522 秒;`"1:08:42"` → 4122 秒。负值/非数字段视为 0。
fn parse_duration_hms(text: &str) -> Option<u32> {
    let mut total = 0u64;
    for part in text.split(':') {
        let value: u64 = part.trim().parse().ok()?;
        total = total.checked_mul(60)?.checked_add(value)?;
    }
    u32::try_from(total).ok()
}

/// 搜索卡封面 `//i0.hdslb.com/…` → `https://i0.hdslb.com/…`。
fn normalize_media_url(url: &str) -> String {
    let trimmed = url.trim();
    if let Some(rest) = trimmed.strip_prefix("//") {
        format!("https://{rest}")
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::BiliError;

    #[test]
    fn parses_video_group_and_skips_other_types() {
        // get_json_with_ticket 解包信封后传入的是 `data` 内层,不含 code/data 包裹。
        let data: RawSearchData = serde_json::from_str(
            r#"{
                "numResults": 9876,
                    "result": [
                        {"result_type": "bili_user", "data": [{"mid": 1, "uname": "UP"}]},
                        {"result_type": "video", "data": [
                            {"bvid": "BV1GJ411x7h7", "aid": 80433022, "cid": 137649199,
                             "title": "<em class=\"keyword\">rust</em> 桌面客户端 &quot;实战&quot;",
                             "author": "Rust 咸鱼", "mid": 486906719,
                             "duration": "8:42", "pubdate": 1575122045,
                             "play": 1208300, "video_review": 12400,
                             "pic": "//i0.hdslb.com/bfs/archive/cover.jpg"},
                            {"aid": 1, "title": "无 bvid 的脏卡"}
                        ]},
                        {"result_type": "live", "data": {"roomid": 9}}
                    ]
            }"#,
        )
        .unwrap();
        let page = parse_search_page("rust", data);
        assert_eq!(page.total, Some(9876));
        assert_eq!(page.videos.len(), 1, "非 video 组与脏卡应跳过");
        let video = &page.videos[0];
        assert_eq!(video.bvid, "BV1GJ411x7h7");
        assert_eq!(video.aid, 80433022);
        assert_eq!(video.cid, 137649199);
        assert_eq!(video.title, "rust 桌面客户端 \"实战\"");
        assert_eq!(video.author, "Rust 咸鱼");
        assert_eq!(video.author_mid, 486906719);
        assert_eq!(video.duration_secs, 522);
        assert_eq!(video.play_count, 1208300);
        assert_eq!(video.danmaku_count, 12400);
        assert_eq!(
            video.pic_url.as_deref(),
            Some("https://i0.hdslb.com/bfs/archive/cover.jpg")
        );
        assert_eq!(page.query, "rust");
    }

    #[test]
    fn parses_hour_duration_and_strips_entities() {
        assert_eq!(parse_duration_hms("1:08:42"), Some(4122));
        assert_eq!(parse_duration_hms("0:59"), Some(59));
        assert_eq!(parse_duration_hms("59"), Some(59));
        assert_eq!(parse_duration_hms("x:y"), None);
        assert_eq!(
            strip_highlight("A &amp; B <em class=\"keyword\">C</em> &lt;D&gt;"),
            "A & B C <D>"
        );
        // 残缺闭标记:剥除开标记,其后内容按普通文本保留。
        assert_eq!(
            strip_highlight("残缺 <em class=\"x\">无闭合"),
            "残缺 无闭合"
        );
    }

    #[test]
    fn duration_overflow_is_rejected() {
        assert_eq!(parse_duration_hms("99999999999:59"), None);
    }

    #[tokio::test]
    #[ignore = "需访问真实 B 站接口（验证搜索端点）"]
    async fn live_search_smoke() {
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
        let client = SearchClient::new(http, creds);
        let page = client.all_v2("rust", 1, 20).await.unwrap();
        assert!(!page.videos.is_empty(), "搜索应至少命中一条视频卡");
        assert!(page.videos.iter().all(|v| v.bvid.starts_with("BV")));
        assert!(page.videos.iter().all(|v| !v.title.contains("<em")));
        assert!(page.videos.iter().all(|v| {
            v.pic_url
                .as_deref()
                .is_none_or(|u| u.starts_with("https://"))
        }));
    }
}
