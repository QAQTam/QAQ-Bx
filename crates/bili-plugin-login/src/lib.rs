//! 扫码登录插件（biliplay 第一个插件）。
//!
//! 流程来自 CDP 实测（docs/bilibili-login-research.md §2）：
//! 1. `GET /x/passport-login/web/qrcode/generate` → `{url, qrcode_key}`
//! 2. 调用方展示二维码（url 即二维码内容），本插件逐次轮询
//!    `GET /x/passport-login/web/qrcode/poll?qrcode_key=...`
//!    86101=未扫 / 86090=已扫未确认 / 0=成功 / 86038=失效
//! 3. 成功后凭据来源（按浏览器真实行为，逐级兜底）：
//!    a. poll 响应自身的 `Set-Cookie` 头
//!    b. `data.url` 的查询参数（兼容 crossDomain 带参形态）
//!    c. 手动跟随 `data.url` 跳转链，收集每跳的 `Set-Cookie`
//!    另：`finger/spi` 补 buvid 设备指纹；refresh_token 来自 poll JSON。

pub mod correspond;
pub mod refresh;

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use async_trait::async_trait;
use bili_core::{
    auth::LoginEvent,
    credentials::{now_unix, CredentialProvider, CredentialStore, Credentials, DefaultCredentialProvider},
    error::{BiliError, Result},
    plugin::{Plugin, PluginContext, PluginMeta},
};
use url::Url;

const GENERATE_URL: &str = "https://passport.bilibili.com/x/passport-login/web/qrcode/generate";
const POLL_URL: &str = "https://passport.bilibili.com/x/passport-login/web/qrcode/poll";
const SPI_URL: &str = "https://api.bilibili.com/x/frontend/finger/spi";
/// 服务端登出端点（主站 header 真实实现：POST form {biliCSRF}）
const LOGOUT_URL: &str = "https://passport.bilibili.com/login/exit/v2";
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/157.0.0.0 Safari/537.36";

/// 凭据文件名（存于 PluginContext::data_dir 下）。
pub const CREDENTIALS_FILE: &str = "credentials.json";

/// 扫码登录插件。`init` 后可用；`login()` 返回可轮询会话。
pub struct QrAuthPlugin {
    meta: PluginMeta,
    provider: Arc<DefaultCredentialProvider>,
    http: std::sync::OnceLock<reqwest::Client>,
}

impl QrAuthPlugin {
    /// `store` 由调用方注入（默认可用 `FileCredentialStore`）。
    pub fn new(store: Arc<dyn CredentialStore>) -> Self {
        Self {
            meta: PluginMeta {
                name: "qr-login",
                version: env!("CARGO_PKG_VERSION"),
                description: "B 站扫码登录（generate/poll）",
            },
            provider: Arc::new(DefaultCredentialProvider::new(store)),
            http: std::sync::OnceLock::new(),
        }
    }

    fn http(&self) -> Result<&reqwest::Client> {
        self.http.get().ok_or_else(|| BiliError::InvalidData("插件尚未 init".into()))
    }
}

#[async_trait]
impl Plugin for QrAuthPlugin {
    fn meta(&self) -> &PluginMeta {
        &self.meta
    }

    async fn init(&self, ctx: &PluginContext) -> Result<()> {
        let _ = self.http.set(ctx.http.clone());
        Ok(())
    }
}

#[async_trait]
impl bili_core::auth::AuthPlugin for QrAuthPlugin {
    async fn login(&self) -> Result<Box<dyn bili_core::auth::LoginFlow>> {
        let http = self.http()?;

        #[derive(serde::Deserialize)]
        struct GenData {
            url: String,
            qrcode_key: String,
        }
        #[derive(serde::Deserialize)]
        struct GenResp {
            code: i64,
            #[serde(default)]
            message: String,
            data: GenData,
        }

        let resp: GenResp = http
            .get(GENERATE_URL)
            .header(reqwest::header::USER_AGENT, UA)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        if resp.code != 0 {
            return Err(BiliError::Api { code: resp.code, message: resp.message });
        }

        // 渲染 PNG（图形 UI 用）；终端渲染由调用方基于 qr_content 自行完成。
        let png = render_png(&resp.data.url)?;

        // 无重定向客户端：用于手动跟随登录跳转链收集 Set-Cookie。
        let no_redirect = reqwest::Client::builder()
            .user_agent(UA)
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| BiliError::InvalidData(format!("构建 HTTP 客户端失败: {e}")))?;

        Ok(Box::new(QrFlow {
            http: http.clone(),
            no_redirect,
            provider: self.provider.clone(),
            qrcode_key: resp.data.qrcode_key,
            content: resp.data.url,
            png,
        }))
    }

    async fn logout(&self) -> Result<()> {
        // 服务端作废（主站 header 同款端点，从 bundle 挖出）；
        // 浏览器侧该请求同时携带 Cookie 与 csrf/biliCSRF 两个参数（CSRF 中间件自动补 csrf）。
        if let Ok(creds) = self.provider.get().await {
            let _ = self
                .http()?
                .post(LOGOUT_URL)
                .header(reqwest::header::USER_AGENT, UA)
                .header(reqwest::header::REFERER, "https://www.bilibili.com/")
                .header(reqwest::header::COOKIE, creds.cookie_header())
                .form(&[
                    ("biliCSRF", creds.bili_jct.as_str()),
                    ("csrf", creds.bili_jct.as_str()),
                ])
                .timeout(Duration::from_secs(10))
                .send()
                .await;
        }
        self.provider.clear().await
    }

    fn credentials(&self) -> Arc<dyn CredentialProvider> {
        self.provider.clone()
    }
}

/// 渲染二维码 PNG（图形 UI 用）。
fn render_png(content: &str) -> Result<Vec<u8>> {
    let code = qrcode::QrCode::new(content.as_bytes())
        .map_err(|e| BiliError::InvalidData(format!("二维码编码失败: {e}")))?;
    let img: image::ImageBuffer<image::Rgba<u8>, Vec<u8>> = code
        .render::<image::Rgba<u8>>()
        .quiet_zone(true)
        .min_dimensions(4, 4)
        .build();
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| BiliError::InvalidData(format!("PNG 编码失败: {e}")))?;
    Ok(png)
}

/// poll 端点的 data 结构（成功时含 url/refresh_token）。
#[derive(serde::Deserialize)]
#[allow(dead_code)]
struct PollData {
    code: i64,
    #[serde(default)]
    url: String,
    #[serde(default)]
    refresh_token: String,
}

#[derive(serde::Deserialize)]
#[allow(dead_code)]
struct PollResp {
    code: i64,
    #[serde(default)]
    message: String,
    #[serde(default)]
    data: Option<PollData>,
}

/// 一次登录会话。
pub struct QrFlow {
    http: reqwest::Client,
    no_redirect: reqwest::Client,
    provider: Arc<DefaultCredentialProvider>,
    qrcode_key: String,
    content: String,
    png: Vec<u8>,
}

#[async_trait]
impl bili_core::auth::LoginFlow for QrFlow {
    fn qr_content(&self) -> &str {
        &self.content
    }

    fn qr_png(&self) -> &[u8] {
        &self.png
    }

    async fn poll(&mut self) -> Result<LoginEvent> {
        let resp = self
            .http
            .get(POLL_URL)
            .query(&[("qrcode_key", self.qrcode_key.as_str())])
            .timeout(Duration::from_secs(10))
            .send()
            .await?
            .error_for_status()?;

        // poll 响应自身的 Set-Cookie（成功时 Cookie 很可能在此下发）
        let set_cookies: Vec<String> = resp
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok().map(String::from))
            .collect();

        let resp_json: PollResp = resp.json().await?;
        let data = resp_json
            .data
            .ok_or(BiliError::InvalidData("poll 响应缺少 data".into()))?;

        match data.code {
            86101 => Ok(LoginEvent::WaitingScan),
            86090 => Ok(LoginEvent::Scanned),
            86038 => Ok(LoginEvent::Expired),
            0 => {
                let creds = credentials_from_login(&self.no_redirect, &data, set_cookies).await?;
                self.provider.update(&creds).await?;
                Ok(LoginEvent::Success(Box::new(creds)))
            }
            code => Err(BiliError::Api { code, message: resp_json.message }),
        }
    }
}

/// `Set-Cookie: name=value; Path=/; ...` → (name, value)。
pub(crate) fn parse_set_cookie(s: &str) -> Option<(String, String)> {
    let end = s.find(';').unwrap_or(s.len());
    let (name, value) = s[..end].trim().split_once('=')?;
    Some((name.trim().to_string(), value.trim().to_string()))
}

/// 手动跟随登录跳转链（最多 5 跳），收集每跳的 Set-Cookie。
async fn collect_url_cookies(client: &reqwest::Client, url: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut current = url.to_string();
    for _ in 0..5 {
        let Ok(resp) = client.get(&current).send().await else { break };
        for v in resp.headers().get_all(reqwest::header::SET_COOKIE) {
            if let Ok(s) = v.to_str() {
                if let Some(kv) = parse_set_cookie(s) {
                    out.push(kv);
                }
            }
        }
        let Some(loc) = resp.headers().get(reqwest::header::LOCATION).and_then(|v| v.to_str().ok()) else { break };
        let Ok(joined) = Url::parse(&current).and_then(|base| base.join(loc)) else { break };
        current = joined.to_string();
    }
    out
}

/// 从 poll 成功响应构建凭据：a) 响应 Set-Cookie → b) url 查询参数（仅兜底关键字段） → c) 跳转链 Set-Cookie。
/// 注意：url 查询参数不是 Cookie，绝不混入 cookies 表（否则会破坏 Cookie 头）。
async fn credentials_from_login(
    no_redirect: &reqwest::Client,
    data: &PollData,
    set_cookies: Vec<String>,
) -> Result<Credentials> {
    let mut real_cookies: BTreeMap<String, String> = BTreeMap::new();

    // a) poll 响应 Set-Cookie
    for sc in &set_cookies {
        if let Some(kv) = parse_set_cookie(sc) {
            real_cookies.insert(kv.0, kv.1);
        }
    }

    // b) data.url 查询参数——只读，不写入 cookies 表
    let mut url_params: BTreeMap<String, String> = BTreeMap::new();
    if let Ok(login_url) = Url::parse(&data.url) {
        for (k, v) in login_url.query_pairs() {
            url_params.insert(k.into_owned(), v.into_owned());
        }
    }

    // c) 仍缺 SESSDATA 时，按浏览器行为 GET data.url 跟随跳转链收集 Cookie
    if !real_cookies.contains_key("SESSDATA") && !data.url.is_empty() {
        for kv in collect_url_cookies(no_redirect, &data.url).await {
            real_cookies.entry(kv.0).or_insert(kv.1);
        }
    }

    let get = |key: &str| -> Option<String> {
        real_cookies.get(key).cloned().or_else(|| url_params.get(key).cloned())
    };

    let sessdata = get("SESSDATA").ok_or_else(|| {
        let path = Url::parse(&data.url)
            .map(|u| format!("{}{}", u.host_str().unwrap_or(""), u.path()))
            .unwrap_or_default();
        let names: Vec<String> = real_cookies.keys().cloned().collect();
        BiliError::InvalidData(format!(
            "未取得 SESSDATA；url={path}；已收集 cookie 名={names:?}；url参数={:?}",
            url_params.keys().collect::<Vec<_>>()
        ))
    })?;
    let bili_jct = get("bili_jct")
        .ok_or_else(|| BiliError::InvalidData("未取得 bili_jct".into()))?;
    let dede_user_id: u64 = get("DedeUserID")
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| BiliError::InvalidData("未取得/非法 DedeUserID".into()))?;

    // Expires 语义未完全验证（实测 Cookie 过期 = 登录 + 180 天）；能解析出未来时间戳则采用。
    let expires_at = get("Expires")
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|t| *t > now_unix() + 60)
        .or(Some(now_unix() + 180 * 86_400));

    // 设备指纹：优先取响应链里的 buvid；没有则调用 finger/spi（不登录也可取），失败不阻断。
    let (buvid3, buvid4) = match (real_cookies.get("buvid3"), real_cookies.get("buvid4")) {
        (Some(b3), Some(b4)) => (Some(b3.clone()), Some(b4.clone())),
        _ => fetch_buvids(no_redirect).await,
    };

    Ok(Credentials {
        sessdata,
        bili_jct,
        dede_user_id,
        dede_user_id_ck_md5: get("DedeUserID__ckMd5"),
        buvid3,
        buvid4,
        refresh_token: (!data.refresh_token.is_empty()).then_some(data.refresh_token.clone()),
        created_at: now_unix(),
        expires_at,
        cookies: real_cookies,
    })
}

async fn fetch_buvids(http: &reqwest::Client) -> (Option<String>, Option<String>) {
    #[derive(serde::Deserialize)]
    struct SpiData {
        #[serde(default, rename = "b_3")]
        b3: String,
        #[serde(default, rename = "b_4")]
        b4: String,
    }
    #[derive(serde::Deserialize)]
    #[allow(dead_code)]
    struct SpiResp {
        code: i64,
        #[serde(default)]
        data: Option<SpiData>,
    }

    let resp: Option<SpiResp> = async {
        let r = http
            .get(SPI_URL)
            .header(reqwest::header::USER_AGENT, UA)
            .timeout(Duration::from_secs(8))
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?
            .json::<SpiResp>()
            .await
            .ok()?;
        Some(r)
    }
    .await;

    match resp.and_then(|r| r.data) {
        Some(d) if !d.b3.is_empty() => (Some(d.b3), Some(d.b4)),
        _ => (None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 二维码内容必须能被 URL 解析（来自 generate 的 data.url）。
    #[test]
    fn qr_content_is_url() {
        let u = Url::parse("https://passport.bilibili.com/h5-app/passport/login/scan?navhide=1&qrcode_key=abc").unwrap();
        assert_eq!(u.host_str().unwrap(), "passport.bilibili.com");
    }

    #[test]
    fn parse_set_cookie_works() {
        let (n, v) = parse_set_cookie("SESSDATA=abc%2Cdef; Path=/; Domain=.bilibili.com; HttpOnly").unwrap();
        assert_eq!(n, "SESSDATA");
        assert_eq!(v, "abc%2Cdef");
        assert!(parse_set_cookie("invalid").is_none());
    }
}
