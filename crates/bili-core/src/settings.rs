//! 应用配置契约与原子持久化（`data/settings.json`）。
//!
//! 设置页面、`AppShell`、解析策略（`PlayerClient::resolve`）与 `VideoEngine`
//! 共享此处的结构化偏好；所有字段支持 `#[serde(default)]` 向后兼容。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::storage::{atomic_write_json, load_json_opt};

/// 设置文件名（存于数据目录下）。
pub const SETTINGS_FILE: &str = "settings.json";

/// 默认大会员与游客的画质档位上限。
pub const MAX_QN_VIP: i64 = 120;
pub const MAX_QN_GUEST: i64 = 80;

/// 首选清晰度档位（映射 B 站 playurl `qn` 与设置页顺序）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum QualityPref {
    Q360,
    Q480,
    Q720,
    #[default]
    Q1080,
    Q1080P60,
    Q4K,
}

impl QualityPref {
    /// UI 列表固定顺序（与 `index()` / `from_index()` 一一对应）。
    pub const ALL: [Self; 6] = [
        Self::Q360,
        Self::Q480,
        Self::Q720,
        Self::Q1080,
        Self::Q1080P60,
        Self::Q4K,
    ];

    pub const fn qn(self) -> i64 {
        match self {
            Self::Q360 => 16,
            Self::Q480 => 32,
            Self::Q720 => 64,
            Self::Q1080 => 80,
            Self::Q1080P60 => 116,
            Self::Q4K => 120,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Q360 => "360P 流畅",
            Self::Q480 => "480P 清晰",
            Self::Q720 => "720P 高清",
            Self::Q1080 => "1080P 高清",
            Self::Q1080P60 => "1080P60 高帧率",
            Self::Q4K => "4K 超清",
        }
    }

    pub const fn index(self) -> usize {
        match self {
            Self::Q360 => 0,
            Self::Q480 => 1,
            Self::Q720 => 2,
            Self::Q1080 => 3,
            Self::Q1080P60 => 4,
            Self::Q4K => 5,
        }
    }

    pub fn from_index(index: usize) -> Option<Self> {
        Self::ALL.get(index).copied()
    }

    /// 结合大会员权限后的实际请求档位上限（用户偏好不超过账号可下发上限）。
    pub fn effective_max_qn(self, is_vip: bool) -> i64 {
        let cap = if is_vip { MAX_QN_VIP } else { MAX_QN_GUEST };
        self.qn().min(cap)
    }
}

/// 视频编码优先级偏好。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CodecPref {
    #[default]
    AvcFirst,
    HevcFirst,
    Av1Experimental,
}

impl CodecPref {
    pub const ALL: [Self; 3] = [Self::AvcFirst, Self::HevcFirst, Self::Av1Experimental];

    pub const fn priority(self) -> &'static [&'static str] {
        match self {
            Self::AvcFirst => &["avc1", "hev1", "av01"],
            Self::HevcFirst => &["hev1", "avc1", "av01"],
            Self::Av1Experimental => &["av01", "hev1", "avc1"],
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::AvcFirst => "H.264",
            Self::HevcFirst => "H.265 / HEVC",
            Self::Av1Experimental => "AV1",
        }
    }

    pub const fn index(self) -> usize {
        match self {
            Self::AvcFirst => 0,
            Self::HevcFirst => 1,
            Self::Av1Experimental => 2,
        }
    }

    pub fn from_index(index: usize) -> Option<Self> {
        Self::ALL.get(index).copied()
    }
}

/// 可选窗口位置记忆（物理像素外框 + 最大化态，与 `WindowPlacement` 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowBounds {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    #[serde(default)]
    pub maximized: bool,
}

/// 首页点击视频卡片时的打开方式。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum HomeOpenMode {
    /// 展开全屏（直接进入完整播放页）。
    #[default]
    FullPlayer,
    /// 展开半屏（右侧半窗抽屉：视频面 + 简介 + 评论，无控条，可随时展开进完整播放页）。
    HalfPane,
}

impl HomeOpenMode {
    pub const ALL: [Self; 2] = [Self::FullPlayer, Self::HalfPane];

    pub const fn label(self) -> &'static str {
        match self {
            Self::FullPlayer => "1. 展开全屏",
            Self::HalfPane => "2. 展开半屏",
        }
    }

    pub const fn index(self) -> usize {
        match self {
            Self::FullPlayer => 0,
            Self::HalfPane => 1,
        }
    }

    pub fn from_index(index: usize) -> Option<Self> {
        Self::ALL.get(index).copied()
    }
}

/// 应用全局设置。所有字段皆可在旧配置文件缺失时按默认值补齐。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub quality: QualityPref,
    pub codec: CodecPref,
    pub home_open_mode: HomeOpenMode,
    pub half_pane_width: f64,
    pub danmaku_enabled: bool,
    pub danmaku_opacity: f32,
    pub danmaku_max_lanes: usize,
    pub autoplay: bool,
    pub hardware_decode: bool,
    pub volume: u32,
    /// WebP 解码器缺失提示是否已展示过(仅弹一次)。
    pub webp_hint_shown: bool,
    pub window: Option<WindowBounds>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            quality: QualityPref::default(),
            codec: CodecPref::default(),
            home_open_mode: HomeOpenMode::default(),
            half_pane_width: 480.0,
            danmaku_enabled: true,
            danmaku_opacity: 0.92,
            danmaku_max_lanes: 10,
            autoplay: true,
            hardware_decode: true,
            volume: 80,
            webp_hint_shown: false,
            window: None,
        }
    }
}

impl AppSettings {
    /// 钳位各数值字段到安全范围（兼容手工编辑或损坏字段）。
    pub fn normalized(mut self) -> Self {
        self.danmaku_opacity = if self.danmaku_opacity.is_finite() {
            self.danmaku_opacity.clamp(0.1, 1.0)
        } else {
            0.92
        };
        self.danmaku_max_lanes = self.danmaku_max_lanes.clamp(1, 20);
        self.volume = self.volume.min(100);
        self.half_pane_width = if self.half_pane_width.is_finite() {
            self.half_pane_width.clamp(340.0, 820.0)
        } else {
            480.0
        };
        self
    }
}

/// 设置文件持久化存储（`settings.json`）。
pub struct FileSettingsStore {
    path: PathBuf,
}

impl FileSettingsStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 读取并归一化配置；文件不存在时返回默认配置。
    pub fn load(&self) -> Result<AppSettings> {
        Ok(load_json_opt::<AppSettings>(&self.path, "设置文件")?
            .unwrap_or_default()
            .normalized())
    }

    /// 启动安全读取：文件损坏或 IO 异常时降级返回默认配置及可读告警，不阻塞启动。
    pub fn load_or_default(&self) -> (AppSettings, Option<String>) {
        match self.load() {
            Ok(settings) => (settings, None),
            Err(err) => (
                AppSettings::default(),
                Some(format!("读取设置失败，已回退默认：{err}")),
            ),
        }
    }

    /// 归一化后原子写入磁盘。
    pub fn save(&self, settings: &AppSettings) -> Result<()> {
        let clean = settings.clone().normalized();
        atomic_write_json(&self.path, &clean, "设置")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_settings_path() -> PathBuf {
        std::env::temp_dir()
            .join(format!(
                "bili_core_settings_test_{}_{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed)
            ))
            .join(SETTINGS_FILE)
    }

    #[test]
    fn default_settings_roundtrip_and_missing_file_defaults() {
        let path = temp_settings_path();
        let store = FileSettingsStore::new(&path);

        let loaded = store.load().unwrap();
        assert_eq!(loaded, AppSettings::default());

        let custom = AppSettings {
            quality: QualityPref::Q1080P60,
            codec: CodecPref::HevcFirst,
            home_open_mode: HomeOpenMode::HalfPane,
            half_pane_width: 520.0,
            danmaku_enabled: false,
            danmaku_opacity: 0.75,
            danmaku_max_lanes: 8,
            autoplay: false,
            hardware_decode: false,
            volume: 45,
            webp_hint_shown: true,
            window: Some(WindowBounds {
                x: 100,
                y: 80,
                width: 1280,
                height: 800,
                maximized: false,
            }),
        };
        store.save(&custom).unwrap();
        let reloaded = store.load().unwrap();
        assert_eq!(reloaded, custom);

        if let Some(dir) = path.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn partial_json_and_out_of_range_values_are_normalized() {
        let path = temp_settings_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"volume": 250, "danmaku_opacity": -5.0, "danmaku_max_lanes": 99}"#,
        )
        .unwrap();

        let store = FileSettingsStore::new(&path);
        let loaded = store.load().unwrap();
        assert_eq!(loaded.volume, 100);
        assert_eq!(loaded.danmaku_opacity, 0.1);
        assert_eq!(loaded.danmaku_max_lanes, 20);
        assert_eq!(loaded.quality, QualityPref::Q1080);
        assert!(loaded.autoplay);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn corrupt_json_falls_back_gracefully() {
        let path = temp_settings_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{broken json").unwrap();

        let store = FileSettingsStore::new(&path);
        let (loaded, warning) = store.load_or_default();
        assert_eq!(loaded, AppSettings::default());
        assert!(warning.is_some());

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn effective_max_qn_respects_vip_and_user_cap() {
        assert_eq!(QualityPref::Q480.effective_max_qn(true), 32);
        assert_eq!(QualityPref::Q480.effective_max_qn(false), 32);
        assert_eq!(QualityPref::Q1080P60.effective_max_qn(false), 80);
        assert_eq!(QualityPref::Q1080P60.effective_max_qn(true), 116);
        assert_eq!(QualityPref::Q4K.effective_max_qn(true), 120);
        assert_eq!(QualityPref::Q4K.effective_max_qn(false), 80);
    }
}
