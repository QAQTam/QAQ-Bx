//! 视频通道错误。

#[derive(Debug, thiserror::Error)]
pub enum BiliVideoError {
    /// libmpv-2.dll 缺失或版本过旧(含 GetLastError/缺失导出细节)。
    #[error("{0}")]
    LibmpvMissing(String),
    /// libmpv API 返回负错误码(mpv_error 字符串化由调用方处理)。
    #[error("libmpv 错误 {0}")]
    MpvApi(i32),
    /// Rust 字符串含 NUL(CString 构造失败)。
    #[error("字符串含 NUL: {0}")]
    Nul(#[from] std::ffi::NulError),
    /// 锁中毒。
    #[error("锁中毒")]
    Poisoned,
}
