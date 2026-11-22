//! shell 纯函数与纯类型:无状态、无 self、无 context 的值级变换。
//!
//! 归属约定:
//! - URL/卡片映射(thumbnail_url 系、video_item 系)—— 数据桥接纯函数;
//! - 布局签名(resize_render_signature/title_bar_tier_signature/portrait 约束)—— 窗口域判定;
//! - 设置映射(resolve_params/video_config_from/danmaku_filter_from)—— AppSettings → 引擎参数;
//! - 弹幕分段数学(danmaku_segment_*)—— 纯算术,单测锚点 6 分钟边界;
//! - engine_switch_plan / search_enter_submits —— 引擎切换与搜索回车判定纯决策。
//! 单元测试位于 shell/tests.rs(经 `use super::*` 可见,调用点零改动)。

use super::*;

/// §8.2:TitleBar 内容结构档位签名——只有会改变标题栏内容结构的档位
/// (Mobile 砍副标题/盘钮、Medium 搜索浮窗钮、Expanded 内敛搜索框)才翻签名;
/// 档内宽度变化不翻,补算不跟每次 resize 走。
pub(super) fn title_bar_tier_signature(layout: ViewportLayout) -> u32 {
    u32::from(layout.is_mobile())
        | (u32::from(layout.shows_inline_title_search()) << 1)
        | (u32::from(layout.shows_title_search_button()) << 2)
}

/// #5:resize 时必须翻动根 view 的结构签名——tier、列数、卡态与派生档位
/// (移动端形态/内联搜索/私信钻取)。签名不翻 = 档内纯像素变化,交给 XAML
/// 自动布局与静默状态累计,静止 150ms 后由 settled 普通消息统一重绘。
pub(super) fn resize_render_signature(layout: ViewportLayout) -> (u8, usize, bool, bool, bool, bool) {
    let tier = match layout.tier {
        layout::ContentLayoutTier::Compact => 0u8,
        layout::ContentLayoutTier::Medium => 1,
        layout::ContentLayoutTier::Expanded => 2,
    };
    (
        tier,
        layout.cards.cols,
        layout.cards.compact_card,
        layout.is_mobile(),
        layout.shows_inline_title_search(),
        layout.uses_whisper_drilldown(),
    )
}

/// 竖屏模式窗口约束:客户区宽度钳制在底栏边界带 [390, 860) DIP,高度不设限
/// (presenter 级硬钳制,reactor `WindowConstraints` → `SetPreferredMaximumWidth`)。
pub(super) fn portrait_window_constraints() -> WindowConstraints {
    WindowConstraints {
        min_width: Some(layout::MIN_WINDOW_WIDTH),
        min_height: Some(layout::MIN_WINDOW_HEIGHT),
        max_width: Some(PORTRAIT_MAX_WIDTH_DIPS),
        max_height: None,
    }
}
/// 升级 https 并对 B 站图床追加 `@480w_270h_1c.{webp|jpg}` 缩略图后缀,
/// 把单张封面流量与 WinUI 解码像素面积压缩到原图的 ~1/16–1/46。
/// 格式由系统 WebP 解码能力探测决定(见 `codec` 模块)。
pub fn thumbnail_url(raw: &str) -> String {
    thumbnail_url_with(raw, crate::codec::raster_suffix())
}

pub(super) fn thumbnail_url_with(raw: &str, suffix: &str) -> String {
    let url = raw.replacen("http://", "https://", 1);
    if url.contains(".hdslb.com/") && !url.contains('@') && !url.contains('?') {
        let lower = url.to_ascii_lowercase();
        if lower.ends_with(".jpg")
            || lower.ends_with(".jpeg")
            || lower.ends_with(".png")
            || lower.ends_with(".webp")
        {
            return format!("{url}@480w_270h_1c.{suffix}");
        }
    }
    url
}

/// FeedCard → 展示模型（`goto != av` 已由 `FeedSession::absorb` 过滤）。
pub(super) fn video_item(card: &FeedCard) -> VideoItem {
    VideoItem {
        bvid: card.bvid.clone().unwrap_or_default(),
        aid: card.id,
        cid: card.cid.unwrap_or(0),
        title: card.title.clone().unwrap_or_default(),
        author: card
            .owner
            .as_ref()
            .map_or_else(String::new, |owner| owner.name.clone()),
        author_mid: card.owner.as_ref().map_or(0, |owner| owner.mid),
        author_face_url: card
            .owner
            .as_ref()
            .and_then(|owner| owner.face.as_deref())
            .and_then(normalize_face_url),
        duration_secs: card
            .duration
            .unwrap_or_default()
            .clamp(0, i64::from(u32::MAX)) as u32,
        play_count: card
            .stat
            .as_ref()
            .map_or(0, |stat| stat.view.unsigned_abs()),
        danmaku_count: card
            .stat
            .as_ref()
            .map_or(0, |stat| stat.danmaku.unsigned_abs()),
        like_count: card
            .stat
            .as_ref()
            .and_then(|stat| stat.like)
            .map(|v| v.unsigned_abs()),
        coin_count: card
            .stat
            .as_ref()
            .and_then(|stat| stat.coin)
            .map(|v| v.unsigned_abs()),
        favorite_count: card
            .stat
            .as_ref()
            .and_then(|stat| stat.favorite)
            .map(|v| v.unsigned_abs()),
        share_count: card
            .stat
            .as_ref()
            .and_then(|stat| stat.share)
            .map(|v| v.unsigned_abs()),
        cover: None,
        pic_url: card.pic.as_deref().map(thumbnail_url),
    }
}

pub(super) fn normalize_face_url(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(if let Some(rest) = trimmed.strip_prefix("//") {
        format!("https://{rest}")
    } else {
        trimmed.replacen("http://", "https://", 1)
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResolveProfileKey {
    pub max_qn: i64,
    pub codec_idx: usize,
}

/// 结合设置与当前大会员缓存推导请求清晰度上限和编码优先级（不发 IO）。
pub(super) fn resolve_params_for(settings: &AppSettings, nav: &NavClient) -> (i64, &'static [&'static str]) {
    let is_vip = nav
        .cached_profile()
        .is_some_and(|profile| profile.vip.is_vip());
    (
        settings.quality.effective_max_qn(is_vip),
        settings.codec.priority(),
    )
}

pub(super) fn resolve_profile_key(settings: &AppSettings, nav: &NavClient) -> ResolveProfileKey {
    let (max_qn, _) = resolve_params_for(settings, nav);
    ResolveProfileKey {
        max_qn,
        codec_idx: settings.codec.index(),
    }
}

pub(super) fn video_config_from(settings: &AppSettings) -> VideoConfig {
    VideoConfig {
        hardware_decode: settings.hardware_decode,
        danmaku_enabled: settings.danmaku_enabled,
        danmaku_opacity: settings.danmaku_opacity,
        danmaku_max_lanes: settings.danmaku_max_lanes,
        danmaku_font_family: settings.danmaku_font_family.clone(),
        danmaku_font_scale: settings.danmaku_font_scale,
        danmaku_filter: danmaku_filter_from(settings),
        // D1:溢出策略默认丢弃(设置项接入随 D4/D5 批次)。
        danmaku_overflow: DanmakuOverflow::Drop,
        danmaku_density: settings.danmaku_density,
        danmaku_area: settings.danmaku_area,
        danmaku_speed_plus: settings.danmaku_speed_plus,
        danmaku_prevent_shade: settings.danmaku_prevent_shade,
    }
}

/// 弹幕样式热更新参数(设置 → 引擎/stage 单一参数源;与 video_config_from
/// 的弹幕字段同源,夹取由引擎线程 DanmakuStyle::normalized 统一执行)。
pub(super) fn danmaku_style_from(settings: &AppSettings) -> DanmakuStyle {
    DanmakuStyle {
        enabled: settings.danmaku_enabled,
        opacity: settings.danmaku_opacity,
        max_lanes: settings.danmaku_max_lanes,
        font_family: settings.danmaku_font_family.clone(),
        font_scale: settings.danmaku_font_scale,
        density: settings.danmaku_density,
        area: settings.danmaku_area,
        speed_plus: settings.danmaku_speed_plus,
        prevent_shade: settings.danmaku_prevent_shade,
    }
}

/// 弹幕过滤规则(设置模型 → 引擎规则;每行一条,空行忽略)。
pub(super) fn danmaku_filter_from(settings: &AppSettings) -> DanmakuFilter {
    DanmakuFilter {
        block_scroll: settings.danmaku_block_scroll,
        block_bottom: settings.danmaku_block_bottom,
        block_top: settings.danmaku_block_top,
        block_color: settings.danmaku_block_color,
        block_repeat: settings.danmaku_block_repeat,
        weight_min: settings.danmaku_weight_min,
        rules: settings
            .danmaku_filter_rules
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect(),
    }
}

/// 弹幕分段数(向上取整,至少 1;duration 缺失按单段兜底)。
pub(super) fn danmaku_segment_count(duration_secs: f64) -> u32 {
    (duration_secs.max(0.0) / DANMAKU_SEGMENT_SECS as f64)
        .ceil()
        .max(1.0) as u32
}

/// 播放头所在分段(index 从 1 起)。
pub(super) fn danmaku_segment_at(secs: f64) -> u32 {
    (secs.max(0.0) / DANMAKU_SEGMENT_SECS as f64).floor() as u32 + 1
}

/// 预存链的下一分段:优先段(播放头所在段)先到,其余按 1..=total 升序补齐。
pub(super) fn next_danmaku_segment(current: u32, first: u32, total: u32) -> Option<u32> {
    if current == first {
        (1..=total).find(|&i| i != first)
    } else {
        (current + 1..=total).find(|&i| i != first)
    }
}

/// 全局搜索框壳的 Enter 判定:PreviewKeyDown 隧道命中主键盘/小键盘 Enter
/// (`original_key` 对 NumpadEnter 上报 0x0D)才提交;释放与其余键放行不拦截。
/// 纯函数便于单测锁行为,视图侧 `on_preview_key_down` 只做「判定→投递→handled」。
pub(crate) fn search_enter_submits(ev: &KeyEventInfo) -> bool {
    !ev.status.is_released && (ev.key == VirtualKey::ENTER || ev.original_key == VirtualKey::ENTER)
}

/// S3:切换动作判定 —— 优先级:直播豁免 > 无会话 > 新引擎方向。
pub(super) fn engine_switch_plan(to_native: bool, live: bool, has_session: bool) -> EngineSwitchAction {
    if live {
        EngineSwitchAction::LiveKeepMpv
    } else if !has_session {
        EngineSwitchAction::RecordOnly
    } else if to_native {
        EngineSwitchAction::ReloadNative
    } else {
        EngineSwitchAction::ReloadMpv
    }
}

/// 搜索结果卡 → 展示模型(封面占位,由 ImageCache/CoverReady 异步填充)。
pub(super) fn search_video_item(video: &SearchVideo) -> VideoItem {
    VideoItem {
        bvid: video.bvid.clone(),
        aid: video.aid,
        cid: video.cid,
        title: video.title.clone(),
        author: video.author.clone(),
        author_mid: video.author_mid,
        author_face_url: None,
        duration_secs: video.duration_secs,
        play_count: video.play_count,
        danmaku_count: video.danmaku_count,
        // 搜索卡不下发互动计数，保持 None（进播放页后由 video_meta 回填）。
        like_count: None,
        coin_count: None,
        favorite_count: None,
        share_count: None,
        cover: None,
        pic_url: video.pic_url.clone(),
    }
}

/// U5:用户列表卡 → open_video 展示模型(封面沿用已解码位图;aid/cid 由
/// video_meta 解析管线回填,卡片不预置)。
pub(super) fn user_card_item(card: &UserVideoCard) -> VideoItem {
    VideoItem {
        bvid: card.bvid.clone(),
        aid: 0,
        cid: 0,
        title: card.title.clone(),
        author: card.up_name.clone(),
        author_mid: 0,
        author_face_url: None,
        duration_secs: card.duration_secs,
        play_count: card.play_count,
        danmaku_count: 0,
        like_count: None,
        coin_count: None,
        favorite_count: None,
        share_count: None,
        cover: card.cover.clone(),
        pic_url: if card.cover_url.is_empty() {
            None
        } else {
            Some(card.cover_url.clone())
        },
    }
}

/// 归档卡（popular/related 真实接口）→ 展示模型(封面占位,由 ImageCache/CoverReady 异步填充)。
pub(super) fn archive_video_item(card: &ArchiveCard) -> VideoItem {
    VideoItem {
        bvid: card.bvid.clone(),
        aid: card.aid,
        cid: card.cid,
        title: card.title.clone(),
        author: card.owner.name.clone(),
        author_mid: card.owner.mid,
        author_face_url: card.owner.face.as_deref().and_then(normalize_face_url),
        duration_secs: card.duration.clamp(0, i64::from(u32::MAX)) as u32,
        play_count: card.stat.view.unsigned_abs(),
        danmaku_count: card.stat.danmaku.unsigned_abs(),
        like_count: card.stat.like.map(|v| v.unsigned_abs()),
        coin_count: card.stat.coin.map(|v| v.unsigned_abs()),
        favorite_count: card.stat.favorite.map(|v| v.unsigned_abs()),
        share_count: card.stat.share.map(|v| v.unsigned_abs()),
        cover: None,
        pic_url: Some(thumbnail_url(&card.pic)),
    }
}

/// 未知元数据稿件的最小展示模型（QAQ_BX_VIDEO 冒烟路由 / open_video 兜底）：
/// 标题暂以 bvid 占位，真实标题/UP 主/封面由 video_meta 解析管线回填。
pub(super) fn minimal_video_item(bvid: &str, title: &str) -> VideoItem {
    VideoItem {
        bvid: bvid.to_string(),
        aid: 0,
        cid: 0,
        title: if title.trim().is_empty() {
            bvid.to_string()
        } else {
            title.to_string()
        },
        author: String::new(),
        author_mid: 0,
        author_face_url: None,
        duration_secs: 0,
        play_count: 0,
        danmaku_count: 0,
        like_count: None,
        coin_count: None,
        favorite_count: None,
        share_count: None,
        cover: None,
        pic_url: None,
    }
}

/// 热门分页合并：按 bvid 去重（跨页重叠防御），返回实际新增项。
pub(super) fn merge_popular_items(existing: &mut Vec<VideoItem>, incoming: Vec<VideoItem>) -> Vec<VideoItem> {
    let mut seen: HashSet<String> = existing.iter().map(|item| item.bvid.clone()).collect();
    let mut fresh = Vec::new();
    for item in incoming {
        if seen.insert(item.bvid.clone()) {
            fresh.push(item);
        }
    }
    existing.extend(fresh.iter().cloned());
    fresh
}

