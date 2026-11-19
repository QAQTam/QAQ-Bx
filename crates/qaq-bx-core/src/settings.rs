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

/// 弹幕默认字体族（缺省与 UI 主字体一致；F3）。
pub const DEFAULT_DANMAKU_FONT: &str = "Noto Sans SC";

/// 弹幕可选字体（设置页顺序；DWrite 系统字体名）。
pub const DANMAKU_FONTS: [(&str, &str); 4] = [
    (DEFAULT_DANMAKU_FONT, "默认（Noto Sans SC）"),
    ("Microsoft YaHei", "微软雅黑"),
    ("SimHei", "黑体"),
    ("SimSun", "宋体"),
];

fn default_danmaku_font() -> String {
    DEFAULT_DANMAKU_FONT.to_string()
}

fn default_danmaku_font_scale() -> f32 {
    1.0
}

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

/// 播放引擎(tier1 默认 **原生 Windows Media**;mpv 降级为兼容性回退通道)。
///
/// `QAQ_BX_NATIVE` 只作**强制覆盖**(自动化 / 开发调试),不参与持久化:
/// `"1"` → 强制原生,`"0"` → 强制 mpv,其余取值按本设置项。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlayerEngine {
    /// 系统媒体引擎(MediaFoundation MediaPlayerElement + 弹幕透明层;弹幕满帧率)。
    #[default]
    Native,
    /// libmpv 自绘(D3D11 同帧叠弹幕;直播唯一通道 + native 解码失败时的回退)。
    Mpv,
}

impl PlayerEngine {
    /// UI 列表固定顺序(与 `index()` / `from_index()` 一一对应)。
    pub const ALL: [Self; 2] = [Self::Native, Self::Mpv];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Native => "1. 原生 Windows Media（默认）",
            Self::Mpv => "2. mpv（兼容性优先）",
        }
    }

    pub const fn index(self) -> usize {
        match self {
            Self::Native => 0,
            Self::Mpv => 1,
        }
    }

    pub fn from_index(index: usize) -> Option<Self> {
        Self::ALL.get(index).copied()
    }

    /// `QAQ_BX_NATIVE` 覆盖值 → 强制引擎;`None` = 不设覆盖(按设置项)。
    pub fn env_override(value: Option<&str>) -> Option<Self> {
        match value {
            Some("1") => Some(Self::Native),
            Some("0") => Some(Self::Mpv),
            _ => None,
        }
    }

    /// 生效引擎:环境变量强制覆盖 > 设置项。
    pub fn resolve(forced: Option<Self>, saved: Self) -> Self {
        forced.unwrap_or(saved)
    }

    pub const fn tag(self) -> &'static str {
        match self {
            Self::Mpv => "mpv",
            Self::Native => "native",
        }
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
    /// 弹幕字体族(F3;`#[serde(default)]` 旧配置缺失时按常量补齐)。
    #[serde(default = "default_danmaku_font")]
    pub danmaku_font_family: String,
    /// 弹幕字号缩放(0.5..=2.0;F3)。
    #[serde(default = "default_danmaku_font_scale")]
    pub danmaku_font_scale: f32,
    /// 屏蔽滚动弹幕(网页版弹幕设置对齐;容器 serde default 为旧配置补齐 false)。
    pub danmaku_block_scroll: bool,
    /// 屏蔽底部弹幕。
    pub danmaku_block_bottom: bool,
    /// 屏蔽顶部弹幕。
    pub danmaku_block_top: bool,
    /// 屏蔽彩色弹幕(非 0xFFFFFF)。
    pub danmaku_block_color: bool,
    /// 过滤重复弹幕(同文本 4 秒窗口内仅保留首条)。
    pub danmaku_block_repeat: bool,
    /// 智能屏蔽权重阈值(0=关,1..=10;仅下发 weight>0 且低于阈值者被屏蔽)。
    pub danmaku_weight_min: u32,
    /// 屏蔽词规则(每行一条;`re:` 前缀=正则,`u:` 前缀=用户 midHash,其余=关键词)。
    pub danmaku_filter_rules: String,
    pub autoplay: bool,
    pub hardware_decode: bool,
    pub volume: u32,
    /// 播放引擎(M7;旧配置缺失 → `PlayerEngine::default()` = mpv)。
    pub player_engine: PlayerEngine,
    /// WebP 解码器缺失提示是否已展示过(仅弹一次)。
    pub webp_hint_shown: bool,
    /// 竖屏模式(实验):窗口客户区钳制在底栏边界带 [390, 860) DIP,没收系统
    /// 最大化钮并改供「贴边最大化」(右缘贴边 × 工作区全高)。移动布局自动接管。
    pub portrait_mode: bool,
    /// 优先选择官方商业 CDN（如 `upos-*`），将 `mcdn`/PCDN 节点后置兜底。
    #[serde(default = "default_true")]
    pub prefer_official_cdn: bool,
    /// 开启多线程分块预加载与自动节点容灾（本地有界滑窗代理）。
    #[serde(default = "default_true")]
    pub multi_thread_preload: bool,
    pub window: Option<WindowBounds>,
}

fn default_true() -> bool {
    true
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
            danmaku_font_family: DEFAULT_DANMAKU_FONT.to_string(),
            danmaku_font_scale: 1.0,
            danmaku_block_scroll: false,
            danmaku_block_bottom: false,
            danmaku_block_top: false,
            danmaku_block_color: false,
            danmaku_block_repeat: false,
            danmaku_weight_min: 0,
            danmaku_filter_rules: String::new(),
            autoplay: true,
            hardware_decode: true,
            volume: 80,
            player_engine: PlayerEngine::default(),
            webp_hint_shown: false,
            portrait_mode: false,
            prefer_official_cdn: true,
            multi_thread_preload: true,
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
        if !self.danmaku_font_family.trim().is_empty() {
            self.danmaku_font_family = self.danmaku_font_family.trim().to_string();
        } else {
            self.danmaku_font_family = DEFAULT_DANMAKU_FONT.to_string();
        }
        self.danmaku_font_scale = if self.danmaku_font_scale.is_finite() {
            self.danmaku_font_scale.clamp(0.5, 2.0)
        } else {
            1.0
        };
        self.danmaku_weight_min = self.danmaku_weight_min.clamp(0, 10);
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
                "qaq_bx_core_settings_test_{}_{}",
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
            danmaku_font_family: "Microsoft YaHei".into(),
            danmaku_font_scale: 1.5,
            danmaku_block_scroll: true,
            danmaku_block_bottom: false,
            danmaku_block_top: true,
            danmaku_block_color: false,
            danmaku_block_repeat: true,
            danmaku_weight_min: 5,
            danmaku_filter_rules: "前方高能\nre:^\\d+$\nu:abcd1234".into(),
            autoplay: false,
            hardware_decode: false,
            volume: 45,
            player_engine: PlayerEngine::Native,
            webp_hint_shown: true,
            portrait_mode: true,
            prefer_official_cdn: false,
            multi_thread_preload: false,
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

/// F3:旧配置缺省弹幕字体字段 → 默认族/1.0;非法值归一化钳位。
#[test]
fn danmaku_font_defaults_for_legacy_configs_and_normalizes_extremes() {
    let legacy: AppSettings = serde_json::from_str("{}").unwrap();
    assert_eq!(legacy.danmaku_font_family, DEFAULT_DANMAKU_FONT);
    assert_eq!(legacy.danmaku_font_scale, 1.0);

    let weird: AppSettings = serde_json::from_str::<AppSettings>(
        r#"{"danmaku_font_family": "  ", "danmaku_font_scale": 9.0}"#,
    )
    .unwrap()
    .normalized();
    assert_eq!(weird.danmaku_font_family, DEFAULT_DANMAKU_FONT);
    assert_eq!(weird.danmaku_font_scale, 2.0);
}

/// 弹幕屏蔽字段:旧配置缺省 → 全关/空规则;权重阈值越界钳位 0..=10。
#[test]
fn danmaku_filter_defaults_for_legacy_configs_and_weight_clamped() {
    let legacy: AppSettings = serde_json::from_str("{}").unwrap();
    assert!(!legacy.danmaku_block_scroll);
    assert!(!legacy.danmaku_block_bottom);
    assert!(!legacy.danmaku_block_top);
    assert!(!legacy.danmaku_block_color);
    assert!(!legacy.danmaku_block_repeat);
    assert_eq!(legacy.danmaku_weight_min, 0);
    assert_eq!(legacy.danmaku_filter_rules, "");

    let weird: AppSettings = serde_json::from_str::<AppSettings>(
        r#"{"danmaku_weight_min": 99, "danmaku_filter_rules": "前方高能"}"#,
    )
    .unwrap()
    .normalized();
    assert_eq!(weird.danmaku_weight_min, 10);
    assert_eq!(weird.danmaku_filter_rules, "前方高能");
}

/// S4:播放引擎设置项 —— 旧配置缺字段按 tier1 默认回落 **native**(弹幕满帧率
/// 通道);反序列化失败不得整份配置回退默认;normalized 不改枚举。
#[test]
fn player_engine_defaults_to_native_for_legacy_configs() {
    let legacy: AppSettings = serde_json::from_str("{}").unwrap();
    assert_eq!(legacy.player_engine, PlayerEngine::Native);

    // 只写了一半的老配置:缺 player_engine → native,其余字段照常读出。
    let partial: AppSettings =
        serde_json::from_str(r#"{"volume": 42, "autoplay": false}"#).unwrap();
    assert_eq!(partial.player_engine, PlayerEngine::Native);
    assert_eq!(partial.volume, 42);
    assert!(!partial.autoplay);

    // 原生引擎可写可读,且 normalized 不改它。
    let native: AppSettings = serde_json::from_str(r#"{"player_engine": "Native"}"#).unwrap();
    assert_eq!(native.player_engine, PlayerEngine::Native);
    assert_eq!(native.normalized().player_engine, PlayerEngine::Native);
}

/// M7:引擎列表顺序 / 标签 / 下标必须一一对应(设置页 ComboBox 下发按 index)。
#[test]
fn player_engine_index_and_label_order_is_stable() {
    assert_eq!(PlayerEngine::ALL[0], PlayerEngine::Native);
    assert_eq!(PlayerEngine::ALL[1], PlayerEngine::Mpv);
    for engine in PlayerEngine::ALL {
        assert_eq!(PlayerEngine::from_index(engine.index()), Some(engine));
        assert!(engine.label().starts_with(&format!("{}.", engine.index() + 1)));
    }
    assert_eq!(PlayerEngine::from_index(2), None);
    assert_eq!(PlayerEngine::Mpv.tag(), "mpv");
    assert_eq!(PlayerEngine::Native.tag(), "native");
}

/// M7:`QAQ_BX_NATIVE` 是强制覆盖而非默认值 —— 1 强制原生 / 0 强制 mpv,
/// 未设与非法值都按设置项(E11/E12/E13 自动化依赖这套口径)。
#[test]
fn player_engine_env_override_pins_both_directions() {
    assert_eq!(PlayerEngine::env_override(Some("1")), Some(PlayerEngine::Native));
    assert_eq!(PlayerEngine::env_override(Some("0")), Some(PlayerEngine::Mpv));
    for junk in [Some("true"), Some(""), Some("2"), None] {
        assert_eq!(PlayerEngine::env_override(junk), None, "{junk:?} 不构成覆盖");
    }

    // 覆盖优先;无覆盖时设置项生效。
    assert_eq!(
        PlayerEngine::resolve(Some(PlayerEngine::Native), PlayerEngine::Mpv),
        PlayerEngine::Native
    );
    assert_eq!(
        PlayerEngine::resolve(Some(PlayerEngine::Mpv), PlayerEngine::Native),
        PlayerEngine::Mpv
    );
    assert_eq!(
        PlayerEngine::resolve(None, PlayerEngine::Native),
        PlayerEngine::Native
    );
    assert_eq!(PlayerEngine::resolve(None, PlayerEngine::Mpv), PlayerEngine::Mpv);
}

/// 竖屏模式(实验):旧配置缺字段必须缺省关;置位后 serde 往返保真。
#[test]
fn portrait_mode_defaults_off_and_roundtrips() {
    let legacy: AppSettings = serde_json::from_str("{}").unwrap();
    assert!(!legacy.portrait_mode, "旧配置无此字段必须缺省关");

    let mut next = AppSettings::default();
    next.portrait_mode = true;
    let parsed: AppSettings =
        serde_json::from_str(&serde_json::to_string(&next).unwrap()).unwrap();
    assert!(parsed.portrait_mode, "serde 往返必须保真");
}

#[test]
fn playback_cdn_and_preload_settings_default_true_and_roundtrip() {
    let legacy: AppSettings = serde_json::from_str("{}").unwrap();
    assert!(legacy.prefer_official_cdn, "旧配置缺字段默认优先官方 CDN");
    assert!(legacy.multi_thread_preload, "旧配置缺字段默认开启多线程预加载");

    let mut next = AppSettings::default();
    next.prefer_official_cdn = false;
    next.multi_thread_preload = false;
    let parsed: AppSettings =
        serde_json::from_str(&serde_json::to_string(&next).unwrap()).unwrap();
    assert!(!parsed.prefer_official_cdn);
    assert!(!parsed.multi_thread_preload);
}
