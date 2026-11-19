//! 通过 `windows-bindgen` 从 WinUI 3 `.winmd` 直接生成的轻量 WinRT 扩展绑定：
//! - 全局默认字体设置（`Application.Resources["ContentControlThemeFontFamily"]`）；
//! - `ScrollViewer::ChangeView` 精确滚动支持。

#![allow(
    non_snake_case,
    non_camel_case_types,
    non_upper_case_globals,
    dead_code,
    clippy::all
)]

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

/// #7 焦点触发预览卡的锚点:当前焦点元素在根可视(=窗口客户区,DIP)中的矩形。
/// 键盘 Tab / 手柄停留这类无 pointer 坐标的弹出路径使用;任一环节失败返回
/// `None`,调用方放弃本次弹出(不做兜底坐标,防止预览卡悬在错误位置)。
/// 尺寸由布局度量传入(本地矩形经 `TransformBounds` 换算,免依赖 `Bounds` 属性)。
pub fn focused_element_rect(width: f64, height: f64) -> Option<(f64, f64, f64, f64)> {
    let focused = FocusManager::GetFocusedElement().ok()?;
    let element = focused.cast::<UIElement>().ok()?;
    let transform = element.TransformToVisual(None).ok()?;
    let rect = transform
        .TransformBounds(Rect {
            x: 0.0,
            y: 0.0,
            width: width as f32,
            height: height as f32,
        })
        .ok()?;
    let (rx, ry, rw, rh) = (
        f64::from(rect.x),
        f64::from(rect.y),
        f64::from(rect.width),
        f64::from(rect.height),
    );
    if !(rx.is_finite() && ry.is_finite() && rw.is_finite() && rh.is_finite())
        || rw <= 0.0
        || rh <= 0.0
    {
        return None;
    }
    Some((rx, ry, rw, rh))
}

/// 切换指定 WinUI 窗口的全屏/常规呈现模式（需在 UI 线程调用）。
pub fn set_window_fullscreen(hwnd_raw: *mut core::ffi::c_void, fullscreen: bool) -> bool {
    if hwnd_raw.is_null() {
        return false;
    }
    let id = WindowId {
        value: hwnd_raw as usize as u64,
    };
    let Ok(app_window) = AppWindow::GetFromWindowId(id) else {
        return false;
    };
    let kind = if fullscreen {
        AppWindowPresenterKind::FullScreen
    } else {
        AppWindowPresenterKind::Default
    };
    app_window.SetPresenterByKind(kind).is_ok()
}

/// 查询指定 WinUI 窗口当前是否处于系统全屏呈现模式（需在 UI 线程调用）。
pub fn is_window_fullscreen(hwnd_raw: *mut core::ffi::c_void) -> Option<bool> {
    if hwnd_raw.is_null() {
        return None;
    }
    let id = WindowId {
        value: hwnd_raw as usize as u64,
    };
    let app_window = AppWindow::GetFromWindowId(id).ok()?;
    let presenter = app_window.Presenter().ok()?;
    let kind = presenter.Kind().ok()?;
    Some(kind == AppWindowPresenterKind::FullScreen)
}

/// 竖屏模式:没收/恢复系统最大化钮(presenter 级;未就绪或类型不符返回 false)。
/// 全屏播放走 FullScreen presenter,与本开关正交;退出全屏回 Default presenter
/// 后需重按一次(presenter 设置可能随 kind 切换复位)。
pub fn set_maximizable(hwnd_raw: *mut core::ffi::c_void, enabled: bool) -> bool {
    if hwnd_raw.is_null() {
        return false;
    }
    let id = WindowId {
        value: hwnd_raw as usize as u64,
    };
    let Ok(app_window) = AppWindow::GetFromWindowId(id) else {
        eprintln!("[portrait] set_maximizable: GetFromWindowId 失败");
        return false;
    };
    let Ok(presenter) = app_window.Presenter() else {
        eprintln!("[portrait] set_maximizable: Presenter() 失败");
        return false;
    };
    let Ok(overlapped) = presenter.cast::<OverlappedPresenter>() else {
        let kind = presenter
            .Kind()
            .map(|k| format!("{k:?}"))
            .unwrap_or_else(|_| "?".into());
        eprintln!("[portrait] set_maximizable: 非 OverlappedPresenter(kind={kind})");
        return false;
    };
    let ok = overlapped.SetIsMaximizable(enabled).is_ok();
    eprintln!("[portrait] set_maximizable({enabled}) → {ok}");
    ok
}

/// 竖屏模式「贴边最大化」:右缘贴边 × 邻近屏工作区全高,客户区宽对齐
/// `client_width_dips`(DIP→像素按当前 `inner.width / current_client_dips`
/// 推算,不依赖任何 DPI API)。返回停靠前外框 (x, y, w, h) 物理像素供还原;
/// 失败返回 None(调用方保持现状)。
pub fn dock_window_right(
    hwnd_raw: *mut core::ffi::c_void,
    client_width_dips: f64,
    current_client_dips: f64,
) -> Option<(i32, i32, i32, i32)> {
    if hwnd_raw.is_null() || !client_width_dips.is_finite() || client_width_dips <= 0.0 {
        return None;
    }
    let id = WindowId {
        value: hwnd_raw as usize as u64,
    };
    let app_window = AppWindow::GetFromWindowId(id).ok()?;
    let area = DisplayArea::GetFromWindowId(id, DisplayAreaFallback::Nearest).ok()?;
    let work = area.WorkArea().ok()?;
    let outer = app_window.Size().ok()?;
    let inner = app_window
        .cast::<IAppWindow2>()
        .ok()?
        .ClientSize()
        .ok()?;
    let prev = (
        app_window.Position().ok()?.x,
        app_window.Position().ok()?.y,
        outer.width,
        outer.height,
    );
    let scale = if current_client_dips.is_finite() && current_client_dips > 0.0 {
        f64::from(inner.width) / current_client_dips
    } else {
        1.0
    };
    let non_client_width = outer.width.saturating_sub(inner.width);
    let target_client_w = (client_width_dips * scale).round().max(1.0) as i32;
    let target_outer_w = target_client_w.saturating_add(non_client_width);
    let target_outer_h = work.height.max(1);
    let x = work.x.saturating_add(work.width.saturating_sub(target_outer_w));
    app_window
        .MoveAndResize(RectInt32 {
            x,
            y: work.y,
            width: target_outer_w,
            height: target_outer_h,
        })
        .ok()?;
    Some(prev)
}

/// 还原 `dock_window_right` 记录的停靠前外框几何(工作队列线程调用)。
pub fn restore_window_bounds(
    hwnd_raw: *mut core::ffi::c_void,
    bounds: (i32, i32, i32, i32),
) -> bool {
    let (x, y, width, height) = bounds;
    if hwnd_raw.is_null() || width <= 0 || height <= 0 {
        return false;
    }
    let id = WindowId {
        value: hwnd_raw as usize as u64,
    };
    let Ok(app_window) = AppWindow::GetFromWindowId(id) else {
        return false;
    };
    app_window
        .MoveAndResize(RectInt32 {
            x,
            y,
            width,
            height,
        })
        .is_ok()
}

/// 竖屏模式 presenter 宽度约束与几何钳制：
/// - 退出全屏 (`SetPresenterByKind(Default)`) 后，新 presenter 的 `PreferredMaximumWidth`
///   等约束可能被重置，且运行时开启竖屏模式时当前窗口宽度可能超出 `max_client_width_dips`；
/// - 本函数在 UI 线程重申 `IOverlappedPresenter3` 最小/最大宽高硬钳制，并在 `shrink_if_wider`
///   为真且当前客户区超宽时，主动将窗口外框宽度收缩至 `max_client_width_dips`。
pub fn enforce_portrait_window_policy(
    hwnd_raw: *mut core::ffi::c_void,
    min_client_width_dips: f64,
    min_client_height_dips: f64,
    max_client_width_dips: Option<f64>,
    current_client_dips: f64,
    shrink_if_wider: bool,
) -> bool {
    if hwnd_raw.is_null() {
        return false;
    }
    let id = WindowId {
        value: hwnd_raw as usize as u64,
    };
    let Ok(app_window) = AppWindow::GetFromWindowId(id) else {
        return false;
    };
    let Ok(outer) = app_window.Size() else {
        return false;
    };
    let Ok(inner) = app_window.cast::<IAppWindow2>().and_then(|w| w.ClientSize()) else {
        return false;
    };
    let scale = if current_client_dips.is_finite() && current_client_dips > 0.0 && inner.width > 0 {
        (f64::from(inner.width) / current_client_dips).clamp(0.5, 4.0)
    } else {
        1.0
    };
    let non_client_w = outer.width.saturating_sub(inner.width);
    let non_client_h = outer.height.saturating_sub(inner.height);
    let to_outer_w = |dips: f64| ((dips * scale).round().max(1.0) as i32).saturating_add(non_client_w);
    let to_outer_h = |dips: f64| ((dips * scale).round().max(1.0) as i32).saturating_add(non_client_h);

    if let Ok(presenter) = app_window.Presenter()
        && let Ok(p3) = presenter.cast::<IOverlappedPresenter3>()
    {
        let min_w = Some(to_outer_w(min_client_width_dips));
        let min_h = Some(to_outer_h(min_client_height_dips));
        let max_w = max_client_width_dips.map(to_outer_w);
        let _ = p3.SetPreferredMinimumWidth(min_w);
        let _ = p3.SetPreferredMinimumHeight(min_h);
        let _ = p3.SetPreferredMaximumWidth(max_w);
        let _ = p3.SetPreferredMaximumHeight(None);
    }

    if shrink_if_wider
        && let Some(max_dips) = max_client_width_dips
        && max_dips.is_finite()
        && max_dips > 0.0
    {
        let max_outer_w = to_outer_w(max_dips);
        if outer.width > max_outer_w
            && let Ok(pos) = app_window.Position()
        {
            let min_outer_h = to_outer_h(min_client_height_dips);
            let _ = app_window.MoveAndResize(RectInt32 {
                x: pos.x,
                y: pos.y,
                width: max_outer_w,
                height: outer.height.max(min_outer_h),
            });
        }
    }
    true
}

/// 当前焦点是否位于文本输入框（或其模板子元素）内，防止单键播放器快捷键劫持打字。
pub fn is_text_input_focused() -> bool {
    let Ok(focused) = FocusManager::GetFocusedElement() else {
        return false;
    };
    if focused.cast::<TextBox>().is_ok() {
        return true;
    }
    let Ok(mut current) = focused.cast::<DependencyObject>() else {
        return false;
    };
    for _ in 0..16 {
        if current.cast::<TextBox>().is_ok() {
            return true;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fullscreen_helpers_handle_null_hwnd_without_panic() {
        assert!(!set_window_fullscreen(std::ptr::null_mut(), true));
        assert!(!set_window_fullscreen(std::ptr::null_mut(), false));
        assert_eq!(is_window_fullscreen(std::ptr::null_mut()), None);
        assert!(!enforce_portrait_window_policy(
            std::ptr::null_mut(),
            390.0,
            640.0,
            Some(859.0),
            859.0,
            true,
        ));
    }
}
