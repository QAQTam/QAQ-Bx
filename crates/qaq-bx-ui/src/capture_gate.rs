//! 崩溃捕获链路自检门(WER LocalDumps 验证用)。
//!
//! 背景:打包应用(MoAppCrash)的转储捕获只认 HKLM LocalDumps,且必须实测
//! 确认链路真的通,不能等一次自然崩溃才发现配置无效。
//! 触发方式是在数据目录放置哨兵文件 `selftest-av.flag`(仅打包/开发数据目录
//! 可达,普通使用不可能意外命中);命中后在 UI 启动前主动制造一次
//! STATUS_ACCESS_VIOLATION(0xC0000005),与真实崩溃同类,走完全相同的
//! WER → LocalDumps 落盘路径。
//!
//! 见 docs/dev-notes/crash-0x359595-xamlroot.md §5。

use std::path::{Path, PathBuf};

/// 哨兵文件名(放在 resolve_data_dir() 下)。
pub const FLAG_FILE: &str = "selftest-av.flag";

/// 计算哨兵文件完整路径。
pub fn flag_path(data_dir: &Path) -> PathBuf {
    data_dir.join(FLAG_FILE)
}

/// 是否请求了受控自崩。
pub fn requested(data_dir: &Path) -> bool {
    flag_path(data_dir).is_file()
}

/// 主动触发一次 access violation,验证 WER LocalDumps 是否真的落盘。
///
/// `write_volatile` 不会被优化消除,空指针写必发 0xC0000005——与线上崩溃
/// 同异常码、同 WER 路径。理论上不可达的 `abort()` 仅作编译器语义兜底。
#[inline(never)]
pub fn fire() -> ! {
    crate::trace::note(format!(
        "capture-gate: deliberate AV pid={} (flag={})",
        std::process::id(),
        FLAG_FILE
    ));
    unsafe {
        std::ptr::null_mut::<u8>().write_volatile(0x5A);
    }
    std::process::abort()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_detection_roundtrip() {
        let dir = std::env::temp_dir().join(format!("qaqbx-capture-gate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("tempdir");
        let flag = flag_path(&dir);

        assert!(!requested(&dir), "无哨兵文件时必须保持静默");

        std::fs::write(&flag, b"").expect("写哨兵文件");
        assert!(requested(&dir), "哨兵文件存在时应触发");

        std::fs::remove_file(&flag).expect("清哨兵文件");
        assert!(!requested(&dir), "清除后必须恢复静默(验证测试不留下崩溃开关)");
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn flag_never_matches_directory() {
        // 同名目录存在时不算触发(is_file 语义),避免误挂。
        let dir = std::env::temp_dir().join(format!("qaqbx-capture-gate-d-{}", std::process::id()));
        let flag = flag_path(&dir);
        std::fs::create_dir_all(&flag).expect("tempdir");
        assert!(!requested(&dir));
        let _ = std::fs::remove_dir(&flag);
        let _ = std::fs::remove_dir(&dir);
    }
}
