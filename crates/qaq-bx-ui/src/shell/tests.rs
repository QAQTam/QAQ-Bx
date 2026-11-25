use super::*;

use qaq_bx_core::settings::{CodecPref, QualityPref};

#[test]
fn latest_value_messages_are_coalescable_and_the_rest_are_not() {
    let size = |width, height| windows_reactor::WindowSize { width, height };
    assert_eq!(
        AppShell::coalesce_key(&Message::WindowResized(size(800.0, 600.0))),
        Some(1),
        "拖框风暴:中间尺寸无意义,必须 latest-wins"
    );
    assert_eq!(
        AppShell::coalesce_key(&Message::FullscreenProbeResult { gen_id: 7, active: true }),
        Some(2),
        "探针只看最新一代的当前态"
    );
    // 非「最新值」语义的消息绝不可被合并丢弃:逐条计数/投递的都有副作用。
    assert_eq!(AppShell::coalesce_key(&Message::PositionTick), None);
    assert_eq!(AppShell::coalesce_key(&Message::Back), None);
    assert_eq!(AppShell::coalesce_key(&Message::WindowResizeSettled), None);
    assert_eq!(
        AppShell::coalesce_key(&Message::ShellInvalidateLayout),
        Some(3),
        "#5 无载荷重绘请求可队尾合并"
    );
}

/// #5 验收:「同档位 resize 不触发重渲染」判定——档内纯像素变化签名不翻,
/// 结构档位翻转必须翻签名(否则 silent 尺寸泵会把旧布局留在屏上)。
#[test]
fn same_tier_resize_keeps_render_signature_and_cross_tier_flips_it() {
    // Expanded 档内小幅拖宽:cols/卡态不变 → 签名同。
    let wide = ViewportLayout::for_window(1280.0, 800.0, false);
    let also_wide = ViewportLayout::for_window(1240.0, 800.0, false);
    assert_eq!(wide.cards.cols, also_wide.cards.cols, "样本须同列数");
    assert_eq!(
        resize_render_signature(wide),
        resize_render_signature(also_wide),
        "同档同列 resize 不得触发根 view 重绘"
    );
    // Mobile 带内拖宽:2 列紧凑卡不翻签名。
    let mob = ViewportLayout::for_window(390.0, 760.0, false);
    let mob2 = ViewportLayout::for_window(420.0, 760.0, false);
    assert_eq!(resize_render_signature(mob), resize_render_signature(mob2));
    // 跨档(Expanded → Medium):签名必翻。
    let mid = ViewportLayout::for_window(700.0, 800.0, false);
    assert_ne!(
        resize_render_signature(wide),
        resize_render_signature(mid),
        "tier 翻转必须请求重绘"
    );
    // 同宽但列数不同(如 600..859 带内跨列曲线拐点):签名必翻。
    let mut flipped = wide;
    flipped.cards.cols += 1;
    assert_ne!(
        resize_render_signature(wide),
        resize_render_signature(flipped),
        "cols 翻转必须请求重绘"
    );
}

#[test]
fn resize_storm_tracker_bursts_close_and_summarize() {
    let t0 = Instant::now();
    // 孤立/双发不足 MIN_BURST:收口不输出。
    let mut storm = ResizeStormTrack::default();
    assert!(storm.on_resized(t0).is_none());
    assert!(storm.on_resized(t0 + Duration::from_millis(55)).is_none());
    assert!(storm.close("settled").is_none(), "孤立 resize 不算风暴");
    // 5 连发 55ms 间隔:n=5 / span=220ms / max_gap=55ms。
    let mut storm = ResizeStormTrack::default();
    for i in 0..5 {
        let now = t0 + Duration::from_millis(55 * i);
        assert!(storm.on_resized(now).is_none(), "第 {i} 拍不应触发收口");
    }
    let summary = storm.close("settled").expect("5 连发应输出风暴摘要");
    assert!(summary.contains("RESIZE-STORM n=5"), "{summary}");
    assert!(summary.contains("span_ms=220"), "{summary}");
    assert!(summary.contains("max_gap_ms=55"), "{summary}");
    // 间隔 >250ms:上一场自动收口并开新风暴。
    let mut storm = ResizeStormTrack::default();
    for i in 0..3 {
        let _ = storm.on_resized(t0 + Duration::from_millis(55 * i));
    }
    let closed = storm
        .on_resized(t0 + Duration::from_millis(110 + 400))
        .expect("间隔 400ms 应先收口上一场");
    assert!(closed.contains("RESIZE-STORM n=3"), "{closed}");
    assert!(
        storm.close("settled").is_none(),
        "新风暴仅 1 拍,不足输出门槛"
    );
}

#[test]
fn title_bar_tier_signature_tracks_content_structure_tiers() {
    use crate::layout::{COMPACT_CONTENT_BREAKPOINT, EXPANDED_BREAKPOINT, MOBILE_BREAKPOINT};
    // chrome 阈值(860)高于 content 阈值(600):Mobile chrome 段内仍有
    // Compact/Medium 两个内容档,三档签名 = 1 / 5 / 2。
    let mobile = ViewportLayout::from_window_size(COMPACT_CONTENT_BREAKPOINT - 100.0, 800.0);
    let medium = ViewportLayout::from_window_size(MOBILE_BREAKPOINT.min(800.0) - 60.0, 800.0);
    let expanded = ViewportLayout::from_window_size(EXPANDED_BREAKPOINT + 20.0, 800.0);
    let signatures = (
        title_bar_tier_signature(mobile),
        title_bar_tier_signature(medium),
        title_bar_tier_signature(expanded),
    );
    assert_ne!(signatures.0, signatures.1, "Mobile 档应翻转签名");
    assert_ne!(signatures.1, signatures.2, "Medium→Expanded 应翻转签名");
    assert_ne!(signatures.0, signatures.2, "Mobile→Expanded 应翻转签名");
    // 同档不同宽(Expanded 档内加宽)不翻签名:补算不跟每次 resize 走。
    let wide_a = ViewportLayout::from_window_size(EXPANDED_BREAKPOINT + 100.0, 800.0);
    let wide_b = ViewportLayout::from_window_size(EXPANDED_BREAKPOINT + 260.0, 800.0);
    assert_eq!(
        title_bar_tier_signature(wide_a),
        title_bar_tier_signature(wide_b),
        "档内宽度变化不得触发补算"
    );
}

#[test]
fn portrait_window_constraints_stay_inside_bottom_bar_band() {
    let constraints = portrait_window_constraints();
    assert_eq!(
        constraints.min_width,
        Some(layout::MIN_WINDOW_WIDTH),
        "下限沿用既有最小窗宽"
    );
    assert_eq!(constraints.max_width, Some(PORTRAIT_MAX_WIDTH_DIPS));
    assert!(
        constraints.max_width.unwrap() < crate::layout::MOBILE_BREAKPOINT,
        "上限必须严格保持在底栏边界之内(任意 DPI 取整后仍不出带)"
    );
    assert!(constraints.max_height.is_none(), "高度不设限");
}

#[test]
fn danmaku_segment_math_anchors_six_minute_boundaries() {
    assert_eq!(danmaku_segment_count(0.0), 1);
    assert_eq!(danmaku_segment_count(359.9), 1);
    assert_eq!(danmaku_segment_count(360.0), 1);
    assert_eq!(danmaku_segment_count(360.1), 2);
    assert_eq!(danmaku_segment_count(1234.0), 4);
    // 负值/缺失兜底单段。
    assert_eq!(danmaku_segment_count(-5.0), 1);
    assert_eq!(danmaku_segment_at(0.0), 1);
    assert_eq!(danmaku_segment_at(359.9), 1);
    assert_eq!(danmaku_segment_at(360.0), 2);
    assert_eq!(danmaku_segment_at(800.0), 3);
}

#[test]
fn danmaku_preload_order_playhead_first_then_ascending() {
    // 播放头在第 1 段:1 → 2 → 3 → 4 → 收束。
    let chain: Vec<u32> = std::iter::successors(Some(1u32), |&c| next_danmaku_segment(c, 1, 4))
        .take(10)
        .collect();
    assert_eq!(chain, vec![1, 2, 3, 4]);
    // 播放头在第 3 段:3 → 1 → 2 → 4 → 收束(优先段只到一次)。
    let chain: Vec<u32> = std::iter::successors(Some(3u32), |&c| next_danmaku_segment(c, 3, 4))
        .take(10)
        .collect();
    assert_eq!(chain, vec![3, 1, 2, 4]);
    // 优先段越界(总段数缩水):先试 9(该段为空/失败),再 1 → 2 收束。
    let chain: Vec<u32> = std::iter::successors(Some(9u32), |&c| next_danmaku_segment(c, 9, 2))
        .take(10)
        .collect();
    assert_eq!(chain, vec![9, 1, 2]);
    // 单段视频:首段即收束。
    let chain: Vec<u32> = std::iter::successors(Some(1u32), |&c| next_danmaku_segment(c, 1, 1))
        .take(10)
        .collect();
    assert_eq!(chain, vec![1]);
}

/// S3:引擎切换判定 —— 直播豁免 > 无会话只记账 > 双向定点重载。
#[test]
fn engine_switch_plan_pins_live_and_reloads_both_directions() {
    use EngineSwitchAction::*;
    // 直播:即使设成 native 也不接管,只做 native 侧下线。
    assert_eq!(engine_switch_plan(true, true, true), LiveKeepMpv);
    assert_eq!(engine_switch_plan(false, true, true), LiveKeepMpv);
    // 无会话(未开播/解析在途):只记账,下一次装载生效。
    assert_eq!(engine_switch_plan(true, false, false), RecordOnly);
    assert_eq!(engine_switch_plan(false, false, false), RecordOnly);
    // 正在播的 VOD:两个方向都走定点重载(常驻播放层,当次生效)。
    assert_eq!(engine_switch_plan(true, false, true), ReloadNative);
    assert_eq!(engine_switch_plan(false, false, true), ReloadMpv);
}
/// M7:直播判定只认 `live:` 前缀(真实稿件 bvid 恒以 BV 开头,不得误判)。
#[test]
fn live_prefix_matches_live_room_items_only() {
    assert!(minimal_video_item(&format!("{LIVE_BVID_PREFIX}123"), "直播").bvid
        .starts_with(LIVE_BVID_PREFIX));
    assert!(!minimal_video_item("BV1GJ411x7h7", "稿件").bvid.starts_with(LIVE_BVID_PREFIX));
}

#[test]
fn resolve_params_caps_guest_at_80_and_honors_codec_pref() {
    let nav = NavClient::new_anonymous(reqwest::Client::new());
    let mut settings = AppSettings::default();
    settings.quality = QualityPref::Q4K;
    settings.codec = CodecPref::Av1Experimental;

    let (max_qn, codecs) = resolve_params_for(&settings, &nav);
    assert_eq!(max_qn, 80);
    assert_eq!(codecs, &["av01", "hev1", "avc1"]);

    settings.quality = QualityPref::Q720;
    settings.codec = CodecPref::HevcFirst;
    let (max_qn, codecs) = resolve_params_for(&settings, &nav);
    assert_eq!(max_qn, 64);
    assert_eq!(codecs, &["hev1", "avc1", "av01"]);
}

/// F4:首播音画缺失探测判定(媒体就绪 + 3s 宽限 + 轨道格式为空)。
#[test]
fn missing_media_track_requires_media_grace_and_empty_format() {
    use MissingTrack::*;
    // 媒体未就绪不探测。
    assert_eq!(
        missing_media_track("", "", false, Some(Duration::from_secs(10))),
        None
    );
    // 宽限期内不探测。
    assert_eq!(
        missing_media_track("", "aac", true, Some(Duration::from_millis(2500))),
        None
    );
    // 无画面有声音 → Video;有画面无声音 → Audio;齐备 → None。
    assert_eq!(
        missing_media_track("", "aac", true, Some(Duration::from_secs(4))),
        Some(Video)
    );
    assert_eq!(
        missing_media_track("h264", "", true, Some(Duration::from_secs(4))),
        Some(Audio)
    );
    assert_eq!(
        missing_media_track("h264", "aac", true, Some(Duration::from_secs(4))),
        None
    );
    // 探测点缺失(未载入)不探测。
    assert_eq!(missing_media_track("", "aac", true, None), None);
}

#[test]
fn mount_pulse_covers_half_pane_fullscreen_and_stops_when_attached() {
    // 全播放页未挂载 → 脉冲。
    assert!(mount_pulse_active(Route::Player, false, false, false, false));
    // 半屏抽屉(route 停在 Home)未挂载 → 必须脉冲(修复冷启动/关闭播放条后首个视频黑屏)。
    assert!(mount_pulse_active(Route::Home, true, false, false, false));
    // 全屏态未挂载 → 必须脉冲(切全屏重挂新 SwapChainPanel)。
    assert!(mount_pulse_active(Route::Player, false, true, false, false));
    assert!(mount_pulse_active(Route::Home, true, true, false, false));
    // 已挂载成功 → 任何路由都停脉冲。
    assert!(!mount_pulse_active(Route::Player, false, false, true, false));
    assert!(!mount_pulse_active(Route::Home, true, false, true, false));
    assert!(!mount_pulse_active(Route::Player, false, true, true, false));
    // 首页无半屏抽屉、无播放画面 → 不脉冲(省电:无 surface 需要挂载)。
    assert!(!mount_pulse_active(Route::Home, false, false, false, false));
    // 其它空态页(收藏/历史等)同理不脉冲。
    assert!(!mount_pulse_active(Route::Favorites, false, false, false, false));
    // S2 tier1:park(播放层垫底)等同可见 —— 后台装载/注入靠这条脉冲驱动。
    assert!(mount_pulse_active(Route::Settings, false, false, false, true));
    assert!(!mount_pulse_active(Route::Settings, false, false, true, true));
}

/// 全局搜索框 Enter 提交门控:按下(主/小键盘)提交并拦截;释放与其他键放行。
#[test]
fn search_enter_gate_only_handles_press() {
    let press = |key: VirtualKey, original: VirtualKey, released: bool| KeyEventInfo {
        key,
        original_key: original,
        status: PhysicalKeyStatus {
            repeat_count: 1,
            scan_code: 0x1C,
            is_extended: false,
            is_menu_down: false,
            was_down: false,
            is_released: released,
        },
        modifiers: InputModifiers::NONE,
    };
    assert!(
        search_enter_submits(&press(VirtualKey::ENTER, VirtualKey::ENTER, false)),
        "主键盘 Enter 按下应提交"
    );
    // 小键盘 Enter:VKEY 为 NumpadEnter(0x68),original_key 上报 0x0D。
    assert!(
        search_enter_submits(&press(VirtualKey(0x68), VirtualKey::ENTER, false)),
        "小键盘 Enter 也应提交"
    );
    assert!(
        !search_enter_submits(&press(VirtualKey::ENTER, VirtualKey::ENTER, true)),
        "释放事件不重复提交"
    );
    assert!(
        !search_enter_submits(&press(VirtualKey(0x41), VirtualKey(0x41), false)),
        "普通字符键放行不拦截"
    );
}

#[test]
fn video_config_maps_settings_fields() {
    let settings = AppSettings {
        hardware_decode: false,
        danmaku_enabled: false,
        danmaku_opacity: 0.42,
        danmaku_max_lanes: 6,
        ..AppSettings::default()
    };
    let cfg = video_config_from(&settings);
    assert!(!cfg.hardware_decode);
    assert!(!cfg.danmaku_enabled);
    assert!((cfg.danmaku_opacity - 0.42).abs() < f32::EPSILON);
    assert_eq!(cfg.danmaku_max_lanes, 6);
}

#[test]
fn thumbnail_url_appends_cdn_resize_for_hdslb_images() {
    // 双格式分支由参数化版本锁定(webp/jpg 由运行时探测决定)。
    assert_eq!(
        thumbnail_url_with("http://i2.hdslb.com/bfs/archive/abc.jpg", "webp"),
        "https://i2.hdslb.com/bfs/archive/abc.jpg@480w_270h_1c.webp"
    );
    assert_eq!(
        thumbnail_url_with("http://i2.hdslb.com/bfs/archive/abc.jpg", "jpg"),
        "https://i2.hdslb.com/bfs/archive/abc.jpg@480w_270h_1c.jpg"
    );
    // 运行时版本与参数化版本一致(探测结果只影响后缀)。
    assert_eq!(
        thumbnail_url("https://i0.hdslb.com/bfs/archive/abc.jpg"),
        thumbnail_url_with(
            "https://i0.hdslb.com/bfs/archive/abc.jpg",
            crate::codec::raster_suffix()
        )
    );
    // 已有参数/非图床保持不二次拼接。
    assert_eq!(
        thumbnail_url("https://i0.hdslb.com/bfs/archive/abc.jpg@100w.webp"),
        "https://i0.hdslb.com/bfs/archive/abc.jpg@100w.webp"
    );
    assert_eq!(
        thumbnail_url("https://example.com/pic.jpg"),
        "https://example.com/pic.jpg"
    );
}

#[test]
fn video_item_carries_owner_mid_and_normalized_face_url() {
    let card = FeedCard {
        id: 80433022,
        goto: Some("av".into()),
        bvid: Some("BV1GJ411x7h7".into()),
        cid: Some(137649199),
        title: Some("MV".into()),
        owner: Some(qaq_bx_core::feed::Owner {
            mid: 486906719,
            name: "索尼音乐中国".into(),
            face: Some("http://i2.hdslb.com/bfs/face/up.jpg".into()),
        }),
        ..FeedCard::default()
    };
    let item = video_item(&card);
    assert_eq!(item.aid, 80433022);
    assert_eq!(item.author_mid, 486906719);
    assert_eq!(
        item.author_face_url.as_deref(),
        Some("https://i2.hdslb.com/bfs/face/up.jpg")
    );
}

/// rcmd 卡 stat 的可选互动计数透传（缺失保持 None，UI 不编造数字）。
#[test]
fn video_item_maps_optional_interaction_counts() {
    let with_like = FeedCard {
        id: 1,
        goto: Some("av".into()),
        bvid: Some("BV1x".into()),
        stat: Some(qaq_bx_core::feed::CardStat {
            view: 10,
            danmaku: 2,
            like: Some(88),
            ..Default::default()
        }),
        ..FeedCard::default()
    };
    let item = video_item(&with_like);
    assert_eq!(item.like_count, Some(88));
    assert_eq!(item.coin_count, None);
    assert_eq!(item.favorite_count, None);
    assert_eq!(item.share_count, None);

    let bare = FeedCard {
        id: 2,
        goto: Some("av".into()),
        bvid: Some("BV2".into()),
        ..FeedCard::default()
    };
    let item = video_item(&bare);
    assert_eq!(item.like_count, None);
    assert_eq!(item.play_count, 0);
}

#[test]
fn archive_video_item_maps_owner_stat_and_https_thumbnail() {
    let card = qaq_bx_core::feed::ArchiveCard {
        aid: 80433022,
        bvid: "BV1GJ411x7h7".into(),
        cid: 137649199,
        title: "测试稿件".into(),
        pic: "http://i0.hdslb.com/bfs/archive/x.jpg".into(),
        duration: 210,
        pubdate: 1501000000,
        owner: qaq_bx_core::feed::Owner {
            mid: 486906719,
            name: "索尼音乐中国".into(),
            face: Some("//i0.hdslb.com/bfs/face/up.jpg".into()),
        },
        stat: qaq_bx_core::feed::CardStat {
            view: 1_234_567,
            danmaku: 4_567,
            like: Some(89_000),
            coin: Some(3_420),
            favorite: Some(5_890),
            share: Some(123),
            reply: None,
        },
        desc: None,
        tname: None,
        tid: None,
        score: None,
        videos: None,
    };
    let item = archive_video_item(&card);
    assert_eq!(item.aid, 80433022);
    assert_eq!(item.cid, 137649199);
    assert_eq!(item.author_mid, 486906719);
    assert_eq!(item.duration_secs, 210);
    assert_eq!(item.play_count, 1_234_567);
    assert_eq!(item.danmaku_count, 4_567);
    assert_eq!(item.like_count, Some(89_000));
    assert_eq!(item.coin_count, Some(3_420));
    assert_eq!(item.favorite_count, Some(5_890));
    assert_eq!(item.share_count, Some(123));
    assert_eq!(
        item.author_face_url.as_deref(),
        Some("https://i0.hdslb.com/bfs/face/up.jpg")
    );
    // 缩略图后缀随 WebP 探测结果变化，仅锚定 https 升级与图源。
    assert!(
        item.pic_url
            .as_deref()
            .is_some_and(|url| url.starts_with("https://i0.hdslb.com/bfs/archive/x.jpg"))
    );
}

#[test]
fn merge_popular_items_dedups_by_bvid() {
    let mut existing = vec![
        minimal_video_item("BV1", "一"),
        minimal_video_item("BV2", "二"),
    ];
    let incoming = vec![
        minimal_video_item("BV2", "二重叠"),
        minimal_video_item("BV3", "三"),
        minimal_video_item("BV3", "三重叠"),
    ];
    let fresh = merge_popular_items(&mut existing, incoming);
    assert_eq!(
        fresh
            .iter()
            .map(|item| item.bvid.as_str())
            .collect::<Vec<_>>(),
        ["BV3"]
    );
    assert_eq!(existing.len(), 3);
    assert_eq!(existing[1].title, "二");
}

#[test]
fn minimal_video_item_falls_back_to_bvid_title() {
    let item = minimal_video_item("BV1xx", "   ");
    assert_eq!(item.title, "BV1xx");
    assert_eq!(item.aid, 0);
    assert_eq!(item.cid, 0);
    assert!(item.cover.is_none() && item.pic_url.is_none());
    let named = minimal_video_item("BV1xx", "真实标题");
    assert_eq!(named.title, "真实标题");
}

#[test]
fn comments_session_tracks_avatar_refs_and_builds_snapshot() {
    let mut session = CommentsSessionState::default();
    session.reset(80433022, 486906719);
    session.hot.replies.push(ReplyItem {
        rpid: 101,
        oid: 80433022,
        root: 0,
        parent: 0,
        mid: 12,
        uname: "评论用户".into(),
        avatar: "https://i0.hdslb.com/bfs/face/u1.jpg".into(),
        level: 6,
        message: "nice".into(),
        ctime: 1700000000,
        like: 5,
        rcount: 0,
        time_desc: "1小时前".into(),
        location: "上海".into(),
        is_up_top: false,
        up_liked: false,
        up_replied: false,
        replies: Vec::new(),
    });

    let thumb = avatar_thumbnail_url("https://i0.hdslb.com/bfs/face/u1.jpg");
    assert!(session.references_avatar(&thumb));
    assert!(
        !session.references_avatar("https://i0.hdslb.com/bfs/face/other.jpg@96w_96h_1c.jpg")
    );

    let cache = ImageCache::with_capacity(8);
    cache.put(thumb.clone(), EncodedImage::new(vec![1, 2, 3]));
    let snap = session.to_snapshot(7, &cache);
    assert_eq!(snap.rev, 7);
    assert_eq!(snap.data.replies.len(), 1);
    assert!(snap.data.avatars.contains_key(&thumb));
}

#[test]
fn native_and_mpv_load_gates_enforce_monotonic_epoch() {
    // PlayURL 解析未完成（epoch == None）：无论引擎残留什么旧代次或错误，均禁止解除闸门。
    assert!(!native_load_gate_settled(None, Some(9), true));
    assert!(!mpv_load_gate_settled(None, 9, true));

    // 世代已提交，但旧会话快照尚未被新会话替换（seq < epoch 且无错）：保持阻塞。
    assert!(!native_load_gate_settled(Some(5), None, false));
    assert!(!native_load_gate_settled(Some(5), Some(4), false));
    assert!(!mpv_load_gate_settled(Some(5), 4, false));

    // 新会话已注入/受理（seq >= epoch）或发生明确错误：放行解除闸门。
    assert!(native_load_gate_settled(Some(5), Some(5), false));
    assert!(native_load_gate_settled(Some(5), Some(6), false));
    assert!(native_load_gate_settled(Some(5), Some(2), true));
    assert!(mpv_load_gate_settled(Some(5), 5, false));
    assert!(mpv_load_gate_settled(Some(5), 7, false));
    assert!(mpv_load_gate_settled(Some(5), 1, true));
}

#[test]
fn missing_native_video_track_triggers_only_when_active_zero_dim_for_3s() {
    assert!(!missing_native_video_track(
        0,
        0,
        false,
        Some(Duration::from_secs(10))
    ));
    assert!(!missing_native_video_track(
        1920,
        1080,
        true,
        Some(Duration::from_secs(10))
    ));
    assert!(!missing_native_video_track(0, 0, true, None));
    assert!(!missing_native_video_track(
        0,
        0,
        true,
        Some(Duration::from_millis(2900))
    ));
    assert!(missing_native_video_track(
        0,
        0,
        true,
        Some(Duration::from_secs(3))
    ));
    assert!(missing_native_video_track(
        1920,
        0,
        true,
        Some(Duration::from_secs(4))
    ));
    assert!(missing_native_video_track(
        0,
        1080,
        true,
        Some(Duration::from_secs(4))
    ));
}

#[test]
fn native_should_show_loading_keeps_spinner_during_opening_and_buffering() {
    use qaq_bx_video::native::NativeStatus;

    // 闸门在途：必显
    let base = NativeStatus::default();
    assert!(native_should_show_loading(&base, true));

    // 注入已完成（gate_pending=false，build loading=false）但处于 Opening / Buffering / None 且有意播放：
    // 旧逻辑 `!status.loading && !status.playing` 会在这里提前抹掉动画，现在必须保持 true！
    let opening = NativeStatus {
        loading: false,
        opening: true,
        wants_play: true,
        ..NativeStatus::default()
    };
    assert!(native_should_show_loading(&opening, false));

    let buffering = NativeStatus {
        loading: false,
        buffering: true,
        wants_play: true,
        media_opened: true,
        position_secs: 42.0,
        ..NativeStatus::default()
    };
    assert!(native_should_show_loading(&buffering, false));

    let pre_open_none = NativeStatus {
        loading: false,
        wants_play: true,
        media_opened: false,
        ..NativeStatus::default()
    };
    assert!(native_should_show_loading(&pre_open_none, false));

    // 真正进入 Playing：关闭加载指示
    let playing = NativeStatus {
        playing: true,
        wants_play: true,
        media_opened: true,
        natural_video_width: 1920,
        natural_video_height: 1080,
        ..NativeStatus::default()
    };
    assert!(!native_should_show_loading(&playing, false));

    // 用户主动 Pause（media_opened=true）：关闭加载指示
    let user_paused = NativeStatus {
        paused: true,
        wants_play: false,
        media_opened: true,
        ..NativeStatus::default()
    };
    assert!(!native_should_show_loading(&user_paused, false));

    // 出错或播完：关闭加载指示
    let errored = NativeStatus {
        opening: true,
        wants_play: true,
        error: Some("失败".into()),
        ..NativeStatus::default()
    };
    assert!(!native_should_show_loading(&errored, false));
}

#[test]
fn native_load_stalled_triggers_only_after_threshold_while_waiting_play() {
    assert!(!native_load_stalled(false, true, false, Some(Duration::from_secs(30))));
    assert!(!native_load_stalled(true, false, false, Some(Duration::from_secs(30))));
    assert!(!native_load_stalled(true, true, true, Some(Duration::from_secs(30))));
    assert!(!native_load_stalled(true, true, false, Some(Duration::from_secs(11))));
    assert!(native_load_stalled(true, true, false, Some(Duration::from_secs(12))));
}

#[test]
fn mpv_should_show_loading_tracks_cache_buffering_and_errors() {
    use qaq_bx_video::engine::PlaybackStatus;

    assert!(mpv_should_show_loading(&PlaybackStatus::default(), false, false));

    // 已解出 duration 但刚卡缓冲（paused-for-cache=true）时必须保持转圈
    let stalled = PlaybackStatus {
        duration: 120.0,
        file_loaded: true,
        playback_started: true,
        buffering: true,
        ..PlaybackStatus::default()
    };
    assert!(mpv_should_show_loading(&stalled, false, false));

    // 首帧起播且非 buffering：熄灭
    let started = PlaybackStatus {
        duration: 120.0,
        file_loaded: true,
        playback_started: true,
        buffering: false,
        ..PlaybackStatus::default()
    };
    assert!(!mpv_should_show_loading(&started, false, false));

    // 引擎出错或结束：熄灭
    assert!(!mpv_should_show_loading(&stalled, false, true));
}
