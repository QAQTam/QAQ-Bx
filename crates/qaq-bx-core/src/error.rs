//! 共享错误类型。所有插件对外只返回这一种错误。

use thiserror::Error;

#[derive(Debug, Error)]
pub enum BiliError {
    #[error("尚未登录")]
    NotLoggedIn,

    #[error("凭据已过期，需要重新登录")]
    Expired,

    #[error("网络错误: {0}")]
    Network(#[from] reqwest::Error),

    #[error("API 错误 {code}: {message}")]
    Api { code: i64, message: String },

    #[error("数据解析失败: {0}")]
    InvalidData(String),

    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, BiliError>;
