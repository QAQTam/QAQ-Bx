//! UI 轨迹日志(`data/ui-trace.log`):给 WER 崩溃转储配对的现场线索。
//!
//! 原生崩溃(SE/AV)不走 Rust panic hook,只能靠 dump;本模块在页面切换、
//! 引擎挂载等关键节点追加时间戳,转储时间戳即可与 UI 状态对齐。全部 best-effort:
//! 任何 I/O 失败静默忽略,绝不影响 UI。文件上限 512KiB,超出后截断重写。

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_LOG_BYTES: u64 = 512 * 1024;

fn log_path() -> PathBuf {
    qaq_bx_core::resolve_data_dir().join("ui-trace.log")
}

/// 进程内单调事件序号,用于把相邻事件排序(时间戳精度不足时兜底)。
static SEQ: AtomicU64 = AtomicU64::new(0);

pub fn note(message: impl AsRef<str>) {
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let line = format!(
        "{millis:>16} #{seq:04} {msg}\n",
        msg = message.as_ref()
    );
    let path = log_path();
    let Ok(mut file) = std::fs::OpenOptions::new().append(true).create(true).open(&path) else {
        return;
    };
    // 超限截断:重开文件重写头部标记(先关闭再截断,避免追加越过截断点)。
    if file.metadata().map(|m| m.len()).unwrap_or(0) > MAX_LOG_BYTES {
        drop(file);
        let marker = format!("{millis:>16} #0000 === ui-trace 截断重写(超过 {MAX_LOG_BYTES} 字节) ===\n");
        if std::fs::write(&path, marker).is_ok() {
            if let Ok(mut f) = std::fs::OpenOptions::new().append(true).create(true).open(&path) {
                let _ = f.write_all(line.as_bytes());
            }
        }
        return;
    }
    let _ = file.write_all(line.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_is_best_effort_and_monotonic() {
        // 不 panic 即可;两次调用序号严格递增。
        note("selftest 单测事件 a");
        let a = SEQ.load(Ordering::Relaxed);
        note("selftest 单测事件 b");
        assert!(SEQ.load(Ordering::Relaxed) > a);
    }
}
