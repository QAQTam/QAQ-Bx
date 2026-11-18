//! 通知中心与私信客户端（回复 / @ / 赞 / 系统通知 / 私信会话与消息）。
//!
//! 端点均实测自 B 站 web 端与 `message.bilibili.com` 前端 bundle（2026-11-16）：
//! - 未读统计：`api.vc.bilibili.com/x/im/web/msgfeed/unread` + `session_svr/single_unread` + `link_setting/get`
//! - 互动通知：`api.bilibili.com/x/msgfeed/{reply,at,like}`
//! - 系统通知：`message.bilibili.com/x/sys-msg/{query_unified_notify,query_user_notify,query_notify_list}`
//! - 私信会话：`api.vc.bilibili.com/session_svr/v1/session_svr/get_sessions`
//!   + `api.bilibili.com/x/polymer/pc-electron/v1/user/cards`
//!   + `api.vc.bilibili.com/svr_sync/v1/svr_sync/fetch_session_msgs`（WBI 签名）

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use serde::Deserialize;

use crate::account::NavClient;
use crate::credentials::CredentialProvider;
use crate::error::Result;
use crate::httpx::get_json_with_referer;
use crate::loose::{i32_any, i64_any, u64_any};
use crate::ticket::TicketManager;
use crate::wbi::WbiSigner;

const MESSAGE_REFERER: &str = "https://message.bilibili.com/";

const MSGFEED_UNREAD_URL: &str =
    "https://api.vc.bilibili.com/x/im/web/msgfeed/unread?build=0&mobi_app=web";
const SINGLE_UNREAD_URL: &str = "https://api.vc.bilibili.com/session_svr/v1/session_svr/single_unread?build=0&mobi_app=web&unread_type=0";
const LINK_SETTING_URL: &str = "https://api.vc.bilibili.com/link_setting/v1/link_setting/get?build=0&mobi_app=web&msg_notify=1&show_unfollowed_msg=1";

const REPLY_FEED_URL: &str =
    "https://api.bilibili.com/x/msgfeed/reply?platform=web&build=0&mobi_app=web";
const AT_FEED_URL: &str = "https://api.bilibili.com/x/msgfeed/at?platform=web&build=0&mobi_app=web";
const LIKE_FEED_URL: &str =
    "https://api.bilibili.com/x/msgfeed/like?platform=web&build=0&mobi_app=web";

const UNIFIED_NOTIFY_URL: &str =
    "https://message.bilibili.com/x/sys-msg/query_unified_notify?page_size=10&build=0&mobi_app=web";
const USER_NOTIFY_URL: &str =
    "https://message.bilibili.com/x/sys-msg/query_user_notify?page_size=20&build=0&mobi_app=web";
const NOTIFY_LIST_URL: &str =
    "https://message.bilibili.com/x/sys-msg/query_notify_list?data_type=1&build=0&mobi_app=web";

const GET_SESSIONS_URL: &str =
    "https://api.vc.bilibili.com/session_svr/v1/session_svr/get_sessions";
const USER_CARDS_URL: &str = "https://api.bilibili.com/x/polymer/pc-electron/v1/user/cards";
const FETCH_SESSION_MSGS_URL: &str =
    "https://api.vc.bilibili.com/svr_sync/v1/svr_sync/fetch_session_msgs";

// ---------- 公开视图契约 ----------

/// 各通知分类的未读数量汇总。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UnreadSummary {
    pub reply: u64,
    pub at: u64,
    pub like: u64,
    pub system: u64,
    pub whisper: u64,
}

impl UnreadSummary {
    pub fn total(&self) -> u64 {
        self.reply
            .saturating_add(self.at)
            .saturating_add(self.like)
            .saturating_add(self.system)
            .saturating_add(self.whisper)
    }
}

/// 互动流游标（用于分页续拉）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FeedCursor {
    pub id: u64,
    pub time: i64,
}

/// 通知内提取的可跳转链接（命中视频 `bvid` 时可直接在应用内播放）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoticeLink {
    pub label: String,
    pub url: String,
    pub bvid: Option<String>,
}

/// 「回复我的」一条通知。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyNotice {
    pub id: u64,
    pub sender_mid: u64,
    pub sender_name: String,
    pub sender_avatar: String,
    pub action_label: String,
    pub content: String,
    pub quote: Option<String>,
    pub time: i64,
    pub url: String,
    pub bvid: Option<String>,
}

/// 「@我的」一条通知。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtNotice {
    pub id: u64,
    pub sender_mid: u64,
    pub sender_name: String,
    pub sender_avatar: String,
    pub action_label: String,
    pub content: String,
    pub quote: Option<String>,
    pub time: i64,
    pub url: String,
    pub bvid: Option<String>,
}

/// 「收到的赞」一条通知。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LikeNotice {
    pub id: u64,
    pub is_latest: bool,
    pub sender_mid: u64,
    pub sender_name: String,
    pub sender_avatar: String,
    pub total_count: u64,
    pub action_label: String,
    pub target_text: String,
    pub time: i64,
    pub url: String,
    pub bvid: Option<String>,
}

/// 「系统通知」一条通知。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemNotice {
    pub id: u64,
    pub cursor: u64,
    pub title: String,
    pub content: String,
    pub time_at: String,
    pub links: Vec<NoticeLink>,
}

/// 私信会话列表项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhisperSession {
    pub talker_id: u64,
    pub session_type: i32,
    pub talker_name: String,
    pub talker_avatar: String,
    pub preview: String,
    pub last_timestamp: i64,
    pub session_ts: u64,
    pub unread_count: u64,
    pub is_system_account: bool,
}

/// 单条私信消息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhisperMessage {
    pub msg_key: u64,
    pub sender_uid: u64,
    pub msg_type: i32,
    pub text: String,
    pub timestamp: i64,
    pub link: Option<NoticeLink>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedPage<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<FeedCursor>,
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemPage {
    pub items: Vec<SystemNotice>,
    pub next_cursor: Option<u64>,
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionPage {
    pub sessions: Vec<WhisperSession>,
    pub end_ts: Option<u64>,
    pub has_more: bool,
}

// ---------- 原始响应模型（私有，宽松解析） ----------

#[derive(Debug, Default, Deserialize)]
struct MsgFeedUnreadData {
    #[serde(default, deserialize_with = "u64_any")]
    at: u64,
    #[serde(default, deserialize_with = "u64_any")]
    like: u64,
    #[serde(default, deserialize_with = "u64_any")]
    reply: u64,
    #[serde(default, deserialize_with = "u64_any")]
    sys_msg: u64,
}

#[derive(Debug, Default, Deserialize)]
struct SingleUnreadData {
    #[serde(default, deserialize_with = "u64_any")]
    unfollow_unread: u64,
    #[serde(default, deserialize_with = "u64_any")]
    follow_unread: u64,
    #[serde(default, deserialize_with = "u64_any")]
    biz_msg_unfollow_unread: u64,
    #[serde(default, deserialize_with = "u64_any")]
    biz_msg_follow_unread: u64,
}

#[derive(Debug, Default, Deserialize)]
struct LinkSettingData {
    #[serde(default, deserialize_with = "i64_any")]
    show_unfollowed_msg: i64,
}

#[derive(Debug, Default, Deserialize)]
struct RawCursor {
    #[serde(default)]
    is_end: bool,
    #[serde(default, deserialize_with = "u64_any")]
    id: u64,
    #[serde(default, deserialize_with = "i64_any")]
    time: i64,
}

#[derive(Debug, Default, Deserialize)]
struct RawUser {
    #[serde(default, deserialize_with = "u64_any")]
    mid: u64,
    #[serde(default)]
    nickname: String,
    #[serde(default)]
    avatar: String,
}

#[derive(Debug, Default, Deserialize)]
struct RawReplyDetail {
    #[serde(default, deserialize_with = "u64_any")]
    target_id: u64,
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    business: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    desc: String,
    #[serde(default)]
    uri: String,
    #[serde(default)]
    source_content: String,
    #[serde(default)]
    target_reply_content: String,
}

#[derive(Debug, Default, Deserialize)]
struct RawReplyItem {
    #[serde(default, deserialize_with = "u64_any")]
    id: u64,
    #[serde(default)]
    user: Option<RawUser>,
    #[serde(default)]
    item: Option<RawReplyDetail>,
    #[serde(default, deserialize_with = "u64_any")]
    counts: u64,
    #[serde(default)]
    is_multi: bool,
    #[serde(default, deserialize_with = "i64_any")]
    reply_time: i64,
}

#[derive(Debug, Default, Deserialize)]
struct RawReplyResp {
    #[serde(default)]
    cursor: RawCursor,
    #[serde(default)]
    items: Option<Vec<RawReplyItem>>,
}

#[derive(Debug, Default, Deserialize)]
struct RawAtItem {
    #[serde(default, deserialize_with = "u64_any")]
    id: u64,
    #[serde(default)]
    user: Option<RawUser>,
    #[serde(default)]
    item: Option<RawReplyDetail>,
    #[serde(default, deserialize_with = "i64_any")]
    at_time: i64,
}

#[derive(Debug, Default, Deserialize)]
struct RawAtResp {
    #[serde(default)]
    cursor: RawCursor,
    #[serde(default)]
    items: Option<Vec<RawAtItem>>,
}

#[derive(Debug, Default, Deserialize)]
struct RawLikeItem {
    #[serde(default, deserialize_with = "u64_any")]
    id: u64,
    #[serde(default)]
    users: Option<Vec<RawUser>>,
    #[serde(default)]
    item: Option<RawReplyDetail>,
    #[serde(default, deserialize_with = "u64_any")]
    counts: u64,
    #[serde(default, deserialize_with = "i64_any")]
    like_time: i64,
}

#[derive(Debug, Default, Deserialize)]
struct RawLikeLatest {
    #[serde(default)]
    items: Option<Vec<RawLikeItem>>,
}

#[derive(Debug, Default, Deserialize)]
struct RawLikeTotal {
    #[serde(default)]
    cursor: RawCursor,
    #[serde(default)]
    items: Option<Vec<RawLikeItem>>,
}

#[derive(Debug, Default, Deserialize)]
struct RawLikeResp {
    #[serde(default)]
    latest: Option<RawLikeLatest>,
    #[serde(default)]
    total: Option<RawLikeTotal>,
}

#[derive(Debug, Default, Deserialize)]
struct RawSysNotifyItem {
    #[serde(default, deserialize_with = "u64_any")]
    id: u64,
    #[serde(default, deserialize_with = "u64_any")]
    cursor: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    time_at: String,
}

#[derive(Debug, Default, Deserialize)]
struct RawSysNotifyWrap {
    #[serde(default)]
    system_notify_list: Option<Vec<RawSysNotifyItem>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RawAccountInfo {
    #[serde(default)]
    name: String,
    #[serde(default)]
    pic_url: String,
}

#[derive(Debug, Default, Deserialize)]
struct RawWhisperMsg {
    #[serde(default, deserialize_with = "u64_any")]
    sender_uid: u64,
    #[serde(default, deserialize_with = "i32_any")]
    msg_type: i32,
    #[serde(default)]
    content: String,
    #[serde(default, deserialize_with = "i64_any")]
    timestamp: i64,
    #[serde(default, deserialize_with = "u64_any")]
    msg_key: u64,
    #[serde(default, deserialize_with = "i32_any")]
    msg_status: i32,
    #[serde(default)]
    account_info: Option<RawAccountInfo>,
}

#[derive(Debug, Default, Deserialize)]
struct RawSession {
    #[serde(default, deserialize_with = "u64_any")]
    talker_id: u64,
    #[serde(default, deserialize_with = "i32_any")]
    session_type: i32,
    #[serde(default, deserialize_with = "u64_any")]
    session_ts: u64,
    #[serde(default, deserialize_with = "u64_any")]
    unread_count: u64,
    #[serde(default, deserialize_with = "u64_any")]
    biz_msg_unread_count: u64,
    #[serde(default)]
    last_msg: Option<RawWhisperMsg>,
    #[serde(default)]
    account_info: Option<RawAccountInfo>,
}

#[derive(Debug, Default, Deserialize)]
struct RawSessionsResp {
    #[serde(default)]
    session_list: Option<Vec<RawSession>>,
    #[serde(default, deserialize_with = "i64_any")]
    has_more: i64,
}

#[derive(Debug, Default, Deserialize)]
struct RawUserCard {
    #[serde(default)]
    name: String,
    #[serde(default)]
    face: String,
}

#[derive(Debug, Default, Deserialize)]
struct RawSessionMsgsResp {
    #[serde(default)]
    messages: Option<Vec<RawWhisperMsg>>,
}

// ---------- 客户端 ----------

/// 通知与私信客户端（Cookie + `bili_ticket` + 共享 `NavClient`）。
pub struct NotifyClient {
    http: reqwest::Client,
    creds: Arc<dyn CredentialProvider>,
    ticket: Option<Arc<TicketManager>>,
    wbi: WbiSigner,
    /// 0=未探测 1=折叠未关注人私信(false) 2=展示未关注人私信(true)
    show_unfollowed_cache: AtomicU8,
}

impl NotifyClient {
    pub fn new(http: reqwest::Client, creds: Arc<dyn CredentialProvider>) -> Self {
        let nav = Arc::new(NavClient::new(http.clone(), creds.clone()));
        Self::with_nav(http, creds, nav)
    }

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
            show_unfollowed_cache: AtomicU8::new(0),
        }
    }

    /// 登出 / 切换账号后重置缓存的私信设置标记。
    pub fn invalidate(&self) {
        self.show_unfollowed_cache.store(0, Ordering::Relaxed);
    }

    async fn get_msg<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T> {
        get_json_with_referer(
            &self.http,
            &self.creds,
            self.ticket.as_deref(),
            url,
            MESSAGE_REFERER,
        )
        .await
    }

    /// 并发拉取各通知维度未读数（`msgfeed/unread` + `single_unread` + 一次性的 `link_setting/get`）。
    pub async fn fetch_unread(&self) -> Result<UnreadSummary> {
        let need_setting = self.show_unfollowed_cache.load(Ordering::Relaxed) == 0;
        let (feed_res, dm_res, setting_res) = tokio::join!(
            self.get_msg::<MsgFeedUnreadData>(MSGFEED_UNREAD_URL),
            self.get_msg::<SingleUnreadData>(SINGLE_UNREAD_URL),
            async {
                if need_setting {
                    Some(self.get_msg::<LinkSettingData>(LINK_SETTING_URL).await)
                } else {
                    None
                }
            },
        );
        let feed = feed_res?;
        let dm = dm_res.unwrap_or_default();
        let show_unfollowed = match setting_res {
            Some(Ok(s)) => {
                let flag = s.show_unfollowed_msg == 1;
                self.show_unfollowed_cache
                    .store(if flag { 2 } else { 1 }, Ordering::Relaxed);
                flag
            }
            Some(Err(_)) | None => self.show_unfollowed_cache.load(Ordering::Relaxed) == 2,
        };

        let mut whisper = dm.follow_unread;
        let mut biz = dm.biz_msg_follow_unread;
        if !show_unfollowed {
            whisper = whisper.saturating_add(dm.unfollow_unread);
            biz = biz.saturating_add(dm.biz_msg_unfollow_unread);
        }
        let whisper_total = if whisper > 0 { whisper } else { biz };

        Ok(UnreadSummary {
            reply: feed.reply,
            at: feed.at,
            like: feed.like,
            system: feed.sys_msg,
            whisper: whisper_total,
        })
    }

    /// 拉取「回复我的」列表。
    pub async fn fetch_replies(&self, cursor: Option<FeedCursor>) -> Result<FeedPage<ReplyNotice>> {
        let url = match cursor {
            Some(c) => format!("{REPLY_FEED_URL}&id={}&reply_time={}", c.id, c.time),
            None => REPLY_FEED_URL.to_string(),
        };
        let resp: RawReplyResp = self.get_msg(&url).await?;
        Ok(parse_reply_page(resp))
    }

    /// 拉取「@我的」列表。
    pub async fn fetch_ats(&self, cursor: Option<FeedCursor>) -> Result<FeedPage<AtNotice>> {
        let url = match cursor {
            Some(c) => format!("{AT_FEED_URL}&id={}&at_time={}", c.id, c.time),
            None => AT_FEED_URL.to_string(),
        };
        let resp: RawAtResp = self.get_msg(&url).await?;
        Ok(parse_at_page(resp))
    }

    /// 拉取「收到的赞」列表。
    pub async fn fetch_likes(&self, cursor: Option<FeedCursor>) -> Result<FeedPage<LikeNotice>> {
        let url = match cursor {
            Some(c) => format!("{LIKE_FEED_URL}&id={}&like_time={}", c.id, c.time),
            None => LIKE_FEED_URL.to_string(),
        };
        let resp: RawLikeResp = self.get_msg(&url).await?;
        Ok(parse_like_page(resp))
    }

    /// 拉取「系统通知」列表（首屏聚合全站公告与用户通知；翻页走 `query_notify_list`）。
    pub async fn fetch_system_notifies(&self, cursor: Option<u64>) -> Result<SystemPage> {
        if let Some(c) = cursor {
            let url = format!("{NOTIFY_LIST_URL}&cursor={c}");
            let items: Option<Vec<RawSysNotifyItem>> = self.get_msg(&url).await?;
            return Ok(parse_sys_list(items.unwrap_or_default()));
        }
        let (unified_res, user_res) = tokio::join!(
            self.get_msg::<RawSysNotifyWrap>(UNIFIED_NOTIFY_URL),
            self.get_msg::<RawSysNotifyWrap>(USER_NOTIFY_URL),
        );
        let unified = unified_res
            .ok()
            .and_then(|w| w.system_notify_list)
            .unwrap_or_default();
        let user = user_res?.system_notify_list.unwrap_or_default();
        Ok(merge_initial_sys_notifies(unified, user))
    }

    /// 拉取私信会话列表（WBI 签名），并批量补全普通会话对象的昵称与头像。
    pub async fn fetch_sessions(&self, end_ts: Option<u64>) -> Result<SessionPage> {
        let mut params = vec![
            ("session_type", "1".to_string()),
            ("group_fold", "1".to_string()),
            ("unfollow_fold", "0".to_string()),
            ("sort_rule", "2".to_string()),
            ("build", "0".to_string()),
            ("mobi_app", "web".to_string()),
        ];
        if let Some(ts) = end_ts {
            params.push(("end_ts", ts.to_string()));
        }
        let signed = self.wbi.sign_query(&params).await?;
        let url = format!("{GET_SESSIONS_URL}?{signed}");
        let resp: RawSessionsResp = self.get_msg(&url).await?;
        let raw_list = resp.session_list.unwrap_or_default();
        let has_more = resp.has_more == 1 && !raw_list.is_empty();

        // 收集缺少 account_info 的单聊 talker_id，批量查用户卡片。
        let mut need_uids: Vec<u64> = raw_list
            .iter()
            .filter(|s| {
                s.session_type == 1
                    && s.account_info.is_none()
                    && s.last_msg
                        .as_ref()
                        .and_then(|m| m.account_info.as_ref())
                        .is_none()
                    && s.talker_id > 0
            })
            .map(|s| s.talker_id)
            .collect();
        need_uids.sort_unstable();
        need_uids.dedup();

        let mut user_map: HashMap<String, RawUserCard> = HashMap::new();
        for chunk in need_uids.chunks(50) {
            let uids_param = chunk
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(",");
            let cards_url = format!("{USER_CARDS_URL}?uids={uids_param}&build=0&mobi_app=web");
            if let Ok(map) = self
                .get_msg::<HashMap<String, RawUserCard>>(&cards_url)
                .await
            {
                user_map.extend(map);
            }
        }

        let end_ts = raw_list.last().map(|s| s.session_ts);
        let sessions = raw_list
            .into_iter()
            .map(|s| build_whisper_session(s, &user_map))
            .collect();
        Ok(SessionPage {
            sessions,
            end_ts,
            has_more,
        })
    }

    /// 拉取指定私信会话的消息记录（按时间从旧到新排列）。
    pub async fn fetch_session_messages(
        &self,
        talker_id: u64,
        session_type: i32,
    ) -> Result<Vec<WhisperMessage>> {
        let signed = self
            .wbi
            .sign_query(&[
                ("talker_id", talker_id.to_string()),
                ("session_type", session_type.to_string()),
                ("size", "30".to_string()),
                ("sender_device_id", "1".to_string()),
                ("build", "0".to_string()),
                ("mobi_app", "web".to_string()),
            ])
            .await?;
        let url = format!("{FETCH_SESSION_MSGS_URL}?{signed}");
        let resp: RawSessionMsgsResp = self.get_msg(&url).await?;
        let mut msgs: Vec<WhisperMessage> = resp
            .messages
            .unwrap_or_default()
            .into_iter()
            .map(build_whisper_message)
            .collect();
        msgs.reverse();
        Ok(msgs)
    }
}

// ---------- 纯转换与解析逻辑（供单测锚定） ----------

fn parse_reply_page(resp: RawReplyResp) -> FeedPage<ReplyNotice> {
    let items = resp
        .items
        .unwrap_or_default()
        .into_iter()
        .filter_map(|raw| {
            let detail = raw.item?;
            let user = raw.user.unwrap_or_default();
            let action_label = if detail.kind == "danmu" || detail.business == "弹幕" {
                if detail.target_id > 0 {
                    "回复了弹幕".to_string()
                } else {
                    "评论了弹幕".to_string()
                }
            } else if detail.business.is_empty() || detail.business == "评论" {
                "回复了评论".to_string()
            } else if raw.is_multi && raw.counts > 1 {
                format!("等人评论了{}", detail.business)
            } else {
                format!("评论了{}", detail.business)
            };
            let quote = non_empty(&detail.target_reply_content)
                .or_else(|| non_empty(&detail.title))
                .or_else(|| non_empty(&detail.desc));
            let url = normalize_web_url(&detail.uri);
            let bvid = extract_bvid(&url).or_else(|| extract_bvid(&detail.uri));
            Some(ReplyNotice {
                id: raw.id,
                sender_mid: user.mid,
                sender_name: fallback_name(user.nickname, user.mid),
                sender_avatar: normalize_web_url(&user.avatar),
                action_label,
                content: detail.source_content,
                quote,
                time: raw.reply_time,
                url,
                bvid,
            })
        })
        .collect::<Vec<_>>();
    let has_more = !resp.cursor.is_end && resp.cursor.id > 0;
    let next_cursor = has_more.then_some(FeedCursor {
        id: resp.cursor.id,
        time: resp.cursor.time,
    });
    FeedPage {
        items,
        next_cursor,
        has_more,
    }
}

fn parse_at_page(resp: RawAtResp) -> FeedPage<AtNotice> {
    let items = resp
        .items
        .unwrap_or_default()
        .into_iter()
        .filter_map(|raw| {
            let detail = raw.item?;
            let user = raw.user.unwrap_or_default();
            let action_label = if detail.business.is_empty() {
                "@了我".to_string()
            } else {
                format!("在{}中 @了我", detail.business)
            };
            let quote = non_empty(&detail.target_reply_content)
                .or_else(|| non_empty(&detail.title))
                .or_else(|| non_empty(&detail.desc));
            let url = normalize_web_url(&detail.uri);
            let bvid = extract_bvid(&url).or_else(|| extract_bvid(&detail.uri));
            Some(AtNotice {
                id: raw.id,
                sender_mid: user.mid,
                sender_name: fallback_name(user.nickname, user.mid),
                sender_avatar: normalize_web_url(&user.avatar),
                action_label,
                content: detail.source_content,
                quote,
                time: raw.at_time,
                url,
                bvid,
            })
        })
        .collect::<Vec<_>>();
    let has_more = !resp.cursor.is_end && resp.cursor.id > 0;
    let next_cursor = has_more.then_some(FeedCursor {
        id: resp.cursor.id,
        time: resp.cursor.time,
    });
    FeedPage {
        items,
        next_cursor,
        has_more,
    }
}

fn parse_like_page(resp: RawLikeResp) -> FeedPage<LikeNotice> {
    let mut items = Vec::new();
    let mut seen = HashSet::new();

    if let Some(latest) = resp.latest {
        for raw in latest.items.unwrap_or_default() {
            if let Some(notice) = map_like_item(raw, true)
                && seen.insert(notice.id)
            {
                items.push(notice);
            }
        }
    }

    let mut has_more = false;
    let mut next_cursor = None;
    if let Some(total) = resp.total {
        has_more = !total.cursor.is_end && total.cursor.id > 0;
        if has_more {
            next_cursor = Some(FeedCursor {
                id: total.cursor.id,
                time: total.cursor.time,
            });
        }
        for raw in total.items.unwrap_or_default() {
            if let Some(notice) = map_like_item(raw, false)
                && seen.insert(notice.id)
            {
                items.push(notice);
            }
        }
    }

    FeedPage {
        items,
        next_cursor,
        has_more,
    }
}

fn map_like_item(raw: RawLikeItem, is_latest: bool) -> Option<LikeNotice> {
    let detail = raw.item?;
    let users = raw.users.unwrap_or_default();
    let first = users.into_iter().next().unwrap_or_default();
    let sender_name = fallback_name(first.nickname, first.mid);
    let action_label = if detail.business.is_empty() {
        "赞了我".to_string()
    } else {
        format!("赞了{}", detail.business)
    };
    let target_text = non_empty(&detail.title)
        .or_else(|| non_empty(&detail.desc))
        .or_else(|| non_empty(&detail.target_reply_content))
        .unwrap_or_default();
    let url = normalize_web_url(&detail.uri);
    let bvid = extract_bvid(&url).or_else(|| extract_bvid(&detail.uri));
    Some(LikeNotice {
        id: raw.id,
        is_latest,
        sender_mid: first.mid,
        sender_name,
        sender_avatar: normalize_web_url(&first.avatar),
        total_count: raw.counts.max(1),
        action_label,
        target_text,
        time: raw.like_time,
        url,
        bvid,
    })
}

fn merge_initial_sys_notifies(
    unified: Vec<RawSysNotifyItem>,
    user: Vec<RawSysNotifyItem>,
) -> SystemPage {
    let mut seen = HashSet::new();
    let mut merged: Vec<SystemNotice> = Vec::new();
    for raw in user.into_iter().chain(unified) {
        if seen.insert(raw.id) {
            merged.push(map_sys_item(raw));
        }
    }
    merged.sort_by_key(|a| std::cmp::Reverse(a.cursor));
    let next_cursor = merged.last().map(|n| n.cursor).filter(|&c| c > 0);
    SystemPage {
        has_more: next_cursor.is_some(),
        items: merged,
        next_cursor,
    }
}

fn parse_sys_list(raw_items: Vec<RawSysNotifyItem>) -> SystemPage {
    let items: Vec<SystemNotice> = raw_items.into_iter().map(map_sys_item).collect();
    let next_cursor = items.last().map(|n| n.cursor).filter(|&c| c > 0);
    SystemPage {
        has_more: !items.is_empty() && next_cursor.is_some(),
        items,
        next_cursor,
    }
}

fn map_sys_item(raw: RawSysNotifyItem) -> SystemNotice {
    let (content, links) = parse_sys_content(&raw.content);
    SystemNotice {
        id: raw.id,
        cursor: raw.cursor,
        title: raw.title,
        content,
        time_at: raw.time_at,
        links,
    }
}

/// 解析系统通知正文：解包 `{"web": "..."}`，提取 `#{文本}{URL}` 链接并保留可读正文。
pub fn parse_sys_content(raw: &str) -> (String, Vec<NoticeLink>) {
    #[derive(Deserialize)]
    struct WebWrapped {
        #[serde(default)]
        web: String,
    }

    let source = if raw.trim_start().starts_with('{') {
        serde_json::from_str::<WebWrapped>(raw)
            .ok()
            .map(|w| w.web)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| raw.to_string())
    } else {
        raw.to_string()
    };

    let mut clean = String::with_capacity(source.len());
    let mut links = Vec::new();
    let mut rest = source.as_str();

    while let Some(start) = rest.find("#{") {
        clean.push_str(&rest[..start]);
        let after_hash = &rest[start + 2..];
        let Some(mid) = after_hash.find("}{") else {
            clean.push_str("#{");
            rest = after_hash;
            continue;
        };
        let raw_label = &after_hash[..mid];
        let after_mid = &after_hash[mid + 2..];
        let Some(end) = after_mid.find('}') else {
            clean.push_str("#{");
            rest = after_hash;
            continue;
        };
        let raw_url = after_mid[..end].trim().trim_matches('"');
        let label = raw_label.trim_end_matches('>').trim().to_string();
        let url = normalize_web_url(raw_url);
        if !url.is_empty() {
            let bvid = extract_bvid(&url);
            links.push(NoticeLink {
                label: if label.is_empty() {
                    "打开链接".to_string()
                } else {
                    label.clone()
                },
                url,
                bvid,
            });
        }
        // 结尾引导类按钮文案内联保留会与下方链接重复；若在句尾则略去，句中保留标签。
        let tail = after_mid[end + 1..].trim();
        if !tail.is_empty() && !raw_label.contains("点击") {
            clean.push_str(&label);
        }
        rest = &after_mid[end + 1..];
    }
    clean.push_str(rest);

    (clean.trim().to_string(), links)
}

fn build_whisper_session(
    raw: RawSession,
    user_cards: &HashMap<String, RawUserCard>,
) -> WhisperSession {
    let account = raw
        .account_info
        .or_else(|| raw.last_msg.as_ref().and_then(|m| m.account_info.clone()));
    let is_system_account = account.is_some();

    let (talker_name, talker_avatar) = if let Some(acc) = account {
        (
            fallback_name(acc.name, raw.talker_id),
            normalize_web_url(&acc.pic_url),
        )
    } else if let Some(card) = user_cards.get(&raw.talker_id.to_string()) {
        (
            fallback_name(card.name.clone(), raw.talker_id),
            normalize_web_url(&card.face),
        )
    } else if raw.session_type == 2 {
        (format!("群聊 {}", raw.talker_id), String::new())
    } else {
        (format!("用户 {}", raw.talker_id), String::new())
    };

    let (preview, last_timestamp) = match raw.last_msg {
        Some(msg) => {
            let (text, _) = format_whisper_content(msg.msg_type, msg.msg_status, &msg.content);
            (text, msg.timestamp)
        }
        None => (String::new(), (raw.session_ts / 1_000_000) as i64),
    };

    WhisperSession {
        talker_id: raw.talker_id,
        session_type: raw.session_type,
        talker_name,
        talker_avatar,
        preview,
        last_timestamp,
        session_ts: raw.session_ts,
        unread_count: raw.unread_count.saturating_add(raw.biz_msg_unread_count),
        is_system_account,
    }
}

fn build_whisper_message(raw: RawWhisperMsg) -> WhisperMessage {
    let (text, link) = format_whisper_content(raw.msg_type, raw.msg_status, &raw.content);
    WhisperMessage {
        msg_key: raw.msg_key,
        sender_uid: raw.sender_uid,
        msg_type: raw.msg_type,
        text,
        timestamp: raw.timestamp,
        link,
    }
}

/// 格式化私信消息正文，兼容文本/图片/视频卡/分享卡/通知卡等 B 站 msg_type。
pub fn format_whisper_content(
    msg_type: i32,
    msg_status: i32,
    raw_content: &str,
) -> (String, Option<NoticeLink>) {
    if msg_status == 1 {
        return ("[已撤回]".to_string(), None);
    }

    #[derive(Default, Deserialize)]
    struct MsgPayload {
        #[serde(default)]
        content: String,
        #[serde(default)]
        title: String,
        #[serde(default)]
        text: String,
        #[serde(default)]
        headline: String,
        #[serde(default)]
        reply_content: String,
        #[serde(default)]
        jump_text: String,
        #[serde(default)]
        jump_uri: String,
        #[serde(default)]
        bvid: String,
        #[serde(default)]
        url: String,
        #[serde(default)]
        id: serde_json::Value,
        #[serde(default, deserialize_with = "i64_any")]
        source: i64,
    }

    let parsed: MsgPayload = serde_json::from_str(raw_content).unwrap_or_default();
    let link_from_url = |label: &str, raw_url: &str, bvid_hint: &str| -> Option<NoticeLink> {
        let bvid = (!bvid_hint.is_empty())
            .then(|| bvid_hint.to_string())
            .or_else(|| extract_bvid(raw_url));
        let url = if !raw_url.is_empty() {
            normalize_web_url(raw_url)
        } else if let Some(ref bv) = bvid {
            format!("https://www.bilibili.com/video/{bv}")
        } else {
            return None;
        };
        Some(NoticeLink {
            label: label.to_string(),
            url,
            bvid,
        })
    };

    match msg_type {
        1 => (parsed.content, None),
        2 => ("[图片]".to_string(), None),
        3 => ("[语音]".to_string(), None),
        5 => ("[已撤回]".to_string(), None),
        6 => ("[表情]".to_string(), None),
        7 | 14 => {
            let tag = if parsed.headline.is_empty() {
                "分享"
            } else {
                "转发"
            };
            let title = non_empty(&parsed.title).unwrap_or_default();
            let link = if !parsed.url.is_empty() || !parsed.bvid.is_empty() {
                link_from_url("打开", &parsed.url, &parsed.bvid)
            } else if parsed.source == 5 {
                let aid = parsed
                    .id
                    .as_u64()
                    .or_else(|| parsed.id.as_str()?.parse().ok());
                aid.map(|a| NoticeLink {
                    label: "打开".to_string(),
                    url: format!("https://www.bilibili.com/video/av{a}"),
                    bvid: None,
                })
            } else {
                None
            };
            (format!("[{tag}] {title}").trim().to_string(), link)
        }
        10 => {
            let body = if !parsed.title.is_empty() && !parsed.text.is_empty() {
                format!("{}\n{}", parsed.title, parsed.text)
            } else if !parsed.title.is_empty() {
                parsed.title
            } else {
                parsed.text
            };
            let label = if parsed.jump_text.is_empty() {
                "查看"
            } else {
                parsed.jump_text.as_str()
            };
            let link = link_from_url(label, &parsed.jump_uri, "");
            (body, link)
        }
        11 => {
            let title = non_empty(&parsed.title).unwrap_or_default();
            let link = link_from_url("播放", &parsed.url, &parsed.bvid);
            (format!("[视频] {title}").trim().to_string(), link)
        }
        12 => {
            let title = non_empty(&parsed.title).unwrap_or_default();
            let link = link_from_url("阅读", &parsed.url, "");
            (format!("[专栏] {title}").trim().to_string(), link)
        }
        13 => (
            non_empty(&parsed.title).unwrap_or_else(|| "[动态]".to_string()),
            link_from_url("查看", &parsed.url, ""),
        ),
        16 => (
            non_empty(&parsed.reply_content).unwrap_or_else(|| "[自动回复]".to_string()),
            None,
        ),
        _ => {
            let fallback = non_empty(&parsed.content)
                .or_else(|| non_empty(&parsed.title))
                .unwrap_or_else(|| "[消息]".to_string());
            (fallback, None)
        }
    }
}

/// 从 URL 中提取 `BV...` 视频号（标准 12 位：`BV` + 10 位字母数字）。
pub fn extract_bvid(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut i = 0;
    while i + 12 <= bytes.len() {
        if bytes[i] == b'B' && bytes[i + 1] == b'V' {
            let prev_ok = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
            let candidate = &bytes[i + 2..i + 12];
            let body_ok = candidate.iter().all(|b| b.is_ascii_alphanumeric());
            let next_ok = i + 12 == bytes.len() || !bytes[i + 12].is_ascii_alphanumeric();
            if prev_ok && body_ok && next_ok {
                return Some(input[i..i + 12].to_string());
            }
        }
        i += 1;
    }
    None
}

/// 把 `//...` 或 `http://...` 统一转为可点击 `https://...` URL。
pub fn normalize_web_url(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        String::new()
    } else if let Some(rest) = trimmed.strip_prefix("//") {
        format!("https://{rest}")
    } else if let Some(rest) = trimmed.strip_prefix("http://") {
        format!("https://{rest}")
    } else {
        trimmed.to_string()
    }
}

fn non_empty(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

fn fallback_name(name: String, id: u64) -> String {
    let t = name.trim();
    if t.is_empty() {
        format!("用户 {id}")
    } else {
        t.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_bvid_and_normalizes_urls() {
        assert_eq!(
            extract_bvid("https://www.bilibili.com/video/BV1i24P6rEmt?p=1"),
            Some("BV1i24P6rEmt".into())
        );
        assert_eq!(
            extract_bvid("//www.bilibili.com/video/BV1GJ411x7h7/"),
            Some("BV1GJ411x7h7".into())
        );
        assert_eq!(extract_bvid("https://space.bilibili.com/100000000"), None);
        assert_eq!(extract_bvid("BV1234567890EXTRA"), None);

        assert_eq!(
            normalize_web_url("//message.bilibili.com/#/reply"),
            "https://message.bilibili.com/#/reply"
        );
        assert_eq!(
            normalize_web_url("http://i2.hdslb.com/u_user/a.png"),
            "https://i2.hdslb.com/u_user/a.png"
        );
    }

    #[test]
    fn parses_sys_content_plain_and_json_wrapped_with_links() {
        let plain = "恭喜，您已通过LV6试炼，解锁趣味彩蛋。#{点击查看试炼结果>>}{https://www.bilibili.com/h5/senior-newbie}";
        let (text, links) = parse_sys_content(plain);
        assert_eq!(text, "恭喜，您已通过LV6试炼，解锁趣味彩蛋。");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].label, "点击查看试炼结果");
        assert_eq!(links[0].url, "https://www.bilibili.com/h5/senior-newbie");

        let wrapped = r#"{"web":"赛程已开播! #{点击前往直播间}{\"https://live.bilibili.com/6\"}"}"#;
        let (text2, links2) = parse_sys_content(wrapped);
        assert_eq!(text2, "赛程已开播!");
        assert_eq!(links2.len(), 1);
        assert_eq!(links2[0].label, "点击前往直播间");
        assert_eq!(links2[0].url, "https://live.bilibili.com/6");
    }

    #[test]
    fn parses_like_feed_with_latest_and_total() {
        let json = r#"{
            "latest": {
                "items": [{
                    "id": 100,
                    "users": [{"mid": 1, "nickname": "A", "avatar": "http://i0.hdslb.com/a.jpg"}],
                    "item": {"business": "视频", "title": "演示", "uri": "https://www.bilibili.com/video/BV1GJ411x7h7"},
                    "counts": 1,
                    "like_time": 1790000000
                }]
            },
            "total": {
                "cursor": {"is_end": false, "id": 99, "time": 1789999000},
                "items": [{
                    "id": 99,
                    "users": [{"mid": 2, "nickname": "B", "avatar": "https://i0.hdslb.com/b.jpg"}],
                    "item": {"business": "评论", "title": "好", "uri": "https://www.bilibili.com/video/BV1i24P6rEmt"},
                    "counts": 3,
                    "like_time": 1789999000
                }]
            }
        }"#;
        let parsed: RawLikeResp = serde_json::from_str(json).unwrap();
        let page = parse_like_page(parsed);
        assert_eq!(page.items.len(), 2);
        assert!(page.items[0].is_latest);
        assert_eq!(page.items[0].bvid.as_deref(), Some("BV1GJ411x7h7"));
        assert!(!page.items[1].is_latest);
        assert_eq!(page.items[1].total_count, 3);
        assert!(page.has_more);
        assert_eq!(
            page.next_cursor,
            Some(FeedCursor {
                id: 99,
                time: 1789999000
            })
        );
    }

    #[test]
    fn formats_whisper_payloads_across_msg_types() {
        let (t1, l1) = format_whisper_content(1, 0, r#"{"content":"你好呀"}"#);
        assert_eq!(t1, "你好呀");
        assert!(l1.is_none());

        let (t10, l10) = format_whisper_content(
            10,
            0,
            r#"{"title":"UP主荣誉周报","text":"本周表现","jump_text":"查看周报","jump_uri":"https://member.bilibili.com/york"}"#,
        );
        assert!(t10.contains("UP主荣誉周报"));
        assert_eq!(l10.unwrap().label, "查看周报");

        let (t11, l11) =
            format_whisper_content(11, 0, r#"{"title":"Rust教程","bvid":"BV1GJ411x7h7"}"#);
        assert_eq!(t11, "[视频] Rust教程");
        assert_eq!(l11.unwrap().bvid.as_deref(), Some("BV1GJ411x7h7"));

        let (t_revoked, _) = format_whisper_content(1, 1, r#"{"content":"secret"}"#);
        assert_eq!(t_revoked, "[已撤回]");
    }

    /// 实弹联调：读取本地凭据（data/credentials.json）依次验证 nav/stat 与通知中心全部只读接口。
    /// 跑法：`cargo test -p qaq-bx-core notify::tests::live_smoke -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "需本地已登录凭据（data/credentials.json）且访问真实 B 站接口"]
    async fn live_smoke() {
        use crate::credentials::FileCredentialStore;
        use crate::storage::resolve_data_dir;
        use crate::ticket::{FileTicketStore, TICKET_FILE};

        let data_dir = resolve_data_dir();
        let store = Arc::new(FileCredentialStore::new(data_dir.join("credentials.json")));
        let creds = Arc::new(crate::credentials::DefaultCredentialProvider::new(store));
        let http = reqwest::Client::builder()
            .user_agent(crate::wbi::BROWSER_UA)
            .build()
            .unwrap();
        let ticket_store = Arc::new(FileTicketStore::new(data_dir.join(TICKET_FILE)));
        let ticket = Arc::new(TicketManager::new(
            http.clone(),
            creds.clone(),
            Some(ticket_store),
        ));
        let _ = ticket.ensure().await;
        let nav = Arc::new(NavClient::with_ticket(http.clone(), creds.clone(), ticket));
        let snap = nav.snapshot().await.expect("nav snapshot");
        assert!(snap.profile.is_login, "当前本地凭据未登录");
        let stat = nav.fetch_stat().await.expect("nav/stat");
        println!(
            "profile={} Lv{} exp={}/{:?} coins={} stat={:?}",
            snap.profile.uname,
            snap.profile.level,
            snap.profile.current_exp,
            snap.profile.next_exp,
            snap.profile.coins,
            stat
        );

        let client = NotifyClient::with_nav(http, creds, nav);
        let unread = client.fetch_unread().await.expect("unread");
        println!("unread={unread:?}");
        let replies = client.fetch_replies(None).await.expect("replies");
        println!(
            "replies={} has_more={}",
            replies.items.len(),
            replies.has_more
        );
        let ats = client.fetch_ats(None).await.expect("ats");
        println!("ats={} has_more={}", ats.items.len(), ats.has_more);
        let likes = client.fetch_likes(None).await.expect("likes");
        println!("likes={} has_more={}", likes.items.len(), likes.has_more);
        let systems = client.fetch_system_notifies(None).await.expect("systems");
        println!(
            "systems={} has_more={}",
            systems.items.len(),
            systems.has_more
        );
        let sessions = client.fetch_sessions(None).await.expect("sessions");
        println!(
            "sessions={} has_more={}",
            sessions.sessions.len(),
            sessions.has_more
        );
        if let Some(first) = sessions.sessions.first() {
            let msgs = client
                .fetch_session_messages(first.talker_id, first.session_type)
                .await
                .expect("session_messages");
            println!("first session ({}) msgs={}", first.talker_name, msgs.len());
        }
    }
}
