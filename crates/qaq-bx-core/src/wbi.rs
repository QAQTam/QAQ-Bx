//! WBI 签名（契约：docs/bilibili-api-inventory.md §4；Node 定案实现 `.qaqh/research/replay-apis.mjs`）。
//!
//! `w_rid = md5(排序后 query 过滤!'()* + mixin32)`，`wts` 参与排序；
//! mixin = nav 的 `wbi_img` 两文件名按固定置换表重排取前 32。
//! nav 请求与 24h 缓存统一由 [`crate::account::NavClient`] 承担（同一进程只拉一次），
//! 本模块只负责从快照的 `wbi_img` URL 计算 mixin 并签名。

use std::sync::Arc;

use md5::{Digest, Md5};

use crate::account::NavClient;
use crate::error::{BiliError, Result};

/// 置换表（64 元素，定案自站点 JS，见 inventory §4）。
const MIXIN_TABLE: [usize; 64] = [
    46, 47, 18, 2, 53, 8, 23, 32, 15, 50, 10, 31, 58, 3, 45, 35, 27, 43, 5, 49, 33, 9, 42, 19, 29,
    28, 14, 39, 12, 38, 41, 13, 37, 48, 7, 16, 24, 55, 40, 61, 26, 17, 0, 1, 60, 51, 30, 4, 22, 25,
    54, 21, 56, 59, 6, 63, 57, 62, 11, 36, 20, 34, 44, 52,
];

pub const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/157.0.0.0 Safari/537.36";

/// WBI 签名器（可在多个 client 间共享）。
pub struct WbiSigner {
    nav: Arc<NavClient>,
}

impl WbiSigner {
    /// 独立构造：内部自建匿名 NavClient（CLI / 单测等只需签名的场景）。
    pub fn new(http: reqwest::Client) -> Self {
        Self {
            nav: Arc::new(NavClient::new_anonymous(http)),
        }
    }

    /// 复用共享 NavClient：feed/播放/弹幕与 UI 账号展示共用同一份 nav 缓存。
    pub fn with_nav(nav: Arc<NavClient>) -> Self {
        Self { nav }
    }

    /// 当前 mixin key（nav 缓存 24h；nav 失败时降级尝试 `TicketManager` 顺带返回的 `wbi_img_urls`）。
    async fn mixin_key(&self) -> Result<String> {
        let (img_url, sub_url) = match self.nav.snapshot().await {
            Ok(snap) => (snap.wbi_img_url, snap.wbi_sub_url),
            Err(err) => self.nav.ticket().and_then(|t| t.wbi_urls()).ok_or(err)?,
        };
        let file_name = |url: &str| -> Result<String> {
            url.rsplit('/')
                .next()
                .and_then(|n| n.split('.').next())
                .map(String::from)
                .ok_or_else(|| BiliError::InvalidData(format!("wbi_img 非法 URL: {url}")))
        };
        let img = file_name(&img_url)?;
        let sub = file_name(&sub_url)?;
        let combined = format!("{img}{sub}");
        let mixin: String = MIXIN_TABLE
            .iter()
            .filter_map(|&i| combined.chars().nth(i))
            .collect();
        Ok(mixin[..32.min(mixin.len())].to_string())
    }

    /// 对参数集签名，返回可直接拼接的完整 query（含 `w_rid`/`wts`）。
    ///
    /// `params` 的值应为已字符串化的最终值；`!'()*` 会被过滤（服务器同款语义）。
    pub async fn sign_query(&self, params: &[(&str, String)]) -> Result<String> {
        let mixin = self.mixin_key().await?;
        let wts = crate::credentials::now_unix();

        let mut pairs: Vec<(String, String)> = params
            .iter()
            .map(|(k, v)| {
                (
                    encode_component(&filter_chars(k)),
                    encode_component(&filter_chars(v)),
                )
            })
            .collect();
        pairs.push(("wts".to_string(), wts.to_string()));
        // JS Array#sort 默认按 UTF-16 码元；参数名全 ASCII 时与字节序一致。
        pairs.sort_by(|a, b| a.0.cmp(&b.0));
        let query = pairs
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&");
        let w_rid = md5_hex(&format!("{query}{mixin}"));
        // query 已含排序后的 wts;不得再追加(重复 wts 会导致服务端校验失败,
        // playurl 对此静默降级为无 DASH 响应,rcmd 等宽松端点则不易察觉)。
        Ok(format!("{query}&w_rid={w_rid}"))
    }
}

/// 服务器语义：值过滤 `!'()*`。
fn filter_chars(s: &str) -> String {
    s.chars()
        .filter(|c| !matches!(c, '!' | '\'' | '(' | ')' | '*'))
        .collect()
}

/// 与 JS `encodeURIComponent` 一致的编码（unreserved: A-Za-z0-9-_.!~*'()；
/// !'()* 已在过滤步移除，~ 保留）。
pub(crate) fn encode_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            other => {
                out.push('%');
                out.push_str(&format!("{other:02X}"));
            }
        }
    }
    out
}

fn md5_hex(s: &str) -> String {
    let mut h = Md5::new();
    h.update(s.as_bytes());
    format!("{:x}", h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_matches_js() {
        assert_eq!(encode_component("abc-_.~"), "abc-_.~");
        assert_eq!(encode_component("a b"), "a%20b");
        assert_eq!(encode_component("中文"), "%E4%B8%AD%E6%96%87");
        assert_eq!(encode_component("a=b&c"), "a%3Db%26c");
    }

    #[test]
    fn filter_strips_specials() {
        assert_eq!(filter_chars("a!'()*b"), "ab");
        assert_eq!(filter_chars("plain"), "plain");
    }

    /// 定案向量：replay-apis.mjs 已在真机验证（search 端 code=0）。
    #[test]
    fn md5_reference() {
        assert_eq!(md5_hex("abc"), "900150983cd24fb0d6963f7d28e17f72");
    }
}
