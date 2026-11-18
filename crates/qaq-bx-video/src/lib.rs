//! qaq-bx-video:视频播放通道(libmpv FFI + 渲染管线)。
//!
//! 蓝本 docs/video-channel.md;FFI 见 `ffi.rs`(动态加载,无硬链接);
//! 封装层 `player.rs`(实例/分轨 loadfile/事件线程/命令);渲染上下文
//! 支持 SW(先行)与 D3D11(下一步)。

pub mod engine;
pub mod error;
pub mod ffi;
pub mod native;
pub mod player;

pub use error::BiliVideoError;
