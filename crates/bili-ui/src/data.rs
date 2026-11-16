//! 占位数据：真实 feed 接入前的一手样本（结构对齐后续 bili-core feed 模型）。

use std::sync::OnceLock;

use windows_reactor::EncodedImage;

#[derive(Clone, Debug, PartialEq)]
pub struct VideoItem {
    pub bvid: String,
    /// B 站稿件 aid（FeedCard.id），用于直出弹幕等请求，避免必经 view。
    pub aid: u64,
    /// 默认分 P cid（FeedCard 已自带时，点击可直接单跳 playurl）。
    pub cid: u64,
    pub title: String,
    pub author: String,
    pub duration_secs: u32,
    pub play_count: u64,
    pub danmaku_count: u64,
    /// 封面位图（异步加载后填充；占位数据为 None）。
    pub cover: Option<EncodedImage>,
    /// 封面 URL（下载源；占位数据为 None）。
    pub pic_url: Option<String>,
}

fn seed() -> Vec<VideoItem> {
    vec![
        video("BV1xx411c7mD", "【4K】Windows 媒体播放器 Fluent 设计解析：Mica 与传输控制", "设计考古组", 1284, 3_214_500, 45_800),
        video("BV1GJ411x7h7", "纯 Rust 写桌面客户端是什么体验？", "Rust 咸鱼", 962, 1_208_300, 12_400),
        video("BV1hb4y1R7aa", "mpv render API 深入：D3D11 interop 从零开始", "渲染细节控", 2145, 486_200, 3_950),
        video("BV1qK411L7YW", "8GB 内存老机器自救指南：给 2012 年的 Xeon 榨干最后一滴性能", "复古硬件党", 1780, 892_700, 9_210),
        video("BV1xs411N7tX", "WinUI 3 组件全景：NavigationView 的正确打开方式", "微软爱好者", 1105, 654_100, 5_670),
        video("BV1bZ4y1S7qR", "B 站弹幕协议逆向：protobuf 分段拉取全流程", "协议挖掘机", 1543, 733_400, 8_120),
        video("BV1fK4y1E7UF", "WBI 签名算法拆解：从 nav 接口到 mixin key 重排", "API 考古学", 642, 512_800, 4_330),
        video("BV1vp4y1c7aM", "RX560 还能战几年？2026 年亮机卡生存报告", "矿渣爱好者", 890, 328_900, 2_780),
        video("BV1sW411G7VE", "命令行扫码登录 B 站：qrcode 终端渲染实践", "终端美化家", 534, 245_600, 1_990),
        video("BV1tY411D7GP", "Fluent Design 十年：从 Acrylic 到 Mica 的进化史", "设计考古组", 2312, 1_540_200, 18_700),
        video("BV1mP411D7eQ", "零拷贝视频管线：SwapChain 面板与合成器握手", "渲染细节控", 1421, 176_300, 1_460),
        video("BV1nJ411F7qk", "老 CPU 的甜点解码：H.264 硬解在老显卡上的极限测试", "复古硬件党", 1099, 401_500, 3_520),
    ]
}

fn video(bvid: &str, title: &str, author: &str, duration_secs: u32, play_count: u64, danmaku_count: u64) -> VideoItem {
    VideoItem {
        bvid: bvid.to_string(),
        aid: 0,
        cid: 0,
        title: title.to_string(),
        author: author.to_string(),
        duration_secs,
        play_count,
        danmaku_count,
        cover: None,
        pic_url: None,
    }
}

/// 全量占位视频（进程内单例）。
pub fn all() -> &'static [VideoItem] {
    static ITEMS: OnceLock<Vec<VideoItem>> = OnceLock::new();
    ITEMS.get_or_init(seed)
}

/// 按 bvid 查找（骨架期卡片回调传 bvid）。
pub fn find(bvid: &str) -> Option<VideoItem> {
    all().iter().find(|item| item.bvid == bvid).cloned()
}

/// 热门页：按播放量取前 8。
pub fn popular() -> Vec<VideoItem> {
    let mut items = all().to_vec();
    items.sort_by(|a, b| b.play_count.cmp(&a.play_count));
    items.truncate(8);
    items
}
