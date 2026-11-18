//! 视频评论区客户端（主楼分页 + 楼中楼展开）。
//!
//! 契约锚：`docs/bilibili-api-inventory.md` §4：
//! - 主楼列表：`GET /x/v2/reply/main?type=1&oid={aid}&mode={3|2}[&next=..&pagination_str=..]`
//!   （无需 WBI；`/x/v2/reply/wbi/main` 实测触发 -403 风控）；
//! - 楼中楼：`GET /x/v2/reply/reply?type=1&oid={aid}&root={rpid}&pn={pn}&ps={ps}`。

use std::sync::Arc;

use serde::Deserialize;

use crate::account::NavClient;
use crate::credentials::CredentialProvider;
use crate::error::{BiliError, Result};
use crate::httpx::get_json_with_ticket;
use crate::loose::{i64_any_or_zero, u64_any_or_zero};
use crate::ticket::TicketManager;
use crate::wbi::encode_component;

const REPLY_MAIN_URL: &str = "https://api.bilibili.com/x/v2/reply/main";
const REPLY_SUB_URL: &str = "https://api.bilibili.com/x/v2/reply/reply";
/// 主楼发评写接口（U1 接线；`csrf` 必须等于 Cookie `bili_jct`）。
/// 契约锚:inventory §4 只收录了读接口,add 端点参数面为社区通行契约,
/// 真机验证挂起(见 docs/dev-notes/long-run.md U1 行)。
const REPLY_ADD_URL: &str = "https://api.bilibili.com/x/v2/reply/add";

/// 评论排序方式（对齐 `/x/v2/reply/main` `mode` 参数）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReplySort {
    /// 按热度（`mode=3`）。
    #[default]
    Hot,
    /// 按时间（`mode=2`）。
    Latest,
}

impl ReplySort {
    pub const fn mode(self) -> u8 {
        match self {
            Self::Hot => 3,
            Self::Latest => 2,
        }
    }
}

/// 评论区游标（`data.cursor`）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReplyCursor {
    pub next: i64,
    pub next_offset: Option<String>,
    pub is_end: bool,
    pub all_count: u64,
}

/// 单条评论或楼中楼回复（对外只读模型）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyItem {
    pub rpid: u64,
    pub oid: u64,
    pub root: u64,
    pub parent: u64,
    pub mid: u64,
    pub uname: String,
    pub avatar: String,
    pub level: u8,
    pub message: String,
    pub ctime: i64,
    pub like: u64,
    /// 楼中楼总回复数（`rcount`）。
    pub rcount: u64,
    /// 相对时间描述（如 `"2天前"`；为空时 UI 回退 `ctime` 格式化）。
    pub time_desc: String,
    /// 归属地（已去 `"IP属地："` 前缀，如 `"上海"`）。
    pub location: String,
    pub is_up_top: bool,
    pub up_liked: bool,
    pub up_replied: bool,
    /// 主楼自带的楼中楼预览（0~3 条）。
    pub replies: Vec<ReplyItem>,
}

/// `/x/v2/reply/main` 一页结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReplyPage {
    pub upper_mid: u64,
    pub top_replies: Vec<ReplyItem>,
    pub replies: Vec<ReplyItem>,
    pub cursor: ReplyCursor,
}

impl ReplyPage {
    pub fn has_more(&self) -> bool {
        !self.cursor.is_end
    }
}

/// `/x/v2/reply/reply` 一页楼中楼结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SubReplyPage {
    pub upper_mid: u64,
    pub root: Option<ReplyItem>,
    pub replies: Vec<ReplyItem>,
    pub page_num: u32,
    pub page_size: u32,
    pub total_count: u64,
    pub has_more: bool,
}

// ---------- 原始响应模型（私有，宽松防 null） ----------

#[derive(Debug, Default, Deserialize)]
struct RawPaginationReply {
    #[serde(default)]
    next_offset: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawCursor {
    #[serde(default, deserialize_with = "i64_any_or_zero")]
    next: i64,
    #[serde(default)]
    is_end: bool,
    #[serde(default, deserialize_with = "u64_any_or_zero")]
    all_count: u64,
    #[serde(default)]
    pagination_reply: Option<RawPaginationReply>,
}

#[derive(Debug, Default, Deserialize)]
struct RawUpper {
    #[serde(default, deserialize_with = "u64_any_or_zero")]
    mid: u64,
}

#[derive(Debug, Default, Deserialize)]
struct RawLevelInfo {
    #[serde(default, deserialize_with = "u64_any_or_zero")]
    current_level: u64,
}

#[derive(Debug, Default, Deserialize)]
struct RawReplyMember {
    #[serde(default, deserialize_with = "u64_any_or_zero")]
    mid: u64,
    #[serde(default)]
    uname: Option<String>,
    #[serde(default)]
    avatar: Option<String>,
    #[serde(default)]
    level_info: Option<RawLevelInfo>,
}

#[derive(Debug, Default, Deserialize)]
struct RawReplyContent {
    #[serde(default)]
    message: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawUpAction {
    #[serde(default)]
    like: bool,
    #[serde(default)]
    reply: bool,
}

#[derive(Debug, Default, Deserialize)]
struct RawReplyControl {
    #[serde(default)]
    time_desc: Option<String>,
    #[serde(default)]
    location: Option<String>,
    #[serde(default)]
    is_up_top: bool,
    #[serde(default)]
    up_reply: bool,
}

#[derive(Debug, Default, Deserialize)]
struct RawReply {
    #[serde(default, deserialize_with = "u64_any_or_zero")]
    rpid: u64,
    #[serde(default, deserialize_with = "u64_any_or_zero")]
    oid: u64,
    #[serde(default, deserialize_with = "u64_any_or_zero")]
    mid: u64,
    #[serde(default, deserialize_with = "u64_any_or_zero")]
    root: u64,
    #[serde(default, deserialize_with = "u64_any_or_zero")]
    parent: u64,
    #[serde(default, deserialize_with = "u64_any_or_zero")]
    rcount: u64,
    #[serde(default, deserialize_with = "i64_any_or_zero")]
    ctime: i64,
    #[serde(default, deserialize_with = "u64_any_or_zero")]
    like: u64,
    #[serde(default)]
    member: Option<RawReplyMember>,
    #[serde(default)]
    content: Option<RawReplyContent>,
    #[serde(default)]
    up_action: Option<RawUpAction>,
    #[serde(default)]
    reply_control: Option<RawReplyControl>,
    #[serde(default)]
    replies: Option<Vec<RawReply>>,
}

#[derive(Debug, Default, Deserialize)]
struct RawMainData {
    #[serde(default)]
    cursor: Option<RawCursor>,
    #[serde(default)]
    upper: Option<RawUpper>,
    #[serde(default)]
    top_replies: Option<Vec<RawReply>>,
    #[serde(default)]
    replies: Option<Vec<RawReply>>,
}

#[derive(Debug, Default, Deserialize)]
struct RawSubPage {
    #[serde(default, deserialize_with = "u64_any_or_zero")]
    num: u64,
    #[serde(default, deserialize_with = "u64_any_or_zero")]
    size: u64,
    #[serde(default, deserialize_with = "u64_any_or_zero")]
    count: u64,
}

#[derive(Debug, Default, Deserialize)]
struct RawSubData {
    #[serde(default)]
    page: Option<RawSubPage>,
    #[serde(default)]
    upper: Option<RawUpper>,
    #[serde(default)]
    root: Option<RawReply>,
    #[serde(default)]
    replies: Option<Vec<RawReply>>,
}

// ---------- 客户端 ----------

/// 视频评论区客户端（复用共享凭据、`bili_ticket` 与防盗链请求头）。
pub struct ReplyClient {
    http: reqwest::Client,
    creds: Arc<dyn CredentialProvider>,
    ticket: Option<Arc<TicketManager>>,
}

impl ReplyClient {
    pub fn new(http: reqwest::Client, creds: Arc<dyn CredentialProvider>) -> Self {
        Self::with_ticket(http, creds, None)
    }

    pub fn with_nav(
        http: reqwest::Client,
        creds: Arc<dyn CredentialProvider>,
        nav: Arc<NavClient>,
    ) -> Self {
        Self::with_ticket(http, creds, nav.ticket().cloned())
    }

    pub fn with_ticket(
        http: reqwest::Client,
        creds: Arc<dyn CredentialProvider>,
        ticket: Option<Arc<TicketManager>>,
    ) -> Self {
        Self {
            http,
            creds,
            ticket,
        }
    }

    /// 拉取视频主楼评论一页（`cursor = None` 为首页）。
    pub async fn fetch_main(
        &self,
        aid: u64,
        sort: ReplySort,
        cursor: Option<&ReplyCursor>,
    ) -> Result<ReplyPage> {
        if aid == 0 {
            return Err(BiliError::InvalidData("缺少稿件 aid".into()));
        }
        let url = build_main_url(aid, sort, cursor);
        let raw: RawMainData =
            get_json_with_ticket(&self.http, &self.creds, self.ticket.as_deref(), &url).await?;
        Ok(parse_main_page(raw))
    }

    /// 拉取某条主楼评论下的楼中楼分页（`pn` 从 1 起）。
    pub async fn fetch_sub(
        &self,
        aid: u64,
        root_rpid: u64,
        pn: u32,
        ps: u32,
    ) -> Result<SubReplyPage> {
        if aid == 0 || root_rpid == 0 {
            return Err(BiliError::InvalidData("缺少稿件 aid 或根评论 rpid".into()));
        }
        let pn = pn.max(1);
        let ps = ps.clamp(1, 20);
        let url = build_sub_url(aid, root_rpid, pn, ps);
        let raw: RawSubData =
            get_json_with_ticket(&self.http, &self.creds, self.ticket.as_deref(), &url).await?;
        Ok(parse_sub_page(raw, pn, ps))
    }

    /// 发布主楼评论（U1）：成功返回新 `rpid`。登录态必需（csrf=bili_jct）。
    pub async fn add(&self, aid: u64, message: &str) -> Result<u64> {
        let trimmed = message.trim();
        if aid == 0 {
            return Err(BiliError::InvalidData("缺少稿件 aid".into()));
        }
        if trimmed.is_empty() {
            return Err(BiliError::InvalidData("评论内容为空".into()));
        }
        if trimmed.chars().count() > 1000 {
            return Err(BiliError::InvalidData("评论过长(上限 1000 字)".into()));
        }
        let csrf = self.creds.get().await?.bili_jct;
        let form = reply_add_params(aid, trimmed, &csrf);
        #[derive(serde::Deserialize)]
        struct RawAddData {
            #[serde(default, deserialize_with = "u64_any_or_zero")]
            rpid: u64,
        }
        let data: RawAddData = crate::httpx::post_form_with_ticket(
            &self.http,
            &self.creds,
            self.ticket.as_deref(),
            REPLY_ADD_URL,
            "https://www.bilibili.com/",
            &form,
        )
        .await?;
        Ok(data.rpid)
    }
}

/// 发评表单参数（纯函数;单测锁字段面;消息先去首尾空白防服务端 422）。
fn reply_add_params(aid: u64, message: &str, csrf: &str) -> Vec<(String, String)> {
    vec![
        ("type".into(), "1".into()),
        ("oid".into(), aid.to_string()),
        ("root".into(), "0".into()),
        ("parent".into(), "0".into()),
        ("message".into(), message.trim().to_string()),
        ("csrf".into(), csrf.to_string()),
    ]
}

fn build_main_url(aid: u64, sort: ReplySort, cursor: Option<&ReplyCursor>) -> String {
    let mut url = format!("{REPLY_MAIN_URL}?type=1&oid={aid}&mode={}", sort.mode());
    if let Some(c) = cursor {
        url.push_str(&format!("&next={}", c.next));
        if let Some(offset) = c.next_offset.as_deref().filter(|s| !s.is_empty()) {
            let json = serde_json::json!({ "offset": offset }).to_string();
            url.push_str("&pagination_str=");
            url.push_str(&encode_component(&json));
        }
    }
    url
}

fn build_sub_url(aid: u64, root_rpid: u64, pn: u32, ps: u32) -> String {
    format!("{REPLY_SUB_URL}?type=1&oid={aid}&root={root_rpid}&pn={pn}&ps={ps}")
}

fn normalize_avatar_url(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if let Some(rest) = trimmed.strip_prefix("//") {
        format!("https://{rest}")
    } else {
        trimmed.replacen("http://", "https://", 1)
    }
}

fn normalize_location(raw: &str) -> String {
    let trimmed = raw.trim();
    trimmed
        .strip_prefix("IP属地：")
        .or_else(|| trimmed.strip_prefix("IP属地:"))
        .unwrap_or(trimmed)
        .trim()
        .to_string()
}

fn normalize_time_desc(raw: &str) -> String {
    let trimmed = raw.trim();
    trimmed
        .strip_suffix("发布")
        .unwrap_or(trimmed)
        .trim()
        .to_string()
}

fn convert_reply(raw: RawReply, force_top: bool, include_children: bool) -> Option<ReplyItem> {
    if raw.rpid == 0 {
        return None;
    }
    let member = raw.member.unwrap_or_default();
    let mid = if member.mid > 0 { member.mid } else { raw.mid };
    let uname = member
        .uname
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "未知用户".to_string());
    let avatar = member
        .avatar
        .as_deref()
        .map(normalize_avatar_url)
        .unwrap_or_default();
    let level = member
        .level_info
        .map_or(0, |l| l.current_level.min(u64::from(u8::MAX)) as u8);
    let message = raw.content.and_then(|c| c.message).unwrap_or_default();
    let up_action = raw.up_action.unwrap_or_default();
    let ctrl = raw.reply_control.unwrap_or_default();
    let replies = if include_children {
        raw.replies
            .unwrap_or_default()
            .into_iter()
            .filter_map(|child| convert_reply(child, false, false))
            .collect()
    } else {
        Vec::new()
    };

    Some(ReplyItem {
        rpid: raw.rpid,
        oid: raw.oid,
        root: raw.root,
        parent: raw.parent,
        mid,
        uname,
        avatar,
        level,
        message,
        ctime: raw.ctime,
        like: raw.like,
        rcount: raw.rcount,
        time_desc: ctrl
            .time_desc
            .as_deref()
            .map(normalize_time_desc)
            .unwrap_or_default(),
        location: ctrl
            .location
            .as_deref()
            .map(normalize_location)
            .unwrap_or_default(),
        is_up_top: force_top || ctrl.is_up_top,
        up_liked: up_action.like,
        up_replied: up_action.reply || ctrl.up_reply,
        replies,
    })
}

fn parse_main_page(raw: RawMainData) -> ReplyPage {
    let cursor_raw = raw.cursor.unwrap_or_default();
    let cursor = ReplyCursor {
        next: cursor_raw.next,
        next_offset: cursor_raw
            .pagination_reply
            .and_then(|p| p.next_offset)
            .filter(|s| !s.is_empty()),
        is_end: cursor_raw.is_end,
        all_count: cursor_raw.all_count,
    };
    let upper_mid = raw.upper.map_or(0, |u| u.mid);
    let top_replies = raw
        .top_replies
        .unwrap_or_default()
        .into_iter()
        .filter_map(|r| convert_reply(r, true, true))
        .collect();
    let replies = raw
        .replies
        .unwrap_or_default()
        .into_iter()
        .filter_map(|r| convert_reply(r, false, true))
        .collect();
    ReplyPage {
        upper_mid,
        top_replies,
        replies,
        cursor,
    }
}

fn parse_sub_page(raw: RawSubData, fallback_pn: u32, fallback_ps: u32) -> SubReplyPage {
    let upper_mid = raw.upper.map_or(0, |u| u.mid);
    let root = raw.root.and_then(|r| convert_reply(r, false, false));
    let replies: Vec<ReplyItem> = raw
        .replies
        .unwrap_or_default()
        .into_iter()
        .filter_map(|r| convert_reply(r, false, false))
        .collect();
    let page = raw.page.unwrap_or_default();
    let page_num = u32::try_from(page.num)
        .ok()
        .filter(|&n| n > 0)
        .unwrap_or(fallback_pn);
    let page_size = u32::try_from(page.size)
        .ok()
        .filter(|&s| s > 0)
        .unwrap_or(fallback_ps);
    let total_count = page.count;
    let has_more = !replies.is_empty()
        && u64::from(page_num).saturating_mul(u64::from(page_size)) < total_count;
    SubReplyPage {
        upper_mid,
        root,
        replies,
        page_num,
        page_size,
        total_count,
        has_more,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reply_add_params_locks_the_write_contract() {
        let form = reply_add_params(80433022, "  一条评论  ", "SECRET");
        assert_eq!(form[0], ("type".into(), "1".into()));
        assert_eq!(form[1], ("oid".into(), "80433022".into()));
        assert_eq!(form[2], ("root".into(), "0".into()));
        assert_eq!(form[4], ("message".into(), "一条评论".into()));
        assert_eq!(form[5], ("csrf".into(), "SECRET".into()));
    }

    /// 发评输入校验(空/超长在客户端先拒,不发网络)。
    #[test]
    fn reply_add_rejects_empty_and_oversized_messages() {
        let client = ReplyClient::new(
            reqwest::Client::new(),
            std::sync::Arc::new(AnonymousCreds),
        );
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let err = client.add(80433022, "   ").await.unwrap_err().to_string();
            assert!(err.contains("为空"), "{err}");
            let long = "弹".repeat(1001);
            let err = client.add(80433022, &long).await.unwrap_err().to_string();
            assert!(err.contains("过长"), "{err}");
        });
    }

    struct AnonymousCreds;
    #[async_trait::async_trait]
    impl crate::credentials::CredentialProvider for AnonymousCreds {
        async fn get(&self) -> Result<crate::credentials::Credentials> {
            Err(BiliError::NotLoggedIn)
        }
        fn cached(&self) -> Option<crate::credentials::Credentials> {
            None
        }
    }

    #[test]
    fn builds_main_and_sub_urls_with_optional_cursor() {
        assert_eq!(
            build_main_url(80433022, ReplySort::Hot, None),
            "https://api.bilibili.com/x/v2/reply/main?type=1&oid=80433022&mode=3"
        );
        let cursor = ReplyCursor {
            next: 204829,
            next_offset: Some("CAE=".to_string()),
            is_end: false,
            all_count: 100,
        };
        let paged = build_main_url(80433022, ReplySort::Latest, Some(&cursor));
        assert!(paged.starts_with("https://api.bilibili.com/x/v2/reply/main?type=1&oid=80433022&mode=2&next=204829&pagination_str="));
        assert!(paged.contains("%7B%22offset%22%3A%22CAE%3D%22%7D"));
        assert_eq!(
            build_sub_url(80433022, 3760801399, 2, 10),
            "https://api.bilibili.com/x/v2/reply/reply?type=1&oid=80433022&root=3760801399&pn=2&ps=10"
        );
    }

    #[test]
    fn parses_main_reply_response_with_top_replies_and_nulls() {
        let json = serde_json::json!({
            "cursor": {
                "next": 2,
                "is_end": false,
                "all_count": 241210,
                "pagination_reply": { "next_offset": "CAEiAggC" }
            },
            "upper": { "mid": 486906719 },
            "top_replies": [{
                "rpid": 999,
                "oid": 80433022,
                "mid": 486906719,
                "ctime": 1700000000,
                "like": 1200,
                "rcount": 5,
                "member": {
                    "mid": "486906719",
                    "uname": "索尼音乐中国",
                    "avatar": "http://i2.hdslb.com/bfs/face/up.jpg",
                    "level_info": { "current_level": 6 }
                },
                "content": { "message": "置顶说明" },
                "up_action": { "like": true, "reply": false },
                "reply_control": {
                    "is_up_top": true,
                    "time_desc": "2天前发布",
                    "location": "IP属地：上海"
                },
                "replies": null
            }],
            "replies": [{
                "rpid": 3760801399u64,
                "oid": 80433022,
                "mid": 320773657,
                "ctime": 1606652805,
                "like": 777509,
                "rcount": 2295,
                "member": {
                    "mid": "320773657",
                    "uname": "老咸鱼A",
                    "avatar": "//i1.hdslb.com/bfs/face/a.jpg",
                    "level_info": { "current_level": 6 }
                },
                "content": { "message": "好听" },
                "up_action": { "like": false, "reply": true },
                "reply_control": {
                    "time_desc": "2178天前发布",
                    "location": null
                },
                "replies": [{
                    "rpid": 4065967309u64,
                    "oid": 80433022,
                    "root": 3760801399u64,
                    "parent": 3760801399u64,
                    "mid": 495970426,
                    "ctime": 1612546823,
                    "like": 42307,
                    "member": {
                        "uname": "-Megumin_",
                        "avatar": "https://i1.hdslb.com/bfs/face/b.jpg",
                        "level_info": { "current_level": 5 }
                    },
                    "content": { "message": "确实" },
                    "reply_control": { "time_desc": "2109天前发布" }
                }]
            }]
        });
        let raw: RawMainData = serde_json::from_value(json).unwrap();
        let page = parse_main_page(raw);

        assert_eq!(page.upper_mid, 486906719);
        assert_eq!(page.cursor.all_count, 241210);
        assert_eq!(page.cursor.next, 2);
        assert_eq!(page.cursor.next_offset.as_deref(), Some("CAEiAggC"));
        assert!(page.has_more());

        assert_eq!(page.top_replies.len(), 1);
        let top = &page.top_replies[0];
        assert!(top.is_up_top);
        assert!(top.up_liked);
        assert_eq!(top.avatar, "https://i2.hdslb.com/bfs/face/up.jpg");
        assert_eq!(top.time_desc, "2天前");
        assert_eq!(top.location, "上海");

        assert_eq!(page.replies.len(), 1);
        let r = &page.replies[0];
        assert_eq!(r.rpid, 3760801399);
        assert_eq!(r.mid, 320773657);
        assert_eq!(r.avatar, "https://i1.hdslb.com/bfs/face/a.jpg");
        assert_eq!(r.level, 6);
        assert!(r.up_replied);
        assert_eq!(r.location, "");
        assert_eq!(r.replies.len(), 1);
        assert_eq!(r.replies[0].uname, "-Megumin_");
    }

    #[test]
    fn parses_sub_reply_page_and_computes_has_more() {
        let json = serde_json::json!({
            "page": { "num": 1, "size": 10, "count": 25 },
            "upper": { "mid": 486906719 },
            "replies": [
                {
                    "rpid": 4012538137u64,
                    "root": 3760801399u64,
                    "parent": 3760801399u64,
                    "mid": 441145305,
                    "ctime": 1611752781,
                    "like": 1556,
                    "member": {
                        "mid": "441145305",
                        "uname": "WtfWasThat",
                        "avatar": "https://i0.hdslb.com/bfs/face/sub.jpg",
                        "level_info": { "current_level": 6 }
                    },
                    "content": { "message": "[支持]" },
                    "reply_control": { "time_desc": "2119天前发布", "location": "IP属地：北京" }
                }
            ]
        });
        let raw: RawSubData = serde_json::from_value(json).unwrap();
        let page = parse_sub_page(raw, 1, 10);
        assert_eq!(page.upper_mid, 486906719);
        assert_eq!(page.total_count, 25);
        assert!(page.has_more);
        assert_eq!(page.replies[0].location, "北京");

        let last_json = serde_json::json!({
            "page": { "num": 3, "size": 10, "count": 25 },
            "replies": [{ "rpid": 1, "content": { "message": "end" } }]
        });
        let last = parse_sub_page(serde_json::from_value(last_json).unwrap(), 3, 10);
        assert!(!last.has_more);
    }

    #[tokio::test]
    #[ignore = "需访问真实 B 站接口（验证 BV1GJ411x7h7 评论区）"]
    async fn live_reply_smoke() {
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
        let client = ReplyClient::new(http, creds);
        let p1 = client
            .fetch_main(80433022, ReplySort::Hot, None)
            .await
            .unwrap();
        assert!(!p1.replies.is_empty());
        assert!(p1.cursor.all_count > 0);
        let sub = client
            .fetch_sub(80433022, p1.replies[0].rpid, 1, 5)
            .await
            .unwrap();
        assert!(!sub.replies.is_empty());
        let p2 = client
            .fetch_main(80433022, ReplySort::Hot, Some(&p1.cursor))
            .await
            .unwrap();
        assert!(!p2.replies.is_empty());
        assert_ne!(p1.replies[0].rpid, p2.replies[0].rpid);
    }
}
