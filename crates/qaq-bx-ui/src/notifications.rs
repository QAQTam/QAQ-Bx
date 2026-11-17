//! 应用身份与 Windows 通知标识（AUMID + ToastActivatorCLSID）。
//!
//! - `TOAST_ACTIVATOR_CLSID` 必须与 `packaging/appx/AppxManifest.template.xml` 中的
//!   `windows.toastNotificationActivation` 与 `windows.comServer` GUID 保持严格一致。
//! - MSIX / Microsoft Store 打包态由系统自动分配 `<PackageFamilyName>!App` AUMID，
//!   并在应用初始化阶段向 Windows App SDK `AppNotificationManager` 注册 COM 激活器；
//! - 非打包开发运行通过 `SetCurrentProcessExplicitAppUserModelID` 绑定显式 AUMID。

use std::sync::Arc;

use crate::winrt_ext::{self, AppNotificationGuard};

/// 应用对外正式名称（窗口标题 / 标题栏 / 清单显示名）。
pub const APP_DISPLAY_NAME: &str = "QAQ-Bx";

/// 非打包运行时的显式进程 AUMID。
pub const UNPACKAGED_AUMID: &str = "QAQ.QAQ-Bx";

/// Windows 通知激活 COM 类标识符（须与 AppxManifest.xml 保持一致）。
pub const TOAST_ACTIVATOR_CLSID: &str = "133f6e22-a3f9-483c-9839-0be521534e66";

/// 启动命令行中由 Windows COM 通知激活传入的参数前缀。
pub const NOTIFICATION_ACTIVATED_ARG: &str = "----AppNotificationActivated:";

/// 当前进程的 Windows 通知身份摘要。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessNotificationIdentity {
    pub packaged: bool,
    pub aumid: Option<String>,
    pub toast_clsid: &'static str,
}

/// 在创建任何窗口前初始化当前进程的 Windows 通知与任务栏 AUMID。
pub fn init_process_identity() -> ProcessNotificationIdentity {
    let packaged = qaq_bx_core::is_packaged_app();
    let aumid = if packaged {
        query_packaged_aumid()
    } else {
        set_explicit_aumid(UNPACKAGED_AUMID).then(|| UNPACKAGED_AUMID.to_string())
    };
    ProcessNotificationIdentity {
        packaged,
        aumid,
        toast_clsid: TOAST_ACTIVATOR_CLSID,
    }
}

/// 打包运行态下注册 `AppNotificationManager`（在窗口主循环就绪时调用）。
pub fn register_if_packaged(on_invoked: Arc<dyn Fn() + Send + Sync>) -> Option<AppNotificationGuard> {
    if !qaq_bx_core::is_packaged_app() {
        return None;
    }
    winrt_ext::register_app_notifications(on_invoked)
}

/// 判断进程是否通过点击 Windows Toast 通知拉起。
pub fn launched_from_notification() -> bool {
    std::env::args().any(|arg| arg.starts_with(NOTIFICATION_ACTIVATED_ARG))
}

#[cfg(windows)]
fn query_packaged_aumid() -> Option<String> {
    const ERROR_INSUFFICIENT_BUFFER: i32 = 122;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentApplicationUserModelId(
            application_user_model_id_length: *mut u32,
            application_user_model_id: *mut u16,
        ) -> i32;
    }
    let mut len: u32 = 0;
    let rc = unsafe { GetCurrentApplicationUserModelId(&mut len, std::ptr::null_mut()) };
    if rc != ERROR_INSUFFICIENT_BUFFER || len == 0 {
        return None;
    }
    let mut buf = vec![0u16; len as usize];
    let rc = unsafe { GetCurrentApplicationUserModelId(&mut len, buf.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..end]))
}

#[cfg(not(windows))]
fn query_packaged_aumid() -> Option<String> {
    None
}

#[cfg(windows)]
fn set_explicit_aumid(aumid: &str) -> bool {
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn SetCurrentProcessExplicitAppUserModelID(app_id: *const u16) -> i32;
    }
    let wide: Vec<u16> = aumid.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe { SetCurrentProcessExplicitAppUserModelID(wide.as_ptr()) == 0 }
}

#[cfg(not(windows))]
fn set_explicit_aumid(_aumid: &str) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_constants_match_windows_store_constraints() {
        assert_eq!(APP_DISPLAY_NAME, "QAQ-Bx");
        assert!(!UNPACKAGED_AUMID.contains(' '));
        let parts: Vec<&str> = TOAST_ACTIVATOR_CLSID.split('-').collect();
        assert_eq!(
            parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
            vec![8, 4, 4, 4, 12]
        );
        assert!(
            TOAST_ACTIVATOR_CLSID
                .chars()
                .all(|c| c == '-' || c.is_ascii_hexdigit())
        );
    }
}
