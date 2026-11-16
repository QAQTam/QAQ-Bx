//! 共享 HTTP 助手：浏览器 UA + Referer + cookie / bili_ticket 注入 + `Envelope` 解包。
//!
//! feed 与 player 的请求面一致（视频类接口统一走防盗链头体系），提取在此共用。

use std::sync::Arc;
use std::time::Duration;

use serde::de::DeserializeOwned;

use crate::credentials::CredentialProvider;
use crate::error::{BiliError, Result};
use crate::feed::Envelope;
use crate::ticket::TicketManager;
use crate::wbi::BROWSER_UA;

/// 拼装请求 Cookie 头（登录态 Cookie ∪ 缓存的 `bili_ticket`）。
pub(crate) async fn build_cookie_header(
    creds: &Arc<dyn CredentialProvider>,
    ticket: Option<&TicketManager>,
) -> Option<String> {
    let base = creds.get().await.ok().map(|c| c.cookie_header());
    let ticket_part = ticket.and_then(TicketManager::cookie_header_fragment);
    match (base, ticket_part) {
        (Some(mut cookies), Some(frag)) => {
            if !cookies.contains("bili_ticket=") {
                if !cookies.is_empty() {
                    cookies.push_str("; ");
                }
                cookies.push_str(&frag);
            }
            Some(cookies)
        }
        (Some(cookies), None) => Some(cookies),
        (None, Some(frag)) => Some(frag),
        (None, None) => None,
    }
}

async fn send_get(
    http: &reqwest::Client,
    creds: &Arc<dyn CredentialProvider>,
    ticket: Option<&TicketManager>,
    url: &str,
) -> Result<reqwest::Response> {
    let mut builder = http
        .get(url)
        .header(reqwest::header::USER_AGENT, BROWSER_UA)
        .header(reqwest::header::REFERER, "https://www.bilibili.com/");
    if let Some(cookie) = build_cookie_header(creds, ticket).await {
        builder = builder.header(reqwest::header::COOKIE, cookie);
    }
    Ok(builder
        .timeout(Duration::from_secs(15))
        .send()
        .await?
        .error_for_status()?)
}

/// 带防盗链头的 GET + `Envelope<T>` 解包（code != 0 映射为 `BiliError::Api`），注入可用 `bili_ticket`。
pub(crate) async fn get_json_with_ticket<T: DeserializeOwned>(
    http: &reqwest::Client,
    creds: &Arc<dyn CredentialProvider>,
    ticket: Option<&TicketManager>,
    url: &str,
) -> Result<T> {
    let resp = send_get(http, creds, ticket, url).await?;
    let env: Envelope<T> = resp.json().await?;
    match (env.code, env.data) {
        (0, Some(data)) => Ok(data),
        (0, None) => Err(BiliError::InvalidData("响应缺 data".into())),
        (code, _) => Err(BiliError::Api { code, message: env.message }),
    }
}

/// 同 [`get_bytes`]，额外注入可用 `bili_ticket`。
pub(crate) async fn get_bytes_with_ticket(
    http: &reqwest::Client,
    creds: &Arc<dyn CredentialProvider>,
    ticket: Option<&TicketManager>,
    url: &str,
) -> Result<Vec<u8>> {
    let resp = send_get(http, creds, ticket, url).await?;
    Ok(resp.bytes().await?.to_vec())
}
