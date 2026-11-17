//! 展示模型：UI 侧视频卡片数据。内容由 bili-core 卡片族（FeedCard / ArchiveCard /
//! SearchVideo）在 shell.rs 的转换函数填充，不再持有任何占位样本数据。

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
    /// UP 主 mid（用于标识评论区 UP 主及后续空间跳转）。
    pub author_mid: u64,
    /// UP 主头像 URL（点击进半屏预览/播放页后再异步下载）。
    pub author_face_url: Option<String>,
    pub duration_secs: u32,
    pub play_count: u64,
    pub danmaku_count: u64,
    /// 点赞/投币/收藏/分享真实计数（卡片未下发且 meta 未回填前为 `None`，UI 省略数字）。
    pub like_count: Option<u64>,
    pub coin_count: Option<u64>,
    pub favorite_count: Option<u64>,
    pub share_count: Option<u64>,
    /// 封面位图（异步加载后填充；未就绪为 None）。
    pub cover: Option<EncodedImage>,
    /// 封面 URL（下载源；无图卡为 None）。
    pub pic_url: Option<String>,
}
