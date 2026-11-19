//! 内部状态自测(reactor `test` 模式):不开窗口、不模拟鼠标,直接挂载
//! AppShell 组件树并派发 Message,断言壳层状态。覆盖「鼠标模拟测不到」
//! 的路径:快速切视频的播放代次取消、评论双桶缓存、导航历史栈。
//!
//! 用法:`qaq-bx-ui.exe --selftest [all|boot|nav|comments|rapid-switch|stress|search|popular|live|unplayable|half-pane|mobile|fullscreen|engine]`
//! 前置:`cargo build -p qaq-bx-ui --features selftest`;需要真实网络与
//! 本地凭据(nav 探测 / feed / 评论均走真实接口)。

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};
use windows_reactor::{
    component,
    test::{ComponentHost, RecordingAdapter},
};

use crate::pages::account::NotifyTab;
use crate::pages::player::CommentSort;
use crate::router::Route;
use crate::shell::{AppShell, Message, ResolveProfileKey, ShellInput, ShellProbe};
use crate::widgets::peek::{CardRect, HoverReport};
use qaq_bx_core::settings::{AppSettings, PlayerEngine};

/// 可解析真实媒体的测试稿件（open_video 走最小模型 + video_meta 解析管线，不依赖任何种子数据）。
const TEST_BV: &str = "BV1GJ411x7h7";
/// TEST_BV 的真实稿件 id(评论区断言用)。
const TEST_AID: u64 = 80433022;

type Host = ComponentHost<RecordingAdapter>;

struct Scenario {
    host: Host,
    probe: Rc<RefCell<ShellProbe>>,
}

impl Scenario {
    /// 挂载独立的 AppShell 组件树(RecordingAdapter 只记录属性/事件,不开窗)。
    fn mount() -> Result<Self, String> {
        let probe = Rc::new(RefCell::new(ShellProbe::default()));
        let mut host = ComponentHost::mount(
            RecordingAdapter::default(),
            [component::<AppShell>(
                "root",
                ShellInput {
                    probe: Some(Rc::clone(&probe)),
                },
            )],
        );
        host.drain(usize::MAX);
        Ok(Self { host, probe })
    }

    fn send(&mut self, message: Message) -> Result<(), String> {
        let sender = self
            .host
            .sender::<AppShell>(&"root".into())
            .ok_or_else(|| "root 组件 sender 缺失".to_string())?;
        // 队列关闭(组件退役)时返回 Err——测试场景内组件始终存活,忽略即可。
        let _ = sender.send(message);
        Ok(())
    }

    /// 驱动消息队列至静止,再派发一次 SelfTestSync 强制在尾部刷新探针。
    fn sync(&mut self) -> Result<(), String> {
        self.host.drain(usize::MAX);
        self.send(Message::SelfTestSync)?;
        self.host.drain(usize::MAX);
        Ok(())
    }

    /// 轮询直到谓词成立;每轮全量 drain + sync,超时返回带探针状态的错误。
    fn wait_for(
        &mut self,
        timeout: Duration,
        predicate: impl Fn(&ShellProbe) -> bool,
    ) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        loop {
            self.sync()?;
            if predicate(&self.probe.borrow()) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "等待超时({timeout:?}),最后探针状态:{:?}",
                    self.probe.borrow()
                ));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

/// 场景 1:挂载 + 首页 feed 落定(覆盖 create 的 feed/登录探测启动路径)。
fn boot(s: &mut Scenario) -> Result<(), String> {
    s.wait_for(Duration::from_secs(30), |p| {
        p.route_tag == "home" && p.history_len == 0
    })
}

/// 场景 2:导航标签切换与历史栈(设置页 → Back → 首页)。
fn nav(s: &mut Scenario) -> Result<(), String> {
    s.sync()?;
    assert_eq!(s.probe.borrow().route_tag, "home", "初始路由应为 home");
    s.send(Message::NavTagSelected(Some("settings".into())))?;
    s.sync()?;
    {
        let probe = s.probe.borrow();
        assert_eq!(probe.route_tag, "settings", "导航标签应切换到设置页");
        assert_eq!(probe.history_len, 1, "导航切换应留一层历史");
    }
    s.send(Message::Back)?;
    s.sync()?;
    let probe = s.probe.borrow();
    assert_eq!(probe.route_tag, "home", "Back 应回到首页");
    assert_eq!(probe.history_len, 0, "返回后历史应为空");
    Ok(())
}

/// 场景 3:真实评论区(解析稿件 → 首页评论 → 排序切换 → 双桶缓存保留)。
fn comments(s: &mut Scenario) -> Result<(), String> {
    s.send(Message::OpenVideo(TEST_BV.into()))?;
    s.wait_for(Duration::from_secs(60), |p| {
        p.now_playing_bvid.as_deref() == Some(TEST_BV) && p.hot_loaded && p.hot_replies > 0
    })
    .map_err(|err| format!("评论首页未就绪:{err}"))?;
    {
        let probe = s.probe.borrow();
        assert_eq!(probe.comments_aid, TEST_AID, "评论会话 aid 应为真实稿件");
        assert!(!probe.comments_sort_latest, "默认排序应为按热度");
        assert!(probe.hot_total.unwrap_or(0) > 0, "总评论数应 > 0");
    }
    // 相关推荐:真实 archive/related 应随同一次播放就绪。
    s.wait_for(Duration::from_secs(30), |p| {
        p.related_items > 0 && p.related_bvid == TEST_BV
    })
    .map_err(|err| format!("相关推荐未就绪:{err}"))?;
    let hot_before = s.probe.borrow().hot_replies;

    s.send(Message::SetCommentSort(CommentSort::Latest))?;
    s.wait_for(Duration::from_secs(60), |p| {
        p.comments_sort_latest && p.latest_loaded && p.latest_replies > 0
    })
    .map_err(|err| format!("按时间排序未就绪:{err}"))?;

    // 双桶缓存:切回热度后原页数据应保留,不得重置。
    s.send(Message::SetCommentSort(CommentSort::Hot))?;
    s.wait_for(Duration::from_secs(10), |p| !p.comments_sort_latest)
        .map_err(|err| format!("切回按热度失败:{err}"))?;
    s.sync()?;
    {
        let probe = s.probe.borrow();
        assert!(probe.hot_loaded, "热度桶缓存应保留");
        assert_eq!(probe.hot_replies, hot_before, "热度桶评论数不应被重置");
    }

    s.send(Message::Back)?;
    s.sync()?;
    {
        let probe = s.probe.borrow();
        assert_eq!(probe.route_tag, "home", "播放页 Back 应回首页");
        // 后台播放(park)产品默认关:离开播放页即关播并释放会话。
        assert!(
            probe.now_playing_bvid.is_none(),
            "离开播放页应停止播放并释放当前视频"
        );
    }
    Ok(())
}

/// 场景 4:快速连续切视频(刻意不等待解析完成,叠加在飞请求,
/// 考验 playback_gen 取消路径与评论区会话重置——复现 try_wake panic 的高压路径)。
fn rapid_switch(s: &mut Scenario) -> Result<(), String> {
    s.sync()?;
    let gen0 = s.probe.borrow().playback_gen;
    for _ in 0..3 {
        s.send(Message::OpenVideo(TEST_BV.into()))?;
        std::thread::sleep(Duration::from_millis(150));
    }
    s.host.drain(usize::MAX);
    s.sync()?;
    {
        let probe = s.probe.borrow();
        let gen_after = probe.playback_gen;
        assert!(
            gen_after >= gen0 + 3,
            "播放代次应至少递增 3 次(gen {gen0} → {gen_after})"
        );
        assert_eq!(probe.route_tag, "player", "三连开之后应停留在播放页");
    }

    s.send(Message::Back)?;
    std::thread::sleep(Duration::from_millis(150));
    s.send(Message::OpenVideo(TEST_BV.into()))?;
    s.sync()?;
    {
        let probe = s.probe.borrow();
        assert_eq!(probe.route_tag, "player", "返回后再次打开应进播放页");
        assert!(
            probe.playback_gen >= gen0 + 4,
            "返回再开应继续递增代次(gen {} → {})",
            gen0 + 3,
            probe.playback_gen
        );
    }
    Ok(())
}

/// 场景 5:消息风暴——追查 2026-11-16 22:43 的 try_wake panic 类问题。
/// 在解析/评论/头像请求在飞时高频注入交互消息(排序翻转/翻页/导航/引擎控制),
/// 多轮叠加,制造消息乱序抵达 update 的工况;headless 下 panic 会被捕获为 FAIL
/// 而非进程 abort,原始 panic 消息可留存。
fn stress(s: &mut Scenario) -> Result<(), String> {
    s.sync()?;
    let gen0 = s.probe.borrow().playback_gen;
    for _ in 0..3 {
        s.send(Message::OpenVideo(TEST_BV.into()))?;
        for _ in 0..8 {
            s.send(Message::SetCommentSort(CommentSort::Latest))?;
            s.send(Message::SetCommentSort(CommentSort::Hot))?;
            s.send(Message::LoadMoreComments)?;
            s.send(Message::Seek(12.0))?;
            s.send(Message::TogglePlay)?;
            s.send(Message::Back)?;
            s.send(Message::OpenVideo(TEST_BV.into()))?;
            s.send(Message::NavTagSelected(Some("settings".into())))?;
            s.send(Message::NavTagSelected(Some("home".into())))?;
            s.send(Message::Back)?;
            std::thread::sleep(Duration::from_millis(40));
        }
        s.host.drain(usize::MAX);
    }
    s.sync()?;
    let gen_after = s.probe.borrow().playback_gen;
    assert!(
        gen_after > gen0,
        "消息风暴后代次应推进(gen {gen0} → {gen_after})"
    );
    Ok(())
}

/// 场景 6:搜索(真实 WBI 签名搜索 → 结果网格 → 点开首个结果,验证
/// open_video 的 search_index 回退链)。
fn search(s: &mut Scenario) -> Result<(), String> {
    s.sync()?;
    s.send(Message::SearchDraftChanged("rust".into()))?;
    s.send(Message::SearchSubmitted)?;
    s.wait_for(Duration::from_secs(45), |p| {
        p.route_tag == "search" && p.search_videos > 0
    })
    .map_err(|err| format!("搜索结果未就绪:{err}"))?;
    let first = s
        .probe
        .borrow()
        .search_first_bvid
        .clone()
        .ok_or("搜索结果缺少首条 bvid")?;
    s.send(Message::OpenVideo(first.clone()))?;
    s.wait_for(Duration::from_secs(60), |p| {
        p.now_playing_bvid.as_deref() == Some(first.as_str())
    })
    .map_err(|err| format!("搜索结果打开失败:{err}"))?;
    s.send(Message::Back)?;
    s.sync()?;
    assert_eq!(s.probe.borrow().route_tag, "search", "Back 应回到搜索页");
    Ok(())
}

/// 场景 8:热门页真实数据（导航进入 → popular 分页落定 → 首卡经 popular_index 打开）。
fn popular(s: &mut Scenario) -> Result<(), String> {
    s.sync()?;
    s.send(Message::NavTagSelected(Some("popular".into())))?;
    s.wait_for(Duration::from_secs(30), |p| {
        p.route_tag == "popular" && p.popular_items > 0
    })
    .map_err(|err| format!("热门数据未就绪:{err}"))?;
    let first = s
        .probe
        .borrow()
        .popular_first_bvid
        .clone()
        .ok_or("热门首卡缺少 bvid")?;
    s.send(Message::OpenVideo(first.clone()))?;
    s.wait_for(Duration::from_secs(60), |p| {
        p.route_tag == "player" && p.now_playing_bvid.as_deref() == Some(first.as_str())
    })
    .map_err(|err| format!("热门首卡打开失败:{err}"))?;
    s.send(Message::Back)?;
    s.sync()?;
    assert_eq!(s.probe.borrow().route_tag, "popular", "Back 应回到热门页");
    Ok(())
}

/// 场景 7:图像编解码探测(WebP WIC 探测器一致性 + 后缀与探测结果一致)。
fn codec(s: &mut Scenario) -> Result<(), String> {
    s.sync()?;
    let first = crate::codec::webp_supported();
    let again = crate::codec::webp_supported();
    assert_eq!(first, again, "探测结果应进程内缓存一致");
    assert!(
        first == (crate::codec::raster_suffix() == "webp"),
        "URL 后缀应与探测结果一致"
    );
    println!(
        "[codec] webp_supported={first} raster_suffix={}",
        crate::codec::raster_suffix()
    );
    // GUID 交叉验证:本机若有真实 WebP 样本(target/bg-probe/),文件可解码
    // 而 GUID 探测失败 ⇒ GUID 写错,必须修正而不是让 WebP 静默失效。
    let probe = std::path::Path::new("../bg-probe/b1280.webp");
    if probe.exists() {
        let by_file = crate::codec::probe_webp_file(probe);
        println!("[codec] probe-file={by_file}");
        if by_file {
            assert!(first, "真实 WebP 可解码但 GUID 探测失败——GUID 写错了");
        }
    }
    Ok(())
}

/// 场景 9:首页半屏抽屉挂载 + 加载指示生命周期(回归冷启动首个视频黑屏 /
/// 关闭播放条丢弃引擎后重开复发的 bug)。半屏态 route 合法地停在 Home——旧门控
/// 只认 `route == Player`,`mount_tick` 永不脉冲,swapchain 错过唯一一次 metrics
/// 挂载后再无重试(真机上表现为永久黑屏)。修复后:
/// 开播 → 环形指示点亮;脉冲驱动 swapchain 挂载;首个 has_media → 指示熄灭。
fn half_pane_pulse(s: &mut Scenario) -> Result<(), String> {
    s.send(Message::OpenVideo(TEST_BV.into()))?;
    s.sync()?;
    assert!(
        s.probe.borrow().video_loading,
        "开播后加载环形指示应点亮(解析/缓冲中)"
    );
    s.wait_for(Duration::from_secs(60), |p| {
        p.route_tag == "home" && p.home_half_pane && p.now_playing_bvid.as_deref() == Some(TEST_BV)
    })
    .map_err(|err| format!("半屏抽屉未就绪:{err}"))?;
    s.wait_for(Duration::from_secs(20), |p| {
        p.engine_attached && p.mount_tick >= 1
    })
    .map_err(|err| format!("半屏挂载脉冲未驱动 swapchain 挂载(旧 bug 现场):{err}"))?;
    s.wait_for(Duration::from_secs(60), |p| !p.video_loading)
        .map_err(|err| format!("媒体就绪后加载指示未熄灭:{err}"))?;

    // 半屏独立控件：弹幕开关切换、静音/恢复、关闭半屏即停止播放
    assert!(s.probe.borrow().danmaku_enabled, "初始设置应开启弹幕");
    s.send(Message::ToggleDanmaku)?;
    s.sync()?;
    assert!(
        !s.probe.borrow().danmaku_enabled,
        "ToggleDanmaku 应关闭弹幕"
    );
    s.send(Message::ToggleDanmaku)?;
    s.sync()?;
    assert!(
        s.probe.borrow().danmaku_enabled,
        "再次 ToggleDanmaku 应开启弹幕"
    );

    assert_eq!(s.probe.borrow().volume, 30);
    s.send(Message::ToggleMute)?;
    s.sync()?;
    assert_eq!(s.probe.borrow().volume, 0, "ToggleMute 应静音");
    s.send(Message::ToggleMute)?;
    s.sync()?;
    assert_eq!(s.probe.borrow().volume, 30, "再次 ToggleMute 应恢复原音量");

    // 半屏态直进全屏 → 退出全屏应回到半屏抽屉，再关闭半屏停播
    s.send(Message::EnterFullscreen)?;
    s.sync()?;
    assert!(s.probe.borrow().is_fullscreen, "半屏态应支持进入全屏");
    s.send(Message::ExitFullscreen)?;
    s.sync()?;
    {
        let p = s.probe.borrow();
        assert!(!p.is_fullscreen, "退出全屏应关闭全屏标志");
        assert!(p.home_half_pane, "半屏态退出全屏应回到半屏抽屉");
    }

    s.send(Message::CloseHalfPane)?;
    s.sync()?;
    {
        let p = s.probe.borrow();
        assert!(!p.home_half_pane, "CloseHalfPane 应关闭半屏");
        assert!(p.now_playing_bvid.is_none(), "关闭半屏后应停止后台播放");
    }
    Ok(())
}

/// 场景 10:Mobile 形态联动（390px 切底栏/2 列/禁半窗/私信单页钻取与 Back 栈/Medium 悬浮搜索/回切 Desktop）。
fn mobile(s: &mut Scenario) -> Result<(), String> {
    s.sync()?;
    {
        let p = s.probe.borrow();
        assert!(!p.is_mobile, "默认启动应为 Desktop 形态");
        // F5 卡片曲线:1280x800 → 内容宽 1224 → 5 列(整 DIP 分满,行尾无空白)。
        assert_eq!(p.grid_cols, 5, "1280x800 曲线应为 5 列");
    }

    // 1. 缩放至 390x760：切 Mobile + 2 列
    s.send(Message::WindowResized(windows_reactor::WindowSize {
        width: 390.0,
        height: 760.0,
    }))?;
    s.sync()?;
    {
        let p = s.probe.borrow();
        assert!(p.is_mobile, "390px 应进入 Mobile 形态");
        assert_eq!(p.grid_cols, 2, "Mobile 应严格为 2 列");
    }

    // 2. Mobile 禁用 HalfPane：即使设置 HalfPane，点视频应直达 Route::Player
    s.send(Message::OpenVideo(TEST_BV.into()))?;
    s.sync()?;
    {
        let p = s.probe.borrow();
        assert!(!p.home_half_pane, "Mobile 下应禁用 HalfPane");
        assert_eq!(p.route_tag, "player", "Mobile 点卡应直达播放页");
    }
    s.send(Message::Back)?;
    s.sync()?;
    assert_eq!(s.probe.borrow().route_tag, "home", "播放页 Back 应回首页");

    // 3. 底栏「更多」新增 WatchLater 导航 + 我的私信单页钻取与 Back 优先级
    s.send(Message::NavigateCompact(Route::WatchLater))?;
    s.sync()?;
    assert_eq!(s.probe.borrow().route_tag, "watch-later");

    s.send(Message::NavigateCompact(Route::Login))?;
    s.sync()?;
    assert_eq!(s.probe.borrow().route_tag, "login");

    s.send(Message::SelectNotifyTab(NotifyTab::Whisper))?;
    s.send(Message::SelectWhisperSession((12_345, 1)))?;
    s.sync()?;
    assert!(
        s.probe.borrow().whisper_detail_open,
        "Mobile 下选中私信会话应展开单页对话详情"
    );

    // 第一次 Back 应先退回会话列表而非弹出路由历史
    s.send(Message::Back)?;
    s.sync()?;
    {
        let p = s.probe.borrow();
        assert!(!p.whisper_detail_open, "首次 Back 应先关闭私信详情");
        assert_eq!(p.route_tag, "login", "首次 Back 不得离开个人中心");
    }

    // 第二次 Back 才弹出路由回到 watch-later
    s.send(Message::Back)?;
    s.sync()?;
    assert_eq!(s.probe.borrow().route_tag, "watch-later");

    // 4. Medium 半屏（640x900）：处于 2 列区间即保持 Mobile，且顶栏右上可呼出悬浮搜索浮窗并提交跳 Search
    s.send(Message::WindowResized(windows_reactor::WindowSize {
        width: 640.0,
        height: 900.0,
    }))?;
    s.sync()?;
    assert!(s.probe.borrow().is_mobile, "640px 2 列区应保持 Mobile 形态");
    assert_eq!(s.probe.borrow().grid_cols, 2, "640px 应铺满 2 列");
    s.send(Message::ToggleSearchOverlay)?;
    s.sync()?;
    assert!(
        s.probe.borrow().search_overlay_open,
        "600..859 半屏下应可呼出悬浮搜索"
    );
    s.send(Message::SearchDraftChanged("rust".into()))?;
    s.send(Message::SearchSubmitted)?;
    s.sync()?;
    {
        let p = s.probe.borrow();
        assert!(!p.search_overlay_open, "提交搜索后应自动收起悬浮窗");
        assert_eq!(p.route_tag, "search", "提交搜索应切至搜索页");
    }

    // 5. 860x800 起恰能容纳 3 列，切回 Desktop；1280x800 恢复 4 列
    s.send(Message::WindowResized(windows_reactor::WindowSize {
        width: 860.0,
        height: 800.0,
    }))?;
    s.sync()?;
    {
        let p = s.probe.borrow();
        assert!(!p.is_mobile, "860px 3 列起点应切回 Desktop");
        assert_eq!(p.grid_cols, 3, "860px 应渲染 3 列");
    }

    s.send(Message::WindowResized(windows_reactor::WindowSize {
        width: 1280.0,
        height: 800.0,
    }))?;
    s.sync()?;
    {
        let p = s.probe.borrow();
        assert!(!p.is_mobile);
        // F5 卡片曲线:1280x800 → 5 列(原 4 列,行尾无空白)。
        assert_eq!(p.grid_cols, 5);
        assert!(!p.whisper_detail_open);
        assert!(!p.search_overlay_open);
    }
    Ok(())
}

/// 场景 11:全屏播放状态机与快捷键闭环（无视频守卫 / 全屏进入 / 弹幕·静音·快进·播停热键 / Back 优先退出全屏 / 导航切换自动退全屏）。
fn fullscreen(s: &mut Scenario) -> Result<(), String> {
    use crate::pages::fullscreen::PlayerHotkey;

    s.sync()?;
    // 1. 无视频播放时进入/切换全屏应为安全空操作
    s.send(Message::EnterFullscreen)?;
    s.send(Message::ToggleFullscreen)?;
    s.send(Message::PlayerHotkey(PlayerHotkey::ToggleFullscreen))?;
    s.sync()?;
    assert!(!s.probe.borrow().is_fullscreen, "无视频播放时不得进入全屏");

    // 2. 打开真实视频并等待挂载就绪
    s.send(Message::OpenVideo(TEST_BV.into()))?;
    s.wait_for(Duration::from_secs(60), |p| {
        p.route_tag == "player"
            && p.now_playing_bvid.as_deref() == Some(TEST_BV)
            && p.engine_attached
            && !p.video_loading
    })
    .map_err(|err| format!("全屏前置视频未就绪:{err}"))?;

    // 3. 进入全屏并驱动全屏快捷键（弹幕 / 静音 / 音量 / 快进 / 播停）
    s.send(Message::EnterFullscreen)?;
    s.sync()?;
    {
        let p = s.probe.borrow();
        assert!(p.is_fullscreen, "EnterFullscreen 应进入全屏态");
        assert_eq!(p.route_tag, "player");
    }

    let initial_danmaku = s.probe.borrow().danmaku_enabled;
    s.send(Message::PlayerHotkey(PlayerHotkey::ToggleDanmaku))?;
    s.sync()?;
    assert_ne!(
        s.probe.borrow().danmaku_enabled,
        initial_danmaku,
        "全屏 D 键应切换弹幕"
    );
    s.send(Message::PlayerHotkey(PlayerHotkey::ToggleDanmaku))?;
    s.sync()?;

    s.send(Message::PlayerHotkey(PlayerHotkey::ToggleMute))?;
    s.sync()?;
    assert_eq!(s.probe.borrow().volume, 0, "全屏 M 键应静音");
    s.send(Message::PlayerHotkey(PlayerHotkey::VolumeDelta(15)))?;
    s.sync()?;
    assert_eq!(s.probe.borrow().volume, 15, "全屏方向上键应恢复并增加音量");
    s.send(Message::PlayerHotkey(PlayerHotkey::SeekDelta(5)))?;
    s.send(Message::PlayerHotkey(PlayerHotkey::TogglePlay))?;
    s.send(Message::PlayerHotkey(PlayerHotkey::TogglePlay))?;
    s.sync()?;

    // 4. Back 应优先退出全屏而不直接退出播放页
    s.send(Message::Back)?;
    s.sync()?;
    {
        let p = s.probe.borrow();
        assert!(!p.is_fullscreen, "首次 Back 应先退出全屏");
        assert_eq!(p.route_tag, "player", "退出全屏后应留在播放页");
        assert_eq!(p.now_playing_bvid.as_deref(), Some(TEST_BV));
    }

    // 5. F/F11 再次进入全屏，切换导航应自动退全屏并释放视频
    s.send(Message::PlayerHotkey(PlayerHotkey::ToggleFullscreen))?;
    s.sync()?;
    assert!(
        s.probe.borrow().is_fullscreen,
        "ToggleFullscreen 应再次进入全屏"
    );
    s.send(Message::NavTagSelected(Some("home".into())))?;
    s.sync()?;
    {
        let p = s.probe.borrow();
        assert!(!p.is_fullscreen, "导航离开应自动退出全屏");
        assert_eq!(p.route_tag, "home");
        // 后台播放(park)产品默认关:导航离开即关播并释放会话。
        assert!(p.now_playing_bvid.is_none());
    }

    // 6. 竖屏模式(实验)全屏进退彻底隔离回归:
    // 在 1280x800 桌面尺寸下开启 portrait_mode → 立即收敛至竖屏 Mobile 2 列(859 DIP);
    // 打开视频 → 进入全屏 → 收到 1920x1080 全屏 WindowResized → 底层视口保持 Mobile 2 列不被污染;
    // 退出全屏首帧立即维持 Mobile 2 列,0 闪回 Desktop 布局。
    s.send(Message::WindowResized(windows_reactor::WindowSize {
        width: 1280.0,
        height: 800.0,
    }))?;
    s.sync()?;
    assert!(!s.probe.borrow().is_mobile, "前置应为桌面 1280 宽");
    s.send(Message::UpdateSettings(AppSettings {
        portrait_mode: true,
        player_engine: PlayerEngine::Mpv,
        ..AppSettings::default()
    }))?;
    s.sync()?;
    {
        let p = s.probe.borrow();
        assert!(p.portrait_mode, "竖屏模式开关应已置位");
        assert!(p.is_mobile, "开启竖屏模式当帧必须立即切换为 Mobile 壳层");
        assert_eq!(p.grid_cols, 2, "开启竖屏模式当帧必须立即收敛为 2 列");
        assert!(
            p.window_width < crate::layout::MOBILE_BREAKPOINT,
            "开启竖屏模式后有效布局宽度必须被钳制在 < 860 DIP"
        );
    }
    s.send(Message::OpenVideo(TEST_BV.into()))?;
    s.wait_for(Duration::from_secs(60), |p| {
        p.route_tag == "player"
            && p.now_playing_bvid.as_deref() == Some(TEST_BV)
            && p.engine_attached
            && !p.video_loading
    })
    .map_err(|err| format!("竖屏全屏前置视频未就绪:{err}"))?;
    {
        let p = s.probe.borrow();
        if p.quality_count >= 2 {
            let target_qn = if p.current_qn == 32 { 16 } else { 32 };
            drop(p);
            s.send(Message::SelectQuality(target_qn))?;
            s.sync()?;
            assert!(
                s.probe.borrow().is_mobile,
                "竖屏模式切换画质期间必须保持 Mobile 布局不 panic"
            );
        }
    }
    s.send(Message::EnterFullscreen)?;
    s.send(Message::WindowResized(windows_reactor::WindowSize {
        width: 1920.0,
        height: 1080.0,
    }))?;
    s.sync()?;
    {
        let p = s.probe.borrow();
        assert!(p.is_fullscreen, "竖屏模式下应正常进入全屏");
        assert!(
            p.is_mobile && p.grid_cols == 2 && p.window_width < crate::layout::MOBILE_BREAKPOINT,
            "全屏 1920x1080 WindowResized 严禁污染底层竖屏布局(is_mobile={}, cols={}, w={})",
            p.is_mobile,
            p.grid_cols,
            p.window_width
        );
    }
    s.send(Message::ExitFullscreen)?;
    s.sync()?;
    {
        let p = s.probe.borrow();
        assert!(!p.is_fullscreen, "ExitFullscreen 应立即退出全屏");
        assert!(
            p.is_mobile && p.grid_cols == 2 && p.window_width < crate::layout::MOBILE_BREAKPOINT,
            "退出全屏首帧(窗口尚未异步缩回前)必须严格保持竖屏 Mobile 布局,不得闪回 Desktop"
        );
    }
    // 即使在非全屏下收到异常超宽 WindowResized,竖屏模式也必须死锁在 Mobile 2 列
    s.send(Message::WindowResized(windows_reactor::WindowSize {
        width: 1920.0,
        height: 1080.0,
    }))?;
    s.sync()?;
    {
        let p = s.probe.borrow();
        assert!(
            p.is_mobile && p.grid_cols == 2 && p.window_width < crate::layout::MOBILE_BREAKPOINT,
            "竖屏模式开启期间任意超宽 WindowResized 均不得突破 Mobile 边界"
        );
    }
    s.send(Message::NavTagSelected(Some("home".into())))?;
    s.sync()?;

    Ok(())
}

/// 场景 12:直播页真实数据(U4;导航进入 → 房间列表落定 → 点首房直链进 Player → Back)。
fn live(s: &mut Scenario) -> Result<(), String> {
    s.sync()?;
    s.send(Message::NavTagSelected(Some("live".into())))?;
    s.wait_for(Duration::from_secs(30), |p| {
        p.route_tag == "live" && p.live_rooms > 0
    })
    .map_err(|err| format!("直播数据未就绪:{err}"))?;
    let roomid = s.probe.borrow().live_first_roomid;
    assert!(roomid > 0, "直播首房缺少 roomid");
    let expect = format!("live:{roomid}");
    s.send(Message::LiveRoomSelected(roomid, "自测房间".into(), "UP".into()))?;
    s.wait_for(Duration::from_secs(60), |p| {
        p.route_tag == "player" && p.now_playing_bvid.as_deref() == Some(expect.as_str())
    })
    .map_err(|err| format!("直播首房打开失败:{err}"))?;
    s.send(Message::Back)?;
    s.sync()?;
    assert_eq!(s.probe.borrow().route_tag, "live", "Back 应回到直播页");
    Ok(())
}

/// 场景 13:付费稿件过滤(充电/付费未解锁的试看降级)。
/// 派发 MediaPreResolved 试看降级错误 → unplayable 登记;同 bvid 去重;
/// 非试看错误(-404 等)不得登记。纯状态断言,不发网络请求。
fn unplayable(s: &mut Scenario) -> Result<(), String> {
    s.sync()?;
    let key = ResolveProfileKey {
        max_qn: 116,
        codec_idx: 0,
    };
    let trial = |bvid: &str| Message::MediaPreResolved {
        bvid: bvid.into(),
        key,
        result: Err("该稿件未解锁（充电专稿/付费专稿），仅提供 180 秒试看片段，不支持播放".into()),
    };
    s.send(trial("BV1TEST_TRIAL1"))?;
    s.sync()?;
    assert_eq!(
        s.probe.borrow().unplayable_count,
        1,
        "试看降级应登记不可播"
    );
    s.send(trial("BV1TEST_TRIAL1"))?;
    s.sync()?;
    assert_eq!(
        s.probe.borrow().unplayable_count,
        1,
        "同 bvid 重复登记应去重"
    );
    s.send(Message::MediaPreResolved {
        bvid: "BV1TEST_ERR".into(),
        key,
        result: Err("API 错误 -404: 啥都木有".into()),
    })?;
    s.sync()?;
    assert_eq!(
        s.probe.borrow().unplayable_count,
        1,
        "非试看错误不得登记不可播"
    );
    Ok(())
}

/// 场景 14:播放引擎设置项(M7)。走真实 settings.json(不含 `player_engine` 字段 ⇒
/// 老配置必须默认 mpv)、真实 `UpdateSettings` 路径与真实直播打开路径:
/// ① 切 native 只记账 —— 没有装载时 `native_playback` 必须保持 false(引擎切换
///    不得偷偷重开/接管任何通路);
/// ② 直播即使设成 native 也永不接管 —— 控制/弹幕/视图三路分流单标志不被劫持,
///    否则进度与音量会打到已卸载的系统媒体引擎上;
/// ③ 切回 mpv 的收口不炸(透明层与 SMTC 写权此时本就未建立)。
fn engine(s: &mut Scenario) -> Result<(), String> {
    s.sync()?;
    {
        let p = s.probe.borrow();
        assert_eq!(p.player_engine, "mpv", "老配置缺 player_engine 必须回落 mpv");
        assert!(!p.native_playback, "未装载不得由 native 驱动");
    }

    s.send(Message::UpdateSettings(AppSettings {
        player_engine: PlayerEngine::Native,
        ..AppSettings::default()
    }))?;
    s.sync()?;
    {
        let p = s.probe.borrow();
        assert_eq!(p.player_engine, "native", "设置项改完生效引擎必须立刻翻转");
        assert!(
            !p.native_playback,
            "只记账口径:切换不得接管任何在播通路"
        );
    }

    // 直播(FLV):即使设置项是 native,也必须整条留在 mpv。
    s.send(Message::NavTagSelected(Some("live".into())))?;
    s.wait_for(Duration::from_secs(30), |p| p.route_tag == "live" && p.live_rooms > 0)
        .map_err(|err| format!("直播数据未就绪:{err}"))?;
    let roomid = s.probe.borrow().live_first_roomid;
    assert!(roomid > 0, "直播首房缺少 roomid");
    s.send(Message::LiveRoomSelected(roomid, "引擎自测房间".into(), "UP".into()))?;
    let expect = format!("live:{roomid}");
    s.wait_for(Duration::from_secs(60), |p| {
        p.route_tag == "player" && p.now_playing_bvid.as_deref() == Some(expect.as_str())
    })
    .map_err(|err| format!("直播首房打开失败:{err}"))?;
    {
        let p = s.probe.borrow();
        assert_eq!(p.player_engine, "native", "直播不应改动生效引擎");
        assert!(!p.native_playback, "直播被 native 劫持:控制/弹幕会打到已卸载引擎");
    }

    s.send(Message::UpdateSettings(AppSettings {
        player_engine: PlayerEngine::Mpv,
        ..AppSettings::default()
    }))?;
    s.sync()?;
    {
        let p = s.probe.borrow();
        assert_eq!(p.player_engine, "mpv", "切回 mpv 后生效引擎应回到 mpv");
        assert!(!p.native_playback, "切回 mpv 后三路分流必须整体下线 native");
    }
    Ok(())
}

/// 场景 15:加载动画与 CDN/预加载策略状态机集成测试（开播/换画质/切配置 0 panic）。
fn loading_state(s: &mut Scenario) -> Result<(), String> {
    s.sync()?;
    {
        let p = s.probe.borrow();
        assert!(p.prefer_official_cdn, "默认应优先官方 CDN");
        assert!(p.multi_thread_preload, "默认应开启多线程并发预加载");
    }

    s.send(Message::OpenVideo(TEST_BV.into()))?;
    s.sync()?;
    assert!(
        s.probe.borrow().video_loading,
        "打开视频瞬间必须立刻置位 video_loading 展示加载动画"
    );

    s.wait_for(Duration::from_secs(60), |p| {
        p.route_tag == "player"
            && p.now_playing_bvid.as_deref() == Some(TEST_BV)
            && p.engine_attached
            && !p.video_loading
    })
    .map_err(|err| format!("视频起播未落定:{err}"))?;

    // 切换画质必须再次触发 video_loading=true 并在重载完成后熄灭。
    let next_qn = {
        let p = s.probe.borrow();
        if p.quality_count >= 2 {
            Some(if p.current_qn == 32 { 16 } else { 32 })
        } else {
            None
        }
    };
    if let Some(qn) = next_qn {
        s.send(Message::SelectQuality(qn))?;
        s.sync()?;
        assert!(
            s.probe.borrow().video_loading,
            "切换画质后在途装载必须显示加载动画"
        );
        s.wait_for(Duration::from_secs(60), |p| {
            p.current_qn == qn && !p.video_loading
        })
        .map_err(|err| format!("切换画质后未能重新落定:{err}"))?;
    }

    // 热切换 CDN/预加载设置项并切设置页不崩溃。
    s.send(Message::UpdateSettings(AppSettings {
        prefer_official_cdn: false,
        multi_thread_preload: false,
        player_engine: PlayerEngine::Mpv,
        ..AppSettings::default()
    }))?;
    s.sync()?;
    {
        let p = s.probe.borrow();
        assert!(!p.prefer_official_cdn);
        assert!(!p.multi_thread_preload);
    }
    s.send(Message::NavTagSelected(Some("settings".into())))?;
    s.sync()?;
    assert_eq!(s.probe.borrow().route_tag, "settings");
    Ok(())
}

/// 场景 16 #7:悬停预览卡状态机集成 —— 进入 1s 弹出、点「播放」进播放页、
/// 宽限内移入 PeekCard 不收起、快速掠过 10 张卡全程不弹出(全部走真实定时器)。
fn peek(s: &mut Scenario) -> Result<(), String> {
    s.sync()?;
    assert!(!s.probe.borrow().peek_open, "初始不得有预览卡");

    let hover = |bvid: &str, entering: bool| {
        Message::CardHover(HoverReport {
            bvid: bvid.to_string(),
            entering,
            rect: entering.then_some(CardRect {
                x: 240.0,
                y: 300.0,
                w: 220.0,
                h: 160.0,
            }),
        })
    };

    // ① 悬停 1s 后弹出;简介预取与封面数据来自真实首页流。
    s.wait_for(Duration::from_secs(30), |p| p.route_tag == "home" && p.home_videos > 0)
        .map_err(|err| format!("首页流未到货:{err}"))?;
    let bvid = s
        .probe
        .borrow()
        .home_first_bvid
        .clone()
        .ok_or("首页首卡 bvid 缺失")?;
    s.send(hover(&bvid, true))?;
    s.sync()?;
    assert!(!s.probe.borrow().peek_open, "刚进入(<1s)不得立即弹出");
    s.wait_for(Duration::from_secs(4), |p| p.peek_open)
        .map_err(|err| format!("悬停 1s 后未弹出:{err}"))?;
    assert_eq!(
        s.probe.borrow().peek_bvid.as_deref(),
        Some(bvid.as_str()),
        "弹出内容必须是被悬停的稿件"
    );

    // ② 指针移入 PeekCard(撤销宽限)→ 离开 PeekCard 重新起宽限 → 到期收起。
    s.send(Message::PeekPointerEntered)?;
    s.sync()?;
    assert!(s.probe.borrow().peek_open, "移入预览卡不得收起");
    s.send(Message::PeekPointerExited)?;
    s.sync()?;
    assert!(s.probe.borrow().peek_open, "刚离开预览卡仍在 200ms 宽限内");
    std::thread::sleep(Duration::from_millis(450));
    s.sync()?;
    assert!(!s.probe.borrow().peek_open, "宽限到期必须收起");

    // ③ 快速掠过 10 张卡:任何一张都不满 1s,全程不得弹出。
    for i in 0..10u32 {
        let sweep = format!("BV1SWEEP{i}");
        s.send(hover(&sweep, true))?;
        s.sync()?;
        s.send(hover(&sweep, false))?;
        s.sync()?;
    }
    std::thread::sleep(Duration::from_millis(1200));
    s.sync()?;
    assert!(!s.probe.borrow().peek_open, "快速掠过后不得残留弹出");

    // ④ 点「播放」进播放页(等价 UIA Invoke 的 UI 路径)。
    s.send(hover(&bvid, true))?;
    s.wait_for(Duration::from_secs(4), |p| p.peek_open)
        .map_err(|err| format!("二次悬停未弹出:{err}"))?;
    s.send(Message::PeekPlay(bvid.clone()))?;
    s.sync()?;
    assert!(!s.probe.borrow().peek_open, "点播放后预览必须立即收起");
    s.wait_for(Duration::from_secs(60), |p| {
        p.route_tag == "player" && p.now_playing_bvid.as_deref() == Some(bvid.as_str())
    })
    .map_err(|err| format!("预览播放未进播放页:{err}"))?;
    Ok(())
}

const SCENARIOS: &[(&str, &str, fn(&mut Scenario) -> Result<(), String>)] = &[
    ("boot", "FullPlayer", boot),
    ("nav", "FullPlayer", nav),
    ("comments", "FullPlayer", comments),
    ("rapid-switch", "FullPlayer", rapid_switch),
    ("stress", "FullPlayer", stress),
    ("search", "FullPlayer", search),
    ("codec", "FullPlayer", codec),
    ("popular", "FullPlayer", popular),
    ("live", "FullPlayer", live),
    ("unplayable", "FullPlayer", unplayable),
    ("half-pane", "HalfPane", half_pane_pulse),
    ("mobile", "HalfPane", mobile),
    ("fullscreen", "FullPlayer", fullscreen),
    ("engine", "FullPlayer", engine),
    ("loading-state", "FullPlayer", loading_state),
    ("peek", "FullPlayer", peek),
];

/// 解析 `--selftest [scenario]`;非 selftest 调用返回 `None` 走正常启动。
pub fn scenario_from_args() -> Option<String> {
    let mut args = std::env::args().skip(1);
    match args.next()?.as_str() {
        "--selftest" => Some(args.next().unwrap_or_else(|| "all".to_string())),
        _ => None,
    }
}

/// 隔离数据目录:不读不写用户真实凭据(AGENTS.md #7,顺带覆盖访客态路径)。
fn prepare_hermetic_data_dir() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = exe
        .parent()
        .ok_or_else(|| "exe 无父目录".to_string())?
        .join("selftest-data");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    // 2024 edition 的 set_var 为 unsafe;此处在组件挂载前的主线程单次调用,无并发读。
    unsafe { std::env::set_var("QAQ_BX_DATA_DIR", &dir) };
    Ok(())
}

/// 按场景写 settings.json(每次 mount 前调用,AppShell::create 读取)。
/// 除 half-pane 回归场景外一律 FullPlayer——否则用户的 HalfPane 配置会让
/// start_playback 走半窗分支(route 合法地留在 Home),路由断言失效。
fn write_hermetic_settings(home_open_mode: &str) -> Result<(), String> {
    let dir = qaq_bx_core::resolve_data_dir();
    // S4:显式钉 mpv —— 无头 RecordingAdapter 没有 MPE 面,若种子默认 native,
    // OpenVideo 场景会真的启动 MF 装载线程(网络+COM),selftest 就不稳定了。
    // 引擎设置项本身的路径由 `engine` 场景(UpdateSettings + 探针)覆盖。
    std::fs::write(
        dir.join("settings.json"),
        format!(
            r#"{{"quality":"Q1080","codec":"AvcFirst","home_open_mode":"{home_open_mode}","autoplay":true,"hardware_decode":true,"volume":30,"player_engine":"Mpv"}}"#
        ),
    )
    .map_err(|e| e.to_string())
}

/// 场景执行器:独立 panic hook(落盘 crash.log,不弹窗),逐场景隔离挂载。
pub fn run(scenario: &str) {
    if let Err(err) = prepare_hermetic_data_dir() {
        println!("[FAIL] 隔离数据目录准备失败:{err}");
        std::process::exit(1);
    }
    std::panic::set_hook(Box::new(|info| {
        eprintln!("[panic] {info}");
        let log_path = qaq_bx_core::resolve_data_dir().join("crash.log");
        if let Some(parent) = log_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&log_path, format!("QAQ-Bx selftest panic:\n{info}"));
    }));

    let mut failed = 0usize;
    for (name, open_mode, func) in SCENARIOS {
        if scenario != "all" && scenario != *name {
            continue;
        }
        let start = Instant::now();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            write_hermetic_settings(open_mode)?;
            let mut state = Scenario::mount()?;
            func(&mut state)
        }))
        .unwrap_or_else(|payload| {
            let message = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_string()))
                .unwrap_or_else(|| "未知 panic".to_string());
            Err(format!("panic:{message}"))
        });
        let elapsed = start.elapsed().as_secs_f32();
        match outcome {
            Ok(()) => println!("[PASS] {name} ({elapsed:.1}s)"),
            Err(err) => {
                println!("[FAIL] {name} ({elapsed:.1}s):{err}");
                failed += 1;
            }
        }
    }
    if failed == 0 {
        println!("[done] selftest 全部通过");
    } else {
        println!("[done] selftest 失败 {failed} 项");
        std::process::exit(1);
    }
}
