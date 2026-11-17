//! 登录能力契约：登录 / 登出 / 凭据 三个基本操作。

use std::sync::Arc;

use async_trait::async_trait;

use crate::{
    credentials::{CredentialProvider, Credentials},
    error::Result,
    plugin::Plugin,
};

/// 一次扫码登录会话的轮询状态机。
#[derive(Debug, Clone)]
pub enum LoginEvent {
    /// 二维码已展示，等待手机扫码
    WaitingScan,
    /// 已扫码，等待手机端确认
    Scanned,
    /// 二维码已失效（过期或已被消费），需重新发起 login
    Expired,
    /// 登录成功，凭据已就绪（实现方负责持久化）
    Success(Box<Credentials>),
}

/// 登录会话句柄：UI 拿二维码展示，循环 `poll` 直到终态。
#[async_trait]
pub trait LoginFlow: Send {
    /// 二维码内容（B 站返回的登录 URL），调用方自行渲染二维码。
    fn qr_content(&self) -> &str;

    /// 二维码 PNG 字节（图形 UI 直接显示用；可能为空——终端场景自行渲染）。
    fn qr_png(&self) -> &[u8];

    /// 轮询一次登录状态。调用方自行控制节奏（建议 2s）。
    /// 成功（`LoginEvent::Success`）或失效（`Expired`）后本会话不再可用。
    async fn poll(&mut self) -> Result<LoginEvent>;
}

/// ★ 登录插件对外契约：登录 / 登出 / 凭据 三个基本操作。
///
/// 实现方：扫码登录、密码登录、导入 Cookie 文件……均可。
/// 消费方：解析类插件只使用 [`AuthPlugin::credentials`] 返回的抽象。
#[async_trait]
pub trait AuthPlugin: Plugin {
    /// 发起一次登录，返回可轮询的会话句柄。
    async fn login(&self) -> Result<Box<dyn LoginFlow>>;

    /// 登出：作废本地凭据，并尽力通知服务端。
    async fn logout(&self) -> Result<()>;

    /// 凭据出口——给解析类插件注入。
    fn credentials(&self) -> Arc<dyn CredentialProvider>;
}
