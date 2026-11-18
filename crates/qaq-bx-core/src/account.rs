//! 账号资料契约 + nav 统一拉取。
//!
//! `GET /x/web-interface/nav` 一次响应同时下发 WBI 密钥（`wbi_img`）与登录用户资料
//! （`mid/uname/face/level_info/vipStatus`…）。历史上被两处各拉一次、各自定义私有
//! struct：`crate::wbi`（只取密钥）与 `qaq-bx-ui/src/auth.rs`（只取昵称头像）。
//! 本模块把 nav 收敛为唯一入口 [`NavClient`]：一份 24h 缓存、一次请求，WBI 签名器、
//! feed/播放/弹幕客户端与 UI 账号展示共用同一个 `Arc<NavClient>`。
//!
//! 契约锚：`docs/bilibili-api-inventory.md` §0（鉴权）/ §4（WBI）/ §2（大会员档位）。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::Deserialize;

use crate::credentials::{CredentialProvider, Credentials};
use crate::error::{BiliError, Result};
use crate::httpx::{build_cookie_header, get_json_with_referer, get_json_with_ticket};
use crate::loose::{f64_any_or_zero, i64_any, opt_u64_any, u64_any};
use crate::ticket::TicketManager;
use crate::wbi::BROWSER_UA;

const NAV_URL: &str = "https://api.bilibili.com/x/web-interface/nav";
const NAV_STAT_URL: &str = "https://api.bilibili.com/x/web-interface/nav/stat";
const RELATIONS_FOLLOWINGS_URL: &str = "https://api.bilibili.com/x/relation/followings";
const RELATIONS_FOLLOWERS_URL: &str = "https://api.bilibili.com/x/relation/followers";
/// 与 WBI 密钥轮换节奏对齐（服务器约每日轮换），到期强制重拉。
const NAV_TTL: Duration = Duration::from_secs(24 * 3600);

// ---------- 用户资料契约 ----------

/// 大会员状态（nav 的 `vipStatus` + `vipType` 推导）。
///
/// 注意：这不只是展示字段——`docs/bilibili-api-inventory.md` §2 实测
/// "未登录/非大会员时 `dash.video` 不含高清晰度档"，清晰度上限需据此调整。
/// `vipType` 的 1/2 口径按 nav 常见取值映射，未逐档真机验证；其余取值落
/// [`VipStatus::Other`] 兜底，不影响 `is_vip()` 判定。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum VipStatus {
    /// `vipStatus != 1`（含未登录）。
    #[default]
    None,
    /// `vipType = 1`。
    Monthly,
    /// `vipType = 2`。
    Annual,
    /// 其它有效类型（保留原始值，供后续口径细化）。
    Other(i64),
}

impl VipStatus {
    pub fn is_vip(self) -> bool {
        !matches!(self, VipStatus::None)
    }

    pub fn label(self) -> &'static str {
        match self {
            VipStatus::None => "非大会员",
            VipStatus::Monthly => "月度大会员",
            VipStatus::Annual => "年度大会员",
            VipStatus::Other(_) => "大会员",
        }
    }
}

/// nav 探测到的用户资料（对外只读视图）。
#[derive(Debug, Clone, Default)]
pub struct UserProfile {
    pub is_login: bool,
    /// 用户 uuid（与 `Credentials.dede_user_id` 同源）。
    pub mid: u64,
    pub uname: String,
    /// 头像 URL（位图下载仍由 UI 侧 image_cache 负责，不进契约层）。
    pub face: String,
    pub level: u8,
    pub current_exp: u64,
    /// 升级所需总经验（Lv6 为 `None`，服务端返回 `"--"`）。
    pub next_exp: Option<u64>,
    /// 硬币数（nav `money`）。
    pub coins: f64,
    pub vip: VipStatus,
    /// 会员展示标签（优先服务端 `vip_label.text`，兜底 `vip.label()`）。
    pub vip_label: String,
    /// 大会员到期时间（毫秒 unix；0 表示无）。
    pub vip_due_date: u64,
    /// 认证头衔（`official.role > 0` 时非空）。
    pub official_title: String,
}

impl UserProfile {
    /// 用户空间主页：`mid` 即 uuid，纯拼接、无需额外 API。
    pub fn space_url(&self) -> String {
        format!("https://space.bilibili.com/{}", self.mid)
    }
}

/// 账号关系与动态计数（`GET /x/web-interface/nav/stat`）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub struct AccountStat {
    #[serde(default, deserialize_with = "u64_any")]
    pub following: u64,
    #[serde(default, deserialize_with = "u64_any")]
    pub follower: u64,
    #[serde(default, deserialize_with = "u64_any")]
    pub dynamic_count: u64,
}

/// 关系列表类型：关注 / 粉丝。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RelationKind {
    Following,
    Follower,
}

/// 关注/粉丝列表中的单个账号摘要。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationUser {
    pub mid: u64,
    pub uname: String,
    pub face: String,
    pub sign: String,
    /// `2` = 已关注，`6` = 已互粉。
    pub attribute: i64,
    pub special: bool,
    pub official_desc: String,
    pub vip_label: String,
    pub mtime: i64,
}

impl RelationUser {
    /// 用户空间主页 URL（`mid` 直接拼接）。
    pub fn space_url(&self) -> String {
        format!("https://space.bilibili.com/{}", self.mid)
    }

    /// 是否为互相关注（`attribute == 6`）。
    pub fn is_mutual(&self) -> bool {
        self.attribute == 6
    }
}

/// 单页关系列表（`GET /x/relation/{followings,followers}`）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RelationPage {
    pub items: Vec<RelationUser>,
    pub total: u64,
    pub next_pn: u32,
    pub has_more: bool,
}

// ---------- nav 原始响应（私有，宽松解析） ----------

#[derive(Debug, Default, Deserialize)]
struct LevelInfo {
    /// 数字或字符串均可能出现，统一宽松解析后收敛到 u8。
    #[serde(default, deserialize_with = "i64_any")]
    current_level: i64,
    #[serde(default, deserialize_with = "opt_u64_any")]
    current_exp: Option<u64>,
    #[serde(default, deserialize_with = "opt_u64_any")]
    next_exp: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
struct OfficialInfo {
    #[serde(default, deserialize_with = "i64_any")]
    role: i64,
    #[serde(default)]
    title: String,
}

#[derive(Debug, Default, Deserialize)]
struct VipLabelInfo {
    #[serde(default)]
    text: String,
}

#[derive(Debug, Default, Deserialize)]
struct WbiImg {
    #[serde(default)]
    img_url: String,
    #[serde(default)]
    sub_url: String,
}

#[derive(Debug, Default, Deserialize)]
struct NavData {
    #[serde(default, rename = "isLogin")]
    is_login: bool,
    #[serde(default, deserialize_with = "u64_any")]
    mid: u64,
    #[serde(default)]
    uname: String,
    #[serde(default)]
    face: String,
    #[serde(default)]
    level_info: LevelInfo,
    #[serde(default, deserialize_with = "f64_any_or_zero")]
    money: f64,
    #[serde(default)]
    official: OfficialInfo,
    #[serde(default)]
    wbi_img: WbiImg,
    #[serde(default, rename = "vipStatus", deserialize_with = "i64_any")]
    vip_status: i64,
    #[serde(default, rename = "vipType", deserialize_with = "i64_any")]
    vip_type: i64,
    #[serde(default, rename = "vipDueDate", deserialize_with = "u64_any")]
    vip_due_date: u64,
    #[serde(default)]
    vip_label: VipLabelInfo,
}

fn default_official_type() -> i64 {
    -1
}

#[derive(Debug, Deserialize)]
struct OfficialVerifyRaw {
    #[serde(
        default = "default_official_type",
        rename = "type",
        deserialize_with = "i64_any"
    )]
    kind: i64,
    #[serde(default)]
    desc: String,
}

impl Default for OfficialVerifyRaw {
    fn default() -> Self {
        Self {
            kind: -1,
            desc: String::new(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct RelationVipRaw {
    #[serde(default, rename = "vipStatus", deserialize_with = "i64_any")]
    vip_status: i64,
    #[serde(default)]
    label: VipLabelInfo,
}

#[derive(Debug, Default, Deserialize)]
struct RelationUserRaw {
    #[serde(default, deserialize_with = "u64_any")]
    mid: u64,
    #[serde(default)]
    uname: String,
    #[serde(default)]
    face: String,
    #[serde(default)]
    sign: String,
    #[serde(default, deserialize_with = "i64_any")]
    attribute: i64,
    #[serde(default, deserialize_with = "i64_any")]
    special: i64,
    #[serde(default)]
    official_verify: OfficialVerifyRaw,
    #[serde(default)]
    vip: RelationVipRaw,
    #[serde(default, deserialize_with = "i64_any")]
    mtime: i64,
}

#[derive(Debug, Default, Deserialize)]
struct RelationData {
    #[serde(default)]
    list: Vec<RelationUserRaw>,
    #[serde(default, deserialize_with = "u64_any")]
    total: u64,
}

impl RelationPage {
    fn from_raw(raw: RelationData, pn: u32, ps: u32) -> Self {
        let items: Vec<RelationUser> = raw
            .list
            .into_iter()
            .filter(|u| u.mid > 0)
            .map(|u| RelationUser {
                mid: u.mid,
                uname: u.uname,
                face: u.face.replacen("http://", "https://", 1),
                sign: u.sign.trim().to_string(),
                attribute: u.attribute,
                special: u.special == 1,
                official_desc: if u.official_verify.kind >= 0 {
                    u.official_verify.desc.trim().to_string()
                } else {
                    String::new()
                },
                vip_label: if u.vip.vip_status == 1 {
                    u.vip.label.text.trim().to_string()
                } else {
                    String::new()
                },
                mtime: u.mtime,
            })
            .collect();
        let has_more = !items.is_empty() && (pn as u64).saturating_mul(ps as u64) < raw.total;
        Self {
            items,
            total: raw.total,
            next_pn: pn.saturating_add(1),
            has_more,
        }
    }
}

// ---------- nav 快照 ----------

/// 一次 nav 响应的解析结果（WBI 密钥 + 用户资料，共享缓存单元）。
#[derive(Debug, Clone)]
pub struct NavSnapshot {
    /// `wbi_img.img_url` / `sub_url` 原样保留；文件名提取与 mixin 重排由 `WbiSigner` 负责。
    pub wbi_img_url: String,
    pub wbi_sub_url: String,
    pub profile: UserProfile,
    pub fetched_at: Instant,
}

// ---------- 客户端 ----------

/// nav 统一客户端：一次请求同时产出 WBI 密钥与用户资料，进程内缓存 24h。
///
/// 无凭据时以未登录态请求（nav 实测未登录仍下发 `wbi_img`）；`get_json` 会在
/// `CredentialProvider::get()` 失败时自动省略 Cookie 头。
/// 并发首屏（probe 与 feed 签名）各自发现空缓存时可能各拉一次，与改造前
/// `WbiSigner` 的语义一致；24h 内后续请求全部命中缓存。
pub struct NavClient {
    http: reqwest::Client,
    creds: Arc<dyn CredentialProvider>,
    ticket: Option<Arc<TicketManager>>,
    cache: Mutex<Option<NavSnapshot>>,
    /// 单飞互斥锁：并发首屏（probe 与 feed 签名）共用一次网络请求。
    fetch_lock: tokio::sync::Mutex<()>,
}

/// 无凭据占位：`WbiSigner::new` 独立构造时用（nav 未登录态仍可下发密钥）。
struct NoCreds;

#[async_trait]
impl CredentialProvider for NoCreds {
    async fn get(&self) -> Result<Credentials> {
        Err(BiliError::NotLoggedIn)
    }

    fn cached(&self) -> Option<Credentials> {
        None
    }
}

impl NavClient {
    /// 带登录态构造（`creds` 一般来自 `AuthPlugin::credentials()`）。
    pub fn new(http: reqwest::Client, creds: Arc<dyn CredentialProvider>) -> Self {
        Self {
            http,
            creds,
            ticket: None,
            cache: Mutex::new(None),
            fetch_lock: tokio::sync::Mutex::new(()),
        }
    }

    /// 带登录态 + `bili_ticket` 管理器构造（统一 runtime / UI 装配用）。
    pub fn with_ticket(
        http: reqwest::Client,
        creds: Arc<dyn CredentialProvider>,
        ticket: Arc<TicketManager>,
    ) -> Self {
        Self {
            http,
            creds,
            ticket: Some(ticket),
            cache: Mutex::new(None),
            fetch_lock: tokio::sync::Mutex::new(()),
        }
    }

    /// 无凭据构造：CLI / 单测等只需要 WBI 签名的场景。
    pub fn new_anonymous(http: reqwest::Client) -> Self {
        Self::new(http, Arc::new(NoCreds))
    }

    /// 关联的风控票据管理器（如有）。
    pub fn ticket(&self) -> Option<&Arc<TicketManager>> {
        self.ticket.as_ref()
    }

    fn cached_fresh(&self) -> Option<NavSnapshot> {
        let guard = self.cache.lock().expect("nav 缓存锁中毒");
        guard
            .as_ref()
            .filter(|s| s.fetched_at.elapsed() < NAV_TTL)
            .cloned()
    }

    /// 缓存未过期直接返回；否则单飞拉取一次。失败时若有旧缓存则降级复用（同 WbiSigner）。
    pub async fn snapshot(&self) -> Result<NavSnapshot> {
        if let Some(hit) = self.cached_fresh() {
            return Ok(hit);
        }
        let _guard = self.fetch_lock.lock().await;
        if let Some(hit) = self.cached_fresh() {
            return Ok(hit);
        }
        let fresh = self.fetch().await;
        let mut guard = self.cache.lock().expect("nav 缓存锁中毒");
        match fresh {
            Ok(s) => {
                *guard = Some(s.clone());
                Ok(s)
            }
            Err(e) => match guard.as_ref() {
                Some(old) => Ok(old.clone()),
                None => Err(e),
            },
        }
    }

    async fn fetch(&self) -> Result<NavSnapshot> {
        // 刻意不复用 `httpx::get_json`：后者把非 0 code 一律判错，而 nav 未登录态
        // 可能返回 code=-101 却仍下发 `data.wbi_img` / `isLogin=false`。改造前
        // `wbi.rs::fetch_keys` 正是"不检查 code、只取 data"才在真机通过（inventory §10），
        // 这里保持同一口径，避免未登录时 WBI 签名/推荐流整体失败。
        #[derive(Deserialize)]
        struct NavResp {
            #[serde(default)]
            code: i64,
            #[serde(default)]
            data: Option<NavData>,
        }

        let mut builder = self
            .http
            .get(NAV_URL)
            .header(reqwest::header::USER_AGENT, BROWSER_UA)
            .header(reqwest::header::REFERER, "https://www.bilibili.com/")
            .timeout(Duration::from_secs(10));
        if let Some(cookie) = build_cookie_header(&self.creds, self.ticket.as_deref()).await {
            builder = builder.header(reqwest::header::COOKIE, cookie);
        }
        let resp: NavResp = builder.send().await?.error_for_status()?.json().await?;
        let data = resp.data.ok_or_else(|| BiliError::Api {
            code: resp.code,
            message: "nav 响应缺 data".into(),
        })?;
        if data.wbi_img.img_url.is_empty() || data.wbi_img.sub_url.is_empty() {
            return Err(BiliError::InvalidData(
                "nav 响应缺 wbi_img（WBI 密钥不可用）".into(),
            ));
        }
        let vip = if data.vip_status == 1 {
            match data.vip_type {
                1 => VipStatus::Monthly,
                2 => VipStatus::Annual,
                other => VipStatus::Other(other),
            }
        } else {
            VipStatus::None
        };
        let vip_label = if !vip.is_vip() {
            String::new()
        } else if !data.vip_label.text.trim().is_empty() {
            data.vip_label.text.trim().to_string()
        } else {
            vip.label().to_string()
        };
        let official_title = if data.official.role > 0 {
            data.official.title.trim().to_string()
        } else {
            String::new()
        };
        Ok(NavSnapshot {
            wbi_img_url: data.wbi_img.img_url,
            wbi_sub_url: data.wbi_img.sub_url,
            profile: UserProfile {
                is_login: data.is_login,
                mid: data.mid,
                uname: data.uname,
                face: data.face,
                level: data.level_info.current_level.clamp(0, u8::MAX as i64) as u8,
                current_exp: data.level_info.current_exp.unwrap_or(0),
                next_exp: data.level_info.next_exp,
                coins: data.money.max(0.0),
                vip,
                vip_label,
                vip_due_date: data.vip_due_date,
                official_title,
            },
            fetched_at: Instant::now(),
        })
    }

    /// 拉取当前登录账号的关注/粉丝/动态计数（`/x/web-interface/nav/stat`）。
    pub async fn fetch_stat(&self) -> Result<AccountStat> {
        get_json_with_ticket(
            &self.http,
            &self.creds,
            self.ticket.as_deref(),
            NAV_STAT_URL,
        )
        .await
    }

    /// 分页拉取指定账号的关注或粉丝列表（`pn` 从 1 起，按 `mtime` 降序）。
    pub async fn fetch_relations(
        &self,
        mid: u64,
        kind: RelationKind,
        pn: u32,
        ps: u32,
    ) -> Result<RelationPage> {
        let pn = pn.max(1);
        let ps = ps.clamp(1, 50);
        let (url, referer_tab) = match kind {
            RelationKind::Following => (
                format!("{RELATIONS_FOLLOWINGS_URL}?vmid={mid}&pn={pn}&ps={ps}&order=desc"),
                "follow",
            ),
            RelationKind::Follower => (
                format!("{RELATIONS_FOLLOWERS_URL}?vmid={mid}&pn={pn}&ps={ps}"),
                "fans",
            ),
        };
        let referer = format!("https://space.bilibili.com/{mid}/fans/{referer_tab}");
        let raw: RelationData = get_json_with_referer(
            &self.http,
            &self.creds,
            self.ticket.as_deref(),
            &url,
            &referer,
        )
        .await?;
        Ok(RelationPage::from_raw(raw, pn, ps))
    }

    /// 仅读内存缓存的用户资料，不发 IO、不报错（UI 渲染 / 清晰度档位判断用）。
    pub fn cached_profile(&self) -> Option<UserProfile> {
        self.cache
            .lock()
            .expect("nav 缓存锁中毒")
            .as_ref()
            .map(|s| s.profile.clone())
    }

    /// 登录成功 / 登出后作废缓存，下次 `snapshot` 会用新凭据重拉用户资料。
    pub fn invalidate(&self) {
        *self.cache.lock().expect("nav 缓存锁中毒") = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nav_parses_logged_in_annual_vip_with_string_numbers() {
        let json = r#"{
            "isLogin": true, "mid": "123456", "uname": "测试账号",
            "face": "https://i0.hdslb.com/bfs/face/x.jpg",
            "level_info": {"current_level": "6"},
            "wbi_img": {"img_url": "https://i0.hdslb.com/bfs/wbi/abc123.png",
                        "sub_url": "https://i0.hdslb.com/bfs/wbi/def456.png"},
            "vipStatus": 1, "vipType": 2, "vipDueDate": "1798761600000"
        }"#;
        let d: NavData = serde_json::from_str(json).unwrap();
        assert!(d.is_login);
        assert_eq!(d.mid, 123456);
        assert_eq!(d.level_info.current_level, 6);
        assert_eq!(d.vip_status, 1);
        assert_eq!(d.vip_type, 2);
        assert_eq!(d.vip_due_date, 1_798_761_600_000);
        assert!(d.wbi_img.img_url.ends_with("abc123.png"));

        let vip = if d.vip_status == 1 {
            match d.vip_type {
                1 => VipStatus::Monthly,
                2 => VipStatus::Annual,
                other => VipStatus::Other(other),
            }
        } else {
            VipStatus::None
        };
        assert_eq!(vip, VipStatus::Annual);
        assert!(vip.is_vip());
        assert_eq!(vip.label(), "年度大会员");
    }

    #[test]
    fn nav_anonymous_defaults_to_no_vip() {
        // 未登录时 nav 的 data 里大量字段缺失 → 全部走 serde default。
        let d: NavData = serde_json::from_str(r#"{"isLogin":false,"wbi_img":{}}"#).unwrap();
        assert!(!d.is_login);
        assert_eq!(d.mid, 0);
        assert_eq!(d.vip_status, 0);
        assert_eq!(VipStatus::None.label(), "非大会员");
        assert!(!VipStatus::None.is_vip());
    }

    #[test]
    fn space_url_is_derived_from_mid() {
        let profile = UserProfile {
            mid: 80433022,
            ..Default::default()
        };
        assert_eq!(profile.space_url(), "https://space.bilibili.com/80433022");
    }

    #[test]
    fn nav_resp_keeps_wbi_img_when_code_is_nonzero() {
        // 未登录态 nav 可能返回 code=-101 但仍下发 wbi_img；WBI 签名依赖它，
        // 不能像通用 Envelope 那样把非 0 code 一律判错。
        #[derive(Deserialize)]
        struct Resp {
            #[serde(default)]
            code: i64,
            #[serde(default)]
            data: Option<NavData>,
        }
        let json = r#"{"code":-101,"message":"账号未登录","ttl":1,
            "data":{"isLogin":false,
                    "wbi_img":{"img_url":"https://i0.hdslb.com/bfs/wbi/a.png",
                               "sub_url":"https://i0.hdslb.com/bfs/wbi/b.png"}}}"#;
        let resp: Resp = serde_json::from_str(json).unwrap();
        assert_eq!(resp.code, -101);
        let d = resp.data.unwrap();
        assert!(!d.is_login);
        assert!(d.wbi_img.img_url.ends_with("a.png"));
        assert!(d.wbi_img.sub_url.ends_with("b.png"));
    }

    #[test]
    fn nav_parses_lv6_dash_next_exp_coins_official_and_stat() {
        let json = r#"{
            "isLogin": true,
            "mid": 100000000,
            "uname": "测试账号",
            "face": "https://i1.hdslb.com/bfs/face/x.jpg",
            "level_info": { "current_level": 6, "current_exp": 45701, "next_exp": "--" },
            "money": 125.1,
            "official": { "role": 2, "title": "知名UP主" },
            "vipStatus": 1,
            "vipType": 1,
            "vipDueDate": 1795363200000,
            "vip_label": { "text": "大会员" }
        }"#;
        let d: NavData = serde_json::from_str(json).unwrap();
        assert_eq!(d.level_info.current_exp, Some(45701));
        assert_eq!(d.level_info.next_exp, None);
        assert!((d.money - 125.1).abs() < f64::EPSILON);
        assert_eq!(d.official.role, 2);
        assert_eq!(d.official.title, "知名UP主");
        assert_eq!(d.vip_label.text, "大会员");

        let stat: AccountStat =
            serde_json::from_str(r#"{"following":168,"follower":"7","dynamic_count":5}"#).unwrap();
        assert_eq!(
            stat,
            AccountStat {
                following: 168,
                follower: 7,
                dynamic_count: 5,
            }
        );
    }

    #[test]
    fn relation_list_parses_mutual_vip_official_and_pagination() {
        let json = r#"{
            "list": [
                {
                    "mid": "147546636",
                    "attribute": 2,
                    "special": 1,
                    "mtime": 1794240869,
                    "uname": "无畏契约",
                    "face": "http://i0.hdslb.com/bfs/face/a.jpg",
                    "sign": "  官方账号签名  ",
                    "official_verify": { "type": 1, "desc": "官方账号" },
                    "vip": { "vipStatus": 1, "label": { "text": "年度大会员" } }
                },
                {
                    "mid": 403696152,
                    "attribute": 6,
                    "special": 0,
                    "mtime": 1768872016,
                    "uname": "千羽风笙",
                    "face": "https://i0.hdslb.com/bfs/face/b.jpg",
                    "sign": "",
                    "official_verify": { "type": -1, "desc": "" },
                    "vip": { "vipStatus": 0, "label": { "text": "旧标签" } }
                }
            ],
            "total": 168
        }"#;
        let raw: RelationData = serde_json::from_str(json).unwrap();
        let page = RelationPage::from_raw(raw, 1, 20);
        assert_eq!(page.total, 168);
        assert_eq!(page.next_pn, 2);
        assert!(page.has_more);
        assert_eq!(page.items.len(), 2);

        assert_eq!(page.items[0].mid, 147546636);
        assert_eq!(page.items[0].face, "https://i0.hdslb.com/bfs/face/a.jpg");
        assert_eq!(page.items[0].sign, "官方账号签名");
        assert!(!page.items[0].is_mutual());
        assert!(page.items[0].special);
        assert_eq!(page.items[0].official_desc, "官方账号");
        assert_eq!(page.items[0].vip_label, "年度大会员");
        assert_eq!(
            page.items[0].space_url(),
            "https://space.bilibili.com/147546636"
        );

        assert!(page.items[1].is_mutual());
        assert!(!page.items[1].special);
        assert_eq!(page.items[1].official_desc, "");
        assert_eq!(page.items[1].vip_label, "");

        let last_raw: RelationData =
            serde_json::from_str(r#"{"list":[{"mid":1,"uname":"u"}],"total":45}"#).unwrap();
        let last_page = RelationPage::from_raw(last_raw, 3, 20);
        assert!(!last_page.has_more);
    }

    #[tokio::test]
    #[ignore = "需本地已登录凭据（data/credentials.json）且访问真实 B 站接口"]
    async fn live_relations_smoke() {
        use crate::credentials::FileCredentialStore;
        use crate::storage::resolve_data_dir;
        use crate::ticket::{FileTicketStore, TICKET_FILE};

        let data_dir = resolve_data_dir();
        let store = Arc::new(FileCredentialStore::new(data_dir.join("credentials.json")));
        let creds = Arc::new(crate::credentials::DefaultCredentialProvider::new(store));
        let http = reqwest::Client::builder()
            .user_agent(BROWSER_UA)
            .build()
            .unwrap();
        let ticket = Arc::new(TicketManager::new(
            http.clone(),
            creds.clone(),
            Some(Arc::new(FileTicketStore::new(data_dir.join(TICKET_FILE)))),
        ));
        let _ = ticket.ensure().await;
        let nav = NavClient::with_ticket(http, creds, ticket);
        let snap = nav.snapshot().await.expect("nav snapshot");
        assert!(snap.profile.is_login);

        let followings = nav
            .fetch_relations(snap.profile.mid, RelationKind::Following, 1, 20)
            .await
            .expect("followings");
        println!(
            "followings={} total={} has_more={} first={:?}",
            followings.items.len(),
            followings.total,
            followings.has_more,
            followings.items.first().map(|u| &u.uname)
        );
        let followers = nav
            .fetch_relations(snap.profile.mid, RelationKind::Follower, 1, 20)
            .await
            .expect("followers");
        println!(
            "followers={} total={} has_more={} first={:?}",
            followers.items.len(),
            followers.total,
            followers.has_more,
            followers.items.first().map(|u| (&u.uname, u.is_mutual()))
        );
    }
}
