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
    referer: &str,
) -> Result<reqwest::Response> {
    let mut builder = http
        .get(url)
        .header(reqwest::header::USER_AGENT, BROWSER_UA)
        .header(reqwest::header::REFERER, referer);
    if let Some(origin) = origin_of(referer) {
        builder = builder.header(reqwest::header::ORIGIN, origin);
    }
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
    get_json_with_referer(http, creds, ticket, url, "https://www.bilibili.com/").await
}

/// 同 [`get_json_with_ticket`]，可指定业务站 `Referer`（如 `https://message.bilibili.com/`）。
pub(crate) async fn get_json_with_referer<T: DeserializeOwned>(
    http: &reqwest::Client,
    creds: &Arc<dyn CredentialProvider>,
    ticket: Option<&TicketManager>,
    url: &str,
    referer: &str,
) -> Result<T> {
    let resp = send_get(http, creds, ticket, url, referer).await?;
    let env: Envelope<T> = resp.json().await?;
    match (env.code, env.data) {
        (0, Some(data)) => Ok(data),
        (0, None) => Err(BiliError::InvalidData("响应缺 data".into())),
        (code, _) => Err(BiliError::Api {
            code,
            message: env.message,
        }),
    }
}

/// 同 [`get_bytes`]，额外注入可用 `bili_ticket`。返回 `Bytes`(引用计数缓冲,
/// 下游零拷贝切片;#2 弹幕分段直传解析)。
pub(crate) async fn get_bytes_with_ticket(
    http: &reqwest::Client,
    creds: &Arc<dyn CredentialProvider>,
    ticket: Option<&TicketManager>,
    url: &str,
) -> Result<bytes::Bytes> {
    let resp = send_get(http, creds, ticket, url, "https://www.bilibili.com/").await?;
    Ok(resp.bytes().await?)
}

/// 带登录态的表单 POST + `Envelope<T>` 解包（写接口统一入口：
/// reply/add 等；未登录由 `CredentialProvider` 报 `NotLoggedIn`）。
pub(crate) async fn post_form_with_ticket<T: DeserializeOwned>(
    http: &reqwest::Client,
    creds: &Arc<dyn CredentialProvider>,
    ticket: Option<&TicketManager>,
    url: &str,
    referer: &str,
    form: &[(String, String)],
) -> Result<T> {
    let env: Envelope<T> = post_form_env(http, creds, ticket, url, referer, form).await?;
    match (env.code, env.data) {
        (0, Some(data)) => Ok(data),
        (0, None) => Err(BiliError::InvalidData("响应缺 data".into())),
        (code, _) => Err(BiliError::Api {
            code,
            message: env.message,
        }),
    }
}

/// #7 稍后再看等「成功只认 code」的写接口:data 可为空/缺省,不做解包。
pub(crate) async fn post_form_expect_ok(
    http: &reqwest::Client,
    creds: &Arc<dyn CredentialProvider>,
    ticket: Option<&TicketManager>,
    url: &str,
    referer: &str,
    form: &[(String, String)],
) -> Result<()> {
    let env: Envelope<serde_json::Value> =
        post_form_env(http, creds, ticket, url, referer, form).await?;
    if env.code == 0 {
        Ok(())
    } else {
        Err(BiliError::Api {
            code: env.code,
            message: env.message,
        })
    }
}

/// 表单 POST 的公共发送段(凭据/cookie/头/超时与错误映射统一)。
async fn post_form_env<T: DeserializeOwned>(
    http: &reqwest::Client,
    creds: &Arc<dyn CredentialProvider>,
    ticket: Option<&TicketManager>,
    url: &str,
    referer: &str,
    form: &[(String, String)],
) -> Result<Envelope<T>> {
    // csrf 需要 Cookie 登录态:先取凭据(未登录在此失败,不发请求)。
    let credentials = creds.get().await?;
    let cookie = match build_cookie_header(creds, ticket).await {
        Some(cookie) => cookie,
        None => credentials.cookie_header(),
    };
    let mut builder = http
        .post(url)
        .header(reqwest::header::USER_AGENT, BROWSER_UA)
        .header(reqwest::header::REFERER, referer)
        .header(reqwest::header::COOKIE, cookie)
        .header(reqwest::header::CONTENT_TYPE, "application/x-www-form-urlencoded");
    if let Some(origin) = origin_of(referer) {
        builder = builder.header(reqwest::header::ORIGIN, origin);
    }
    let resp = builder
        .timeout(Duration::from_secs(15))
        .form(form)
        .send()
        .await?
        .error_for_status()?;
    Ok(resp.json().await?)
}

fn origin_of(url: &str) -> Option<&str> {
    let scheme_end = url.find("://")?;
    let after_scheme = &url[scheme_end + 3..];
    match after_scheme.find('/') {
        Some(slash) => Some(&url[..scheme_end + 3 + slash]),
        None => Some(url),
    }
}

#[cfg(test)]
mod tests {
    use super::origin_of;

    #[test]
    fn extracts_origin_from_root_and_subpath_referers() {
        assert_eq!(
            origin_of("https://www.bilibili.com/"),
            Some("https://www.bilibili.com")
        );
        assert_eq!(
            origin_of("https://space.bilibili.com/12345/fans/follow"),
            Some("https://space.bilibili.com")
        );
        assert_eq!(
            origin_of("https://message.bilibili.com"),
            Some("https://message.bilibili.com")
        );
        assert_eq!(origin_of("not-a-url"), None);
    }
}
