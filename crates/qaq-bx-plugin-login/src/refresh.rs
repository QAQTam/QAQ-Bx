//! Cookie 续期链路（docs/bilibili-login-research.md §3，chunk 895 真实顺序）。
//!
//! 官方实现顺序（含保命语义）：
//! 1. `GET cookie/info?csrf=` → `{refresh, timestamp}`（GET，无害探测）
//! 2. `GET /correspond/1/<native path>` SSR 页 → `#1-name`=refresh_csrf、`#1-value`=refresh_token
//!    （path = correspond_path("refresh", timestamp)，原生 RSA-OAEP-SHA256）
//! 3. `POST cookie/refresh` `{refresh_csrf, source:"main_web", refresh_token, csrf}`
//!    → 新 Set-Cookie + `data.refresh_token` → **立即落盘**（此步后新旧 Cookie 短期并存）
//! 4. `POST sso/list` `{csrf}` → `data.sso[]` 逐个 POST（同步登录态到其他域，跳过其 Cookie）
//! 5. `POST confirm/refresh` `{refresh_token: <旧值>, csrf}` → 旧 Cookie 作废 → 再次落盘
//!
//! 任何一步失败：3 之前旧凭据完全有效；3 之后新凭据已落盘仍可用（confirm 未完成不会立即失效）。

use std::time::Duration;

use qaq_bx_core::credentials::{now_unix, CredentialProvider as _, Credentials};
use qaq_bx_core::error::{BiliError, Result};

use super::correspond::correspond_path;
use super::parse_set_cookie;
use super::{QrAuthPlugin, UA};

const COOKIE_INFO_URL: &str = "https://passport.bilibili.com/x/passport-login/web/cookie/info";
const CORRESPOND_BASE: &str = "https://www.bilibili.com/correspond/1/";
const REFRESH_URL: &str = "https://passport.bilibili.com/x/passport-login/web/cookie/refresh";
const SSO_LIST_URL: &str = "https://passport.bilibili.com/x/passport-login/web/sso/list";
const CONFIRM_URL: &str = "https://passport.bilibili.com/x/passport-login/web/confirm/refresh";

/// 从 SSR 页面提取 `id="<id>"` 元素的 innerText（TokenRefresh 组件 getDomText 同款语义）。
fn extract_dom_text(html: &str, id: &str) -> Option<String> {
    let pat = format!(r#"id="{id}""#);
    let i = html.find(&pat)?;
    let after = &html[i + pat.len()..];
    let close = after.find('>')?;
    let rest = &after[close + 1..];
    let end = rest.find('<')?;
    Some(rest[..end].trim().to_string())
}

/// SESSDATA 形如 `<token>%2C<过期unix秒>%2C<hmac>` —— 解析内嵌过期时间。
fn sessdata_expiry(sessdata: &str) -> Option<i64> {
    sessdata
        .split("%2C")
        .nth(1)
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|t| *t > now_unix())
}

/// 收集响应的 Set-Cookie 为 (name, value) 对。
fn collect_set_cookies(resp: &reqwest::Response) -> Vec<(String, String)> {
    resp.headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(parse_set_cookie)
        .collect()
}

impl QrAuthPlugin {
    /// 探测当前凭据是否需要续期（GET cookie/info，无害）。
    pub async fn needs_refresh(&self) -> Result<bool> {
        let creds = self.provider.get().await?;

        #[derive(serde::Deserialize)]
        struct InfoData {
            #[serde(default)]
            refresh: bool,
        }
        #[derive(serde::Deserialize)]
        struct InfoResp {
            code: i64,
            #[serde(default)]
            message: String,
            #[serde(default)]
            data: Option<InfoData>,
        }

        let resp: InfoResp = self
            .http()?
            .get(COOKIE_INFO_URL)
            .query(&[("csrf", creds.bili_jct.as_str())])
            .header(reqwest::header::USER_AGENT, UA)
            .header(reqwest::header::COOKIE, creds.cookie_header())
            .timeout(Duration::from_secs(10))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        if resp.code != 0 {
            return Err(BiliError::Api { code: resp.code, message: resp.message });
        }
        Ok(resp.data.map(|d| d.refresh).unwrap_or(false))
    }

    /// 执行完整续期链路，返回续期后的凭据（已落盘）。
    /// `refresh:false` 时为无操作，原样返回。
    pub async fn refresh(&self) -> Result<Credentials> {
        let creds = self.provider.get().await?;
        let http = self.http()?;
        let cookie_header = creds.cookie_header();

        // ---- 1) 探测 ----
        #[derive(serde::Deserialize)]
        struct InfoData {
            #[serde(default)]
            refresh: bool,
            #[serde(default)]
            timestamp: i64,
        }
        #[derive(serde::Deserialize)]
        struct InfoResp {
            code: i64,
            #[serde(default)]
            message: String,
            #[serde(default)]
            data: Option<InfoData>,
        }
        let info: InfoResp = http
            .get(COOKIE_INFO_URL)
            .query(&[("csrf", creds.bili_jct.as_str())])
            .header(reqwest::header::USER_AGENT, UA)
            .header(reqwest::header::COOKIE, &cookie_header)
            .timeout(Duration::from_secs(10))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        if info.code != 0 {
            return Err(BiliError::Api { code: info.code, message: info.message });
        }
        let Some(info_data) = info.data else {
            return Err(BiliError::InvalidData("cookie/info 响应缺少 data".into()));
        };
        if !info_data.refresh {
            return Ok(creds); // 无需续期，无操作
        }

        // ---- 2) correspond/1 SSR 页：取 refresh_csrf / refresh_token ----
        let hash_path = correspond_path("refresh", info_data.timestamp)?;
        let html = http
            .get(format!("{CORRESPOND_BASE}{hash_path}"))
            .header(reqwest::header::USER_AGENT, UA)
            .header(reqwest::header::COOKIE, &cookie_header)
            .header(reqwest::header::REFERER, "https://www.bilibili.com/")
            .timeout(Duration::from_secs(10))
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        let refresh_csrf = extract_dom_text(&html, "1-name")
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                BiliError::InvalidData("correspond/1 SSR 页缺少 refresh_csrf (#1-name)".into())
            })?;
        let old_refresh_token = extract_dom_text(&html, "1-value")
            .filter(|s| !s.is_empty())
            .or_else(|| creds.refresh_token.clone())
            .ok_or_else(|| {
                BiliError::InvalidData("refresh_token 缺失（SSR #1-value 与本地均为空）".into())
            })?;

        // ---- 3) cookie/refresh：换发新 Cookie + 新 refresh_token ----
        let resp = http
            .post(REFRESH_URL)
            .header(reqwest::header::USER_AGENT, UA)
            .header(reqwest::header::COOKIE, &cookie_header)
            .header(reqwest::header::REFERER, "https://www.bilibili.com/")
            .form(&[
                ("refresh_csrf", refresh_csrf.as_str()),
                ("source", "main_web"),
                ("refresh_token", old_refresh_token.as_str()),
                ("csrf", creds.bili_jct.as_str()),
            ])
            .timeout(Duration::from_secs(15))
            .send()
            .await?;
        let set_cookies = collect_set_cookies(&resp);

        #[derive(serde::Deserialize)]
        struct RefreshData {
            #[serde(default)]
            refresh_token: String,
        }
        #[derive(serde::Deserialize)]
        struct RefreshResp {
            code: i64,
            #[serde(default)]
            message: String,
            #[serde(default)]
            data: Option<RefreshData>,
        }
        let refresh_resp: RefreshResp = resp.json().await?;
        if refresh_resp.code != 0 {
            // 此步失败：旧 Cookie 仍完全有效（尚未 confirm），直接抛错。
            return Err(BiliError::Api { code: refresh_resp.code, message: refresh_resp.message });
        }
        let new_refresh_token = refresh_resp
            .data
            .and_then(|d| (!d.refresh_token.is_empty()).then_some(d.refresh_token))
            .ok_or_else(|| BiliError::InvalidData("cookie/refresh 响应缺少新 refresh_token".into()))?;

        // 用新 Set-Cookie 合并出中间凭据并立即落盘（官方同序：localStorage 先写）。
        let mut mid_cookies = creds.cookies.clone();
        for (k, v) in set_cookies {
            mid_cookies.insert(k, v);
        }
        let sessdata = mid_cookies
            .get("SESSDATA")
            .cloned()
            .unwrap_or_else(|| creds.sessdata.clone());
        let bili_jct = mid_cookies
            .get("bili_jct")
            .cloned()
            .unwrap_or_else(|| creds.bili_jct.clone());
        let mid_expires = sessdata_expiry(&sessdata)
            .or(creds.expires_at)
            .or(Some(now_unix() + 180 * 86_400));
        let mid_creds = Credentials {
            sessdata,
            bili_jct,
            dede_user_id: creds.dede_user_id,
            dede_user_id_ck_md5: mid_cookies
                .get("DedeUserID__ckMd5")
                .cloned()
                .or_else(|| creds.dede_user_id_ck_md5.clone()),
            buvid3: creds.buvid3.clone(),
            buvid4: creds.buvid4.clone(),
            refresh_token: Some(new_refresh_token),
            created_at: creds.created_at,
            expires_at: mid_expires,
            cookies: mid_cookies,
        };
        self.provider.update(&mid_creds).await?;

        // ---- 4) sso/list + 逐域同步（Cookie 属于其他域，不并入 cookies 表）----
        #[derive(serde::Deserialize)]
        struct SsoData {
            #[serde(default)]
            sso: Vec<String>,
        }
        #[derive(serde::Deserialize)]
        struct SsoResp {
            code: i64,
            #[serde(default)]
            data: Option<SsoData>,
        }
        let sso: SsoResp = http
            .post(SSO_LIST_URL)
            .header(reqwest::header::USER_AGENT, UA)
            .header(reqwest::header::COOKIE, mid_creds.cookie_header())
            .header(reqwest::header::REFERER, "https://www.bilibili.com/")
            .form(&[("csrf", mid_creds.bili_jct.as_str())])
            .timeout(Duration::from_secs(10))
            .send()
            .await?
            .json()
            .await?;
        if sso.code == 0 {
            for url in sso.data.into_iter().flat_map(|d| d.sso) {
                let _ = http
                    .post(&url)
                    .header(reqwest::header::USER_AGENT, UA)
                    .header(reqwest::header::REFERER, "https://www.bilibili.com/")
                    .form(&[("csrf", mid_creds.bili_jct.as_str())])
                    .timeout(Duration::from_secs(10))
                    .send()
                    .await;
            }
        }

        // ---- 5) confirm/refresh（旧 refresh_token）→ 旧 Cookie 作废 ----
        let resp = http
            .post(CONFIRM_URL)
            .header(reqwest::header::USER_AGENT, UA)
            .header(reqwest::header::COOKIE, mid_creds.cookie_header())
            .header(reqwest::header::REFERER, "https://www.bilibili.com/")
            .form(&[
                ("refresh_token", old_refresh_token.as_str()),
                ("csrf", mid_creds.bili_jct.as_str()),
            ])
            .timeout(Duration::from_secs(15))
            .send()
            .await?;
        let set_cookies = collect_set_cookies(&resp);

        #[derive(serde::Deserialize)]
        struct ConfirmResp {
            code: i64,
            #[serde(default)]
            message: String,
        }
        let confirm: ConfirmResp = resp.json().await?;
        if confirm.code != 0 {
            // confirm 失败：新 Cookie 已落盘且可用（短期并存），返回新凭据但报告错误。
            return Err(BiliError::Api { code: confirm.code, message: confirm.message });
        }

        // confirm 后的 Set-Cookie 并入，最终落盘。
        let mut final_cookies = mid_creds.cookies.clone();
        for (k, v) in set_cookies {
            final_cookies.insert(k, v);
        }
        let final_creds = Credentials {
            sessdata: final_cookies
                .get("SESSDATA")
                .cloned()
                .unwrap_or_else(|| mid_creds.sessdata.clone()),
            bili_jct: final_cookies
                .get("bili_jct")
                .cloned()
                .unwrap_or_else(|| mid_creds.bili_jct.clone()),
            dede_user_id: mid_creds.dede_user_id,
            dede_user_id_ck_md5: final_cookies
                .get("DedeUserID__ckMd5")
                .cloned()
                .or_else(|| mid_creds.dede_user_id_ck_md5.clone()),
            buvid3: mid_creds.buvid3.clone(),
            buvid4: mid_creds.buvid4.clone(),
            refresh_token: mid_creds.refresh_token.clone(),
            created_at: mid_creds.created_at,
            expires_at: sessdata_expiry(
                final_cookies
                    .get("SESSDATA")
                    .map(String::as_str)
                    .unwrap_or(&mid_creds.sessdata),
            )
            .or(mid_creds.expires_at),
            cookies: final_cookies,
        };
        self.provider.update(&final_creds).await?;
        Ok(final_creds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_dom_text_works() {
        let html = r#"<html><div id="1-name">b7e7abcd1234</div><span>ignored</span><div id="1-value"></div></html>"#;
        assert_eq!(extract_dom_text(html, "1-name").as_deref(), Some("b7e7abcd1234"));
        assert_eq!(extract_dom_text(html, "1-value").as_deref(), Some(""));
        assert_eq!(extract_dom_text(html, "2-name"), None);
    }

    #[test]
    fn sessdata_expiry_parses() {
        let sd = "e5a70e91%2C1810303119%2C01d41%2Ab1CjCbtH0KJIy7tASZz2vjQL3AEWQdjJFKlqNvg";
        let exp = sessdata_expiry(sd).unwrap();
        // 过期时间在未来（1810303119 远大于当前）→ 可解析
        assert!(exp > now_unix());
        assert_eq!(sessdata_expiry("bad"), None);
    }

    #[test]
    fn collect_set_cookies_ignores_attributes() {
        // 直接测 parse_set_cookie 的公共行为经由 collect：构造一个假响应不可行（reqwest 类型），
        // 这里验证 parse 语义（collect 依赖它）。
        let (n, v) = parse_set_cookie("SESSDATA=abc%2C123; Path=/; HttpOnly; Domain=.bilibili.com").unwrap();
        assert_eq!(n, "SESSDATA");
        assert_eq!(v, "abc%2C123");
    }
}
