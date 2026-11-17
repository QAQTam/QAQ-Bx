//! 图像编解码能力探测(WIC)与图片 URL 格式后缀。
//!
//! WebP 比 JPG 同尺寸省 ~60% 流量(实测 @1280w_720h:20KB vs 53KB),
//! 但 WIC 解码依赖系统编解码器:Win11 22H2+ 或安装了「Web 图像扩展」的
//! Win10 才有。进程启动时探测一次:
//! - 有 → 封面/头像 URL 追加 `.webp` 后缀;
//! - 无 → 回退 `.jpg`,并弹一次提示建议安装扩展(`webp_hint_shown` 落盘防重复)。

use std::sync::OnceLock;

/// WIC WebP 容器格式 GUID(wincodec.h `GUID_ContainerFormatWebP`)。
const GUID_CONTAINER_FORMAT_WEBP: windows_core::GUID =
    windows_core::GUID::from_u128(0x8b8cfb4c_2f41_4f79_9c35_16ccbe026c48);

/// 探测 WIC 是否注册了 WebP 解码器(进程内缓存一次)。
pub fn webp_supported() -> bool {
    static CACHE: OnceLock<bool> = OnceLock::new();
    *CACHE.get_or_init(detect_webp)
}

fn detect_webp() -> bool {
    use windows::Win32::combaseapi::{CoCreateInstance, CoInitializeEx};
    use windows::Win32::wincodec::{CLSID_WICImagingFactory, IWICImagingFactory};
    // 主线程通常已由 reactor 初始化 COM(STA);自测等未初始化线程补一次 MTA。
    // 已初始化为 STA 时返回 RPC_E_CHANGED_MODE,直接忽略(dwcoinit = COINIT_MULTITHREADED)。
    unsafe {
        let _ = CoInitializeEx(None, 0);
        let factory: IWICImagingFactory =
            match CoCreateInstance(&CLSID_WICImagingFactory, None, 1 /* CLSCTX_INPROC_SERVER */) {
                Ok(factory) => factory,
                Err(_) => return false,
            };
        factory
            .CreateDecoder(&GUID_CONTAINER_FORMAT_WEBP, None)
            .is_ok()
    }
}

/// 图片 URL 的格式后缀("webp" / "jpg"),拼在 `@w_h_1c.` 之后。
pub fn raster_suffix() -> &'static str {
    if webp_supported() {
        "webp"
    } else {
        "jpg"
    }
}

/// 用真实 WebP 文件交叉验证 GUID 探测(仅 selftest 调试用):
/// `CreateDecoderFromFilename` 成功而 `CreateDecoder(GUID)` 失败 ⇒ GUID 错误,必须修。
#[cfg(feature = "selftest")]
pub(crate) fn probe_webp_file(path: &std::path::Path) -> bool {
    use windows::Win32::combaseapi::{CoCreateInstance, CoInitializeEx};
    use windows::Win32::wincodec::{CLSID_WICImagingFactory, IWICImagingFactory};
    use windows_core::HSTRING;
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    let _ = bytes; // 仅存在性校验用
    let wide = HSTRING::from(path.as_os_str().to_string_lossy().as_ref());
    unsafe {
        let _ = CoInitializeEx(None, 0);
        let factory: IWICImagingFactory =
            match CoCreateInstance(&CLSID_WICImagingFactory, None, 1) {
                Ok(factory) => factory,
                Err(_) => return false,
            };
        factory.CreateDecoderFromFilename(&wide, None, 0, 0).is_ok()
    }
}

/// 无 WebP 编解码器时的一次性提示弹窗(仅生产构建;selftest 静默)。
#[cfg(not(feature = "selftest"))]
pub fn show_webp_hint_dialog() {
    use windows::Win32::winuser::{MB_ICONINFORMATION, MB_OK, MessageBoxW};
    use windows_core::HSTRING;

    let text = HSTRING::from(
        "检测到系统缺少 WebP 图像解码器,图片加载已回退 JPG。\n\
         建议从 Microsoft Store 安装「Web 图像扩展」(WebP Image Extensions),\n\
         可显著加快封面与头像的加载速度。",
    );
    let caption = HSTRING::from("QAQ-Bx 图片格式提示");
    unsafe {
        let _ = MessageBoxW(
            None,
            &text,
            &caption,
            (MB_ICONINFORMATION | MB_OK) as u32,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detection_is_cached_and_consistent() {
        let first = webp_supported();
        let again = webp_supported();
        assert_eq!(first, again, "探测应进程内缓存,两次结果一致");
    }

    #[test]
    fn suffix_matches_detection() {
        assert_eq!(raster_suffix() == "webp", webp_supported());
    }
}
