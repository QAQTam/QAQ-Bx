//! 本地数据目录解析与原子 JSON 读写助手。
//!
//! 凭据（credentials.json）、设置（settings.json）与风控票据（ticket.json）
//! 共用此处的文件持久化与原子落盘逻辑，避免并发/异常退出留下半截 JSON。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::{BiliError, Result};

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// 数据目录：优先 `QAQ_BX_DATA_DIR`（非空）；
/// 若运行在 AppX/MSIX 容器内（`WindowsApps` 目录只读），自动切至 `%LOCALAPPDATA%\QAQ-Bx\data`；
/// 否则回退到 `<cwd>/data`（CLI 与 UI 同源）。
pub fn resolve_data_dir() -> PathBuf {
    if let Some(dir) = std::env::var("QAQ_BX_DATA_DIR")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
    {
        return dir;
    }

    if is_packaged_app()
        && let Some(local) = std::env::var_os("LOCALAPPDATA").filter(|v| !v.is_empty())
    {
        return PathBuf::from(local).join("QAQ-Bx").join("data");
    }

    std::env::current_dir().unwrap_or_default().join("data")
}

/// 判断当前进程是否运行在 MSIX / AppX 打包身份下。
#[cfg(windows)]
pub fn is_packaged_app() -> bool {
    const APPMODEL_ERROR_NO_PACKAGE: i32 = 15700;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentPackageFullName(
            package_full_name_length: *mut u32,
            package_full_name: *mut u16,
        ) -> i32;
    }
    let mut len: u32 = 0;
    let rc = unsafe { GetCurrentPackageFullName(&mut len, std::ptr::null_mut()) };
    rc != APPMODEL_ERROR_NO_PACKAGE
}

#[cfg(not(windows))]
pub fn is_packaged_app() -> bool {
    false
}

/// 读取可选 JSON 文件（文件不存在返回 `Ok(None)`，解析失败返回可读错误）。
pub(crate) fn load_json_opt<T: DeserializeOwned>(path: &Path, label: &str) -> Result<Option<T>> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let parsed = serde_json::from_slice(&bytes)
                .map_err(|e| BiliError::InvalidData(format!("{label}损坏: {e}")))?;
            Ok(Some(parsed))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// 原子写入 JSON：先写同目录临时文件再 rename 覆盖目标（出错清理临时文件）。
pub(crate) fn atomic_write_json<T: Serialize>(path: &Path, value: &T, label: &str) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|e| BiliError::InvalidData(format!("{label}序列化失败: {e}")))?;

    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("data.json");
    let tmp_name = format!(".{file_name}.tmp.{}.{seq}", std::process::id());
    let tmp_path = path.with_file_name(tmp_name);

    if let Err(err) = std::fs::write(&tmp_path, &bytes) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(err.into());
    }
    if let Err(err) = std::fs::rename(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(err.into());
    }
    Ok(())
}

/// 删除文件（不存在时视为成功）。
pub(crate) fn remove_file_if_exists(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_and_load_roundtrip() {
        let dir = std::env::temp_dir().join(format!(
            "qaq_bx_core_storage_test_{}_{}",
            std::process::id(),
            TMP_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let path = dir.join("sample.json");

        #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
        struct Sample {
            a: u32,
            b: String,
        }

        assert_eq!(load_json_opt::<Sample>(&path, "测试").unwrap(), None);

        let v1 = Sample {
            a: 1,
            b: "first".into(),
        };
        atomic_write_json(&path, &v1, "测试").unwrap();
        assert_eq!(load_json_opt::<Sample>(&path, "测试").unwrap(), Some(v1));

        let v2 = Sample {
            a: 2,
            b: "second".into(),
        };
        atomic_write_json(&path, &v2, "测试").unwrap();
        assert_eq!(load_json_opt::<Sample>(&path, "测试").unwrap(), Some(v2));

        remove_file_if_exists(&path).unwrap();
        assert_eq!(load_json_opt::<Sample>(&path, "测试").unwrap(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
