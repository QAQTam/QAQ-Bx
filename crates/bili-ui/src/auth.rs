//! 登录桥接层：把 bili-plugin-login / bili-core 的异步服务装配到 reactor 后台模型。
//!
//! - 复用 `crate::runtime::shared_runtime()` 与 `block_on`，不再为每个任务自建 Tokio runtime；
//! - 数据目录统一走 `bili_core::resolve_data_dir()`（`BILIPLAY_DATA_DIR` 或 `<cwd>/data`）；
//! - 装配 `TicketManager`（`ticket.json`）进共享 `NavClient`，并在启动探测阶段预热 `bili_ticket` 与执行自动续期检查。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bili_core::account::{NavClient, VipStatus};
use bili_core::auth::{AuthPlugin as _, LoginEvent, LoginFlow};
use bili_core::credentials::{CredentialProvider, FileCredentialStore};
use bili_core::notify::NotifyClient;
use bili_core::plugin::{PluginContext, PluginRegistry};
use bili_core::resolve_data_dir;
use bili_core::ticket::{FileTicketStore, TicketManager, TICKET_FILE};
use bili_plugin_login::{QrAuthPlugin, CREDENTIALS_FILE};
use windows_reactor::EncodedImage;

use crate::runtime::block_on;

const UA: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/157.0.0.0 Safari/537.36";

/// 登录会话共享槽（shell 与后台轮询闭包各持一份 Arc）。
pub type FlowCell = Arc<Mutex<Option<Box<dyn LoginFlow>>>>;

/// UI 侧登录态（footer 与登录页共用）。
#[derive(Clone)]
pub enum AuthState {
    /// 正在探测。
    Unknown,
    /// 未登录（无凭据或 nav 确认失效）。
    Guest,
    LoggedIn(UserInfo),
}

/// 启动/登录后探测回报（含自动续期状态告警，如果有）。
#[derive(Clone)]
pub struct ProbeReport {
    pub state: AuthState,
    pub notice: Option<String>,
}

/// 用户资料的 UI 视图投影（`bili_core::account::UserProfile` + 头像位图）。
#[derive(Clone)]
pub struct UserInfo {
    pub uname: String,
    pub mid: u64,
    pub level: u8,
    pub current_exp: u64,
    pub next_exp: Option<u64>,
    pub coins: f64,
    pub vip: VipStatus,
    pub vip_label: String,
    pub vip_due_date: u64,
    pub official_title: String,
    /// 用户空间主页（由 mid 派生）。
    pub space_url: String,
    pub avatar: Option<EncodedImage>,
}

/// 把 hdslb 头像 URL 裁成 `@96w_96h_1c.{webp|jpg}` 轻量缩略图
/// (格式由系统 WebP 解码能力探测决定,见 `codec` 模块)。
pub fn avatar_thumbnail_url(raw: &str) -> String {
    avatar_thumbnail_url_with(raw, crate::codec::raster_suffix())
}

fn avatar_thumbnail_url_with(raw: &str, suffix: &str) -> String {
    let trimmed = raw.trim();
    let url = if let Some(rest) = trimmed.strip_prefix("//") {
        format!("https://{rest}")
    } else {
        trimmed.replacen("http://", "https://", 1)
    };
    if url.contains(".hdslb.com/") && !url.contains('@') && !url.contains('?') {
        let lower = url.to_ascii_lowercase();
        if lower.ends_with(".jpg")
            || lower.ends_with(".jpeg")
            || lower.ends_with(".png")
            || lower.ends_with(".webp")
        {
            return format!("{url}@96w_96h_1c.{suffix}");
        }
    }
    url
}

/// 轮询事件投给 UI 的视图（不携带凭据本体）。
#[derive(Clone, Debug)]
pub enum PollOutcome {
    WaitingScan,
    Scanned,
    Expired,
    Success,
    Error(String),
}

/// 登录页流程状态。
#[derive(Clone)]
pub enum LoginFlowState {
    Idle,
    Starting,
    AwaitingScan { qr: EncodedImage },
    Scanned { qr: EncodedImage },
    Expired { qr: EncodedImage },
    Failed(String),
}

/// 桥接服务：装配凭据存储 + 扫码插件 + `bili_ticket` 管理器 + 共享 `NavClient` / `NotifyClient`。
pub struct Auth {
    data_dir: PathBuf,
    plugin: Arc<QrAuthPlugin>,
    http: reqwest::Client,
    ticket: Arc<TicketManager>,
    /// nav 统一客户端：用户资料/WBI 密钥共用（登录成功或登出后作废缓存）。
    nav: Arc<NavClient>,
    /// 通知中心客户端（跨刷新复用设置缓存）。
    notify: Arc<NotifyClient>,
}

impl Auth {
    /// 数据目录（与 CLI 共用 `bili_core::resolve_data_dir()`）。
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// 凭据提供者（供 FeedClient 等核心客户端装配）。
    pub fn credential_provider(&self) -> Arc<dyn CredentialProvider> {
        self.plugin.credentials()
    }

    /// 装配好的 HTTP 客户端（UA/超时与本服务一致）。
    pub fn http(&self) -> reqwest::Client {
        self.http.clone()
    }

    /// 组装：FileCredentialStore + QrAuthPlugin + TicketManager + NavClient。
    pub fn new() -> Self {
        let data_dir = resolve_data_dir();
        let store = Arc::new(FileCredentialStore::new(data_dir.join(CREDENTIALS_FILE)));
        let plugin = Arc::new(QrAuthPlugin::new(store));
        let http = reqwest::Client::builder()
            .user_agent(UA)
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(15))
            .build()
            .expect("构建 HTTP 客户端失败");

        let mut registry = PluginRegistry::new();
        registry.register(plugin.clone());
        block_on(async {
            registry
                .init_all(&PluginContext {
                    http: http.clone(),
                    data_dir: data_dir.clone(),
                })
                .await
                .expect("登录插件初始化失败");
        });

        let ticket_store = Arc::new(FileTicketStore::new(data_dir.join(TICKET_FILE)));
        let ticket = Arc::new(TicketManager::new(
            http.clone(),
            plugin.credentials(),
            Some(ticket_store),
        ));
        let nav = Arc::new(NavClient::with_ticket(
            http.clone(),
            plugin.credentials(),
            ticket.clone(),
        ));
        let notify = Arc::new(NotifyClient::with_nav(
            http.clone(),
            plugin.credentials(),
            nav.clone(),
        ));
        Self {
            data_dir,
            plugin,
            http,
            ticket,
            nav,
            notify,
        }
    }

    /// nav 客户端（供 shell 装配 feed/播放/弹幕客户端，共用一份 nav 与 ticket 缓存）。
    pub fn nav_client(&self) -> Arc<NavClient> {
        self.nav.clone()
    }

    /// 通知中心客户端（复用同一份凭证、HTTP 客户端与共享 nav）。
    pub fn notify_client(&self) -> Arc<NotifyClient> {
        self.notify.clone()
    }

    /// 探测登录态 + 启动自动续期 + `bili_ticket` 预热。
    pub fn probe_blocking(&self) -> ProbeReport {
        block_on(self.probe_async())
    }

    async fn probe_async(&self) -> ProbeReport {
        // 1) 无本地凭据 → 预热匿名 `bili_ticket` 后立即返回 Guest（离线秒回）。
        if self.plugin.credentials().get().await.is_err() {
            let _ = self.ticket.ensure().await;
            return ProbeReport {
                state: AuthState::Guest,
                notice: None,
            };
        }

        // 2) 检查是否需要 Cookie 自动续期（失败不阻断，仍尝试用当前凭据探测 nav）。
        let mut notice: Option<String> = None;
        match self.plugin.needs_refresh().await {
            Ok(true) => match self.plugin.refresh().await {
                Ok(refreshed) => {
                    eprintln!("[auth] Cookie 已自动续期（mid={}）", refreshed.dede_user_id);
                    self.nav.invalidate();
                    self.ticket.invalidate();
                    self.notify.invalidate();
                }
                Err(err) => {
                    let msg = format!("Cookie 自动续期失败（将沿用现有凭据）：{err}");
                    eprintln!("[auth] {msg}");
                    notice = Some(msg);
                }
            },
            Ok(false) => {}
            Err(err) => {
                eprintln!("[auth] 检查 Cookie 续期失败：{err}");
            }
        }

        // 3) 预热或续签 3 天风控票据（best-effort，失败不阻塞 UI）。
        if let Err(err) = self.ticket.ensure().await {
            eprintln!("[ticket] GenWebTicket 预热失败：{err}");
        }

        // 4) 拉取 nav 用户资料 + 下载头像。
        let snap = match self.nav.snapshot().await {
            Ok(snap) => snap,
            Err(_) => {
                return ProbeReport {
                    state: AuthState::Guest,
                    notice,
                };
            }
        };
        let profile = snap.profile;
        if !profile.is_login {
            return ProbeReport {
                state: AuthState::Guest,
                notice,
            };
        }
        let avatar_url = avatar_thumbnail_url(&profile.face);
        let avatar = self.fetch_image_async(&avatar_url).await;
        let space_url = profile.space_url();
        ProbeReport {
            state: AuthState::LoggedIn(UserInfo {
                uname: profile.uname,
                mid: profile.mid,
                level: profile.level,
                current_exp: profile.current_exp,
                next_exp: profile.next_exp,
                coins: profile.coins,
                vip: profile.vip,
                vip_label: profile.vip_label,
                vip_due_date: profile.vip_due_date,
                official_title: profile.official_title,
                space_url,
                avatar,
            }),
            notice,
        }
    }

    /// 发起扫码登录：generate → QR PNG 存入共享槽。返回可展示的二维码位图。
    pub fn start_login_blocking(&self, cell: &FlowCell) -> Result<EncodedImage, String> {
        let flow = block_on(self.plugin.login()).map_err(|e| e.to_string())?;
        let png = flow.qr_png().to_vec();
        *cell.lock().expect("登录会话槽中毒") = Some(flow);
        Ok(EncodedImage::new(png))
    }

    /// 轮询一次（shell 每 2s tick）。终态（失效/成功）时清空共享槽。
    pub fn poll_blocking(&self, cell: &FlowCell) -> PollOutcome {
        let outcome = {
            let mut guard = cell.lock().expect("登录会话槽中毒");
            let Some(flow) = guard.as_mut() else {
                return PollOutcome::Error("登录会话不存在".into());
            };
            match block_on(flow.poll()) {
                Ok(LoginEvent::WaitingScan) => PollOutcome::WaitingScan,
                Ok(LoginEvent::Scanned) => PollOutcome::Scanned,
                Ok(LoginEvent::Expired) => PollOutcome::Expired,
                Ok(LoginEvent::Success(_creds)) => PollOutcome::Success,
                Err(e) => PollOutcome::Error(e.to_string()),
            }
        };
        if matches!(outcome, PollOutcome::Expired | PollOutcome::Success) {
            *cell.lock().expect("登录会话槽中毒") = None;
        }
        if matches!(outcome, PollOutcome::Success) {
            // 新凭据已落盘 → 清空旧 nav 与 ticket 缓存，让 probe 用新会话重拉。
            self.nav.invalidate();
            self.ticket.invalidate();
            self.notify.invalidate();
        }
        outcome
    }

    /// 登出：服务端作废 + 清除本地凭据与票据缓存（不可逆——UI 侧需二次确认）。
    pub fn logout_blocking(&self) -> Result<(), String> {
        block_on(self.plugin.logout()).map_err(|e| e.to_string())?;
        self.nav.invalidate();
        self.ticket.invalidate();
        self.notify.invalidate();
        Ok(())
    }

    /// 异步下载头像/封面静态图（hdslb 静态资源无需 Referer）。
    pub async fn fetch_image_async(&self, url: &str) -> Option<EncodedImage> {
        if url.is_empty() {
            return None;
        }
        let bytes = self
            .http
            .get(url)
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .ok()?
            .bytes()
            .await
            .ok()?;
        Some(EncodedImage::new(bytes.to_vec()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avatar_thumbnail_url_matches_codec_suffix() {
        // 双格式分支由参数化版本锁定(webp/jpg 由运行时探测决定)。
        assert_eq!(
            avatar_thumbnail_url_with("//i2.hdslb.com/bfs/face/u1.jpg", "webp"),
            "https://i2.hdslb.com/bfs/face/u1.jpg@96w_96h_1c.webp"
        );
        assert_eq!(
            avatar_thumbnail_url_with("http://i2.hdslb.com/bfs/face/u1.jpg", "jpg"),
            "https://i2.hdslb.com/bfs/face/u1.jpg@96w_96h_1c.jpg"
        );
        // 运行时版本与参数化版本一致(探测结果只影响后缀)。
        assert_eq!(
            avatar_thumbnail_url("https://i2.hdslb.com/bfs/face/u1.jpg"),
            avatar_thumbnail_url_with("https://i2.hdslb.com/bfs/face/u1.jpg", crate::codec::raster_suffix())
        );
        // 已有参数 → 不二次拼接;非图床保持不动。
        assert_eq!(
            avatar_thumbnail_url_with("https://i2.hdslb.com/bfs/face/u1.jpg@50w.jpg", "webp"),
            "https://i2.hdslb.com/bfs/face/u1.jpg@50w.jpg"
        );
        assert_eq!(
            avatar_thumbnail_url_with("https://example.com/a.jpg", "webp"),
            "https://example.com/a.jpg"
        );
    }
}
