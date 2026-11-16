//! `bili_ticket`（3 天风控票据）：`GenWebTicket` + HMAC-SHA256 + 本地缓存。
//!
//! 契约锚 `docs/bilibili-login-research.md` §5（逆向自 `token-iframe` chunk 895）：
//! - HMAC-SHA256 密钥由 `"YhxToH[2q"` 逐字节减 1 得到（`"XgwSnGZ1p"`），消息为 `"ts{unix_secs}"`；
//! - `POST https://api.bilibili.com/bapis/bilibili.api.ticket.v1.Ticket/GenWebTicket`，query 携带
//!   `key_id=ec02`、`hexsign`、`context[ts]`、及可选 `csrf`（已登录取 `bili_jct`）；
//! - 提前 30 分钟（`expires_at - now <= 1800`）续签，并将 `data.nav.{img,sub}` 留存作 WBI 密钥兜底。

use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::credentials::{now_unix, CredentialProvider};
use crate::error::{BiliError, Result};
use crate::loose::i64_any;
use crate::storage::{atomic_write_json, load_json_opt, remove_file_if_exists};
use crate::wbi::BROWSER_UA;

const GEN_WEB_TICKET_URL: &str =
    "https://api.bilibili.com/bapis/bilibili.api.ticket.v1.Ticket/GenWebTicket";
const TICKET_KEY_ID: &str = "ec02";
/// `chunk895.js`: `r = "YhxToH[2q"`，逐字符 `- 1` 解码为真实 HMAC 密钥 `"XgwSnGZ1p"`。
const TICKET_KEY_OBFUSCATED: &[u8] = b"YhxToH[2q";
/// 续签提前量（秒）：到期前 30 分钟内触发续签（chunk 895 `eD()` 同款 `18e5` ms）。
pub const TICKET_REFRESH_LEAD_SECS: i64 = 1_800;

/// 票据缓存文件名（存于数据目录下）。
pub const TICKET_FILE: &str = "ticket.json";

/// 解码站点内嵌混淆密钥。
fn decode_ticket_key() -> Vec<u8> {
    TICKET_KEY_OBFUSCATED
        .iter()
        .map(|b| b.wrapping_sub(1))
        .collect()
}

/// 标准 RFC 2104 HMAC-SHA256，返回小写 hex（对齐 `CryptoJS.enc.Hex.stringify`）。
pub fn hmac_sha256_hex(key: &[u8], data: &[u8]) -> String {
    const BLOCK_SIZE: usize = 64;
    let mut k_block = [0u8; BLOCK_SIZE];
    if key.len() > BLOCK_SIZE {
        let hashed = Sha256::digest(key);
        k_block[..hashed.len()].copy_from_slice(&hashed);
    } else {
        k_block[..key.len()].copy_from_slice(key);
    }

    let mut o_pad = [0x5cu8; BLOCK_SIZE];
    let mut i_pad = [0x36u8; BLOCK_SIZE];
    for i in 0..BLOCK_SIZE {
        o_pad[i] ^= k_block[i];
        i_pad[i] ^= k_block[i];
    }

    let mut inner = Sha256::new();
    inner.update(i_pad);
    inner.update(data);
    let inner_hash = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(o_pad);
    outer.update(inner_hash);
    hex::encode(outer.finalize())
}

/// 缓存的风控票据（内存 + `ticket.json`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketData {
    pub ticket: String,
    pub created_at: i64,
    pub ttl: i64,
    pub expires_at: i64,
    #[serde(default)]
    pub wbi_img_url: String,
    #[serde(default)]
    pub wbi_sub_url: String,
}

impl TicketData {
    /// 是否仍在硬有效期内（可放入 `Cookie` 使用）。
    pub fn is_usable_at(&self, now: i64) -> bool {
        !self.ticket.is_empty() && now < self.expires_at
    }

    /// 是否需要续签（缺票据、已过期或距到期不足 30 分钟）。
    pub fn needs_refresh_at(&self, now: i64) -> bool {
        self.ticket.is_empty() || self.expires_at - now <= TICKET_REFRESH_LEAD_SECS
    }
}

#[derive(Debug, Deserialize)]
struct GenTicketNav {
    #[serde(default)]
    img: String,
    #[serde(default)]
    sub: String,
}

#[derive(Debug, Deserialize)]
struct GenTicketData {
    #[serde(default)]
    ticket: String,
    #[serde(default, deserialize_with = "i64_any")]
    created_at: i64,
    #[serde(default, deserialize_with = "i64_any")]
    ttl: i64,
    #[serde(default)]
    nav: Option<GenTicketNav>,
}

#[derive(Debug, Deserialize)]
struct GenTicketEnvelope {
    code: i64,
    #[serde(default)]
    message: String,
    #[serde(default)]
    data: Option<GenTicketData>,
}

/// 从 `GenWebTicket` JSON 响应体解析出 [`TicketData`]。
pub(crate) fn parse_ticket_response(body: &[u8], fallback_now: i64) -> Result<TicketData> {
    let env: GenTicketEnvelope = serde_json::from_slice(body)
        .map_err(|e| BiliError::InvalidData(format!("GenWebTicket 响应解析失败: {e}")))?;
    if env.code != 0 {
        return Err(BiliError::Api { code: env.code, message: env.message });
    }
    let data = env
        .data
        .ok_or_else(|| BiliError::InvalidData("GenWebTicket 响应缺 data".into()))?;
    if data.ticket.is_empty() {
        return Err(BiliError::InvalidData("GenWebTicket 返回空 ticket".into()));
    }
    let created_at = if data.created_at > 0 { data.created_at } else { fallback_now };
    let ttl = if data.ttl > 0 { data.ttl } else { 259_200 };
    let (wbi_img_url, wbi_sub_url) = data
        .nav
        .map(|n| (n.img, n.sub))
        .unwrap_or_default();
    Ok(TicketData {
        ticket: data.ticket,
        created_at,
        ttl,
        expires_at: created_at.saturating_add(ttl),
        wbi_img_url,
        wbi_sub_url,
    })
}

/// `ticket.json` 文件持久化。
pub struct FileTicketStore {
    path: PathBuf,
}

impl FileTicketStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn load(&self) -> Result<Option<TicketData>> {
        load_json_opt(&self.path, "票据缓存文件")
    }

    pub fn save(&self, ticket: &TicketData) -> Result<()> {
        atomic_write_json(&self.path, ticket, "票据")
    }

    pub fn clear(&self) -> Result<()> {
        remove_file_if_exists(&self.path)
    }
}

/// 进程级 `bili_ticket` 管理器（单飞续签 + 内存/文件缓存）。
pub struct TicketManager {
    http: reqwest::Client,
    creds: Arc<dyn CredentialProvider>,
    store: Option<Arc<FileTicketStore>>,
    cache: RwLock<Option<TicketData>>,
    refresh_lock: tokio::sync::Mutex<()>,
}

impl TicketManager {
    pub fn new(
        http: reqwest::Client,
        creds: Arc<dyn CredentialProvider>,
        store: Option<Arc<FileTicketStore>>,
    ) -> Self {
        let initial = store
            .as_ref()
            .and_then(|s| s.load().ok().flatten());
        Self {
            http,
            creds,
            store,
            cache: RwLock::new(initial),
            refresh_lock: tokio::sync::Mutex::new(()),
        }
    }

    pub fn cached(&self) -> Option<TicketData> {
        self.cache.read().expect("ticket 缓存锁中毒").clone()
    }

    /// 当前可用且未硬过期的 Cookie 片段（不发网络/磁盘 IO）。
    pub fn cookie_header_fragment(&self) -> Option<String> {
        let data = self.cached()?;
        if !data.is_usable_at(now_unix()) {
            return None;
        }
        Some(format!(
            "bili_ticket={}; bili_ticket_expires={}",
            data.ticket, data.expires_at
        ))
    }

    /// 取票据响应里顺带返回的 WBI `nav.{img,sub}` URL（作为 nav 失败时的兜底密钥来源）。
    pub fn wbi_urls(&self) -> Option<(String, String)> {
        let data = self.cached()?;
        if data.wbi_img_url.is_empty() || data.wbi_sub_url.is_empty() {
            return None;
        }
        Some((data.wbi_img_url, data.wbi_sub_url))
    }

    /// 若缓存缺失或即将到期（≤ 30 分钟），单飞触发一次 `GenWebTicket` 续签并落盘。
    pub async fn ensure(&self) -> Result<TicketData> {
        if let Some(data) = self.cached()
            && !data.needs_refresh_at(now_unix())
        {
            return Ok(data);
        }

        let _guard = self.refresh_lock.lock().await;
        if let Some(data) = self.cached()
            && !data.needs_refresh_at(now_unix())
        {
            return Ok(data);
        }

        let fresh = self.fetch_ticket().await?;
        if let Some(store) = &self.store {
            let _ = store.save(&fresh);
        }
        *self.cache.write().expect("ticket 缓存锁中毒") = Some(fresh.clone());
        Ok(fresh)
    }

    /// 作废当前票据缓存并清除磁盘文件（登录/登出切账号时调用）。
    pub fn invalidate(&self) {
        *self.cache.write().expect("ticket 缓存锁中毒") = None;
        if let Some(store) = &self.store {
            let _ = store.clear();
        }
    }

    async fn fetch_ticket(&self) -> Result<TicketData> {
        let ts = now_unix();
        let key = decode_ticket_key();
        let hexsign = hmac_sha256_hex(&key, format!("ts{ts}").as_bytes());
        let creds = self.creds.get().await.ok();

        let mut url = format!(
            "{GEN_WEB_TICKET_URL}?key_id={TICKET_KEY_ID}&hexsign={hexsign}&context[ts]={ts}"
        );
        if let Some(c) = &creds
            && !c.bili_jct.is_empty()
        {
            url.push_str("&csrf=");
            url.push_str(&c.bili_jct);
        }

        let mut req = self
            .http
            .post(&url)
            .header(reqwest::header::USER_AGENT, BROWSER_UA)
            .header(reqwest::header::REFERER, "https://www.bilibili.com/")
            .timeout(Duration::from_secs(10));
        if let Some(c) = &creds {
            req = req.header(reqwest::header::COOKIE, c.cookie_header());
        }

        let bytes = req.send().await?.error_for_status()?.bytes().await?;
        parse_ticket_response(&bytes, ts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticket_key_decodes_expected_secret() {
        assert_eq!(decode_ticket_key(), b"XgwSnGZ1p");
    }

    #[test]
    fn hmac_sha256_matches_rfc4231_vectors() {
        // RFC 4231 Test Case 1: key = 0x0b * 20, data = "Hi There"
        assert_eq!(
            hmac_sha256_hex(&[0x0b; 20], b"Hi There"),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        // RFC 4231 Test Case 2: key = "Jefe", data = "what do ya want for nothing?"
        assert_eq!(
            hmac_sha256_hex(b"Jefe", b"what do ya want for nothing?"),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn ticket_refresh_window_matches_30_min_rule() {
        let t = TicketData {
            ticket: "abc".into(),
            created_at: 1_000,
            ttl: 3_600,
            expires_at: 4_600,
            wbi_img_url: String::new(),
            wbi_sub_url: String::new(),
        };
        assert!(t.is_usable_at(4_599));
        assert!(!t.is_usable_at(4_600));
        assert!(!t.needs_refresh_at(2_799)); // 剩 1801s -> 暂不续签
        assert!(t.needs_refresh_at(2_800)); // 剩 1800s -> 触发续签
        assert!(t.needs_refresh_at(5_000)); // 已过期 -> 触发续签
    }

    #[test]
    fn parse_gen_web_ticket_response() {
        let json = br#"{
            "code": 0,
            "message": "0",
            "data": {
                "ticket": "eyJhbGciOi",
                "created_at": "1700000000",
                "ttl": 259200,
                "nav": {
                    "img": "https://i0.hdslb.com/bfs/wbi/img123.png",
                    "sub": "https://i0.hdslb.com/bfs/wbi/sub456.png"
                }
            }
        }"#;
        let parsed = parse_ticket_response(json, 1_700_000_005).unwrap();
        assert_eq!(parsed.ticket, "eyJhbGciOi");
        assert_eq!(parsed.created_at, 1_700_000_000);
        assert_eq!(parsed.ttl, 259_200);
        assert_eq!(parsed.expires_at, 1_700_259_200);
        assert!(parsed.wbi_img_url.ends_with("img123.png"));
        assert!(parsed.wbi_sub_url.ends_with("sub456.png"));
    }
}
