//! 通过 `windows-bindgen` 从 WinUI 3 `.winmd` 直接生成的轻量 WinRT 扩展绑定：
//! - 全局默认字体设置（`Application.Resources["ContentControlThemeFontFamily"]`）；
//! - `ScrollViewer::ChangeView` 精确滚动支持。

#![allow(non_snake_case, non_camel_case_types, non_upper_case_globals, dead_code, clippy::all)]

include!(concat!(env!("OUT_DIR"), "/winrt_bindings.rs"));

use windows_collections::IMap;
use windows_core::{IInspectable, Interface};

/// 为整个 WinUI 3 `Application` 注入全局默认字体（需在 UI 线程调用）。
pub fn apply_global_font(family_name: &str) -> bool {
    let Ok(app) = Application::Current() else {
        return false;
    };
    let Ok(resources) = app.Resources() else {
        return false;
    };
    let Ok(map) = resources.cast::<IMap<IInspectable, IInspectable>>() else {
        return false;
    };
    let Ok(font) = FontFamily::CreateInstanceWithName(family_name) else {
        return false;
    };
    let Ok(font_obj) = font.cast::<IInspectable>() else {
        return false;
    };
    let Ok(key) = PropertyValue::CreateString("ContentControlThemeFontFamily") else {
        return false;
    };
    map.Insert(&key, &font_obj).is_ok()
}

/// 通过当前聚焦控件向上查找最近的 `ScrollViewer` 并执行 `ChangeView` 相对垂直滚动。
pub fn scroll_focused_viewer_by(delta_y: f64) -> bool {
    let Ok(focused) = FocusManager::GetFocusedElement() else {
        return false;
    };
    let Ok(mut current) = focused.cast::<DependencyObject>() else {
        return false;
    };
    for _ in 0..32 {
        if let Ok(sv) = current.cast::<ScrollViewer>() {
            let Ok(cur_y) = sv.VerticalOffset() else {
                return false;
            };
            let target_y = (cur_y + delta_y).max(0.0);
            return sv.ChangeView(None, Some(target_y), None).unwrap_or(false);
        }
        let Ok(parent) = VisualTreeHelper::GetParent(&current) else {
            break;
        };
        current = parent;
    }
    false
}

/// 持有已向 Windows App SDK `AppNotificationManager` 注册的通知激活会话。
pub struct AppNotificationGuard {
    manager: AppNotificationManager,
    _revoker: windows_core::EventRevoker,
}

impl Drop for AppNotificationGuard {
    fn drop(&mut self) {
        let _ = self.manager.Unregister();
    }
}

/// 注册当前进程的 Windows 应用通知激活器（仅建议在 MSIX 打包运行态调用）。
pub fn register_app_notifications(
    on_invoked: std::sync::Arc<dyn Fn() + Send + Sync>,
) -> Option<AppNotificationGuard> {
    let manager = AppNotificationManager::Default().ok()?;
    let revoker = manager
        .NotificationInvoked(move |_, _| {
            on_invoked();
        })
        .ok()?;
    if manager.Register().is_err() {
        return None;
    }
    Some(AppNotificationGuard {
        manager,
        _revoker: revoker,
    })
}
