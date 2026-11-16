//! 内部状态自测(reactor `test` 模式):不开窗口、不模拟鼠标,直接挂载
//! AppShell 组件树并派发 Message,断言壳层状态。覆盖「鼠标模拟测不到」
//! 的路径:快速切视频的播放代次取消、评论双桶缓存、导航历史栈。
//!
//! 用法:`bili-ui.exe --selftest [all|boot|nav|comments|rapid-switch]`
//! 前置:`cargo build -p bili-ui --features selftest`;需要真实网络与
//! 本地凭据(nav 探测 / feed / 评论均走真实接口)。

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};
use windows_reactor::{component, test::{ComponentHost, RecordingAdapter}};

use crate::pages::player::CommentSort;
use crate::shell::{AppShell, Message, ShellInput, ShellProbe};

/// 占位种子数据中可解析真实媒体的测试稿件(`data::find` 直达,不依赖随机 feed)。
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
        p.now_playing_bvid.as_deref() == Some(TEST_BV)
            && p.hot_loaded
            && p.hot_replies > 0
    })
    .map_err(|err| format!("评论首页未就绪:{err}"))?;
    {
        let probe = s.probe.borrow();
        assert_eq!(probe.comments_aid, TEST_AID, "评论会话 aid 应为真实稿件");
        assert!(!probe.comments_sort_latest, "默认排序应为按热度");
        assert!(probe.hot_total.unwrap_or(0) > 0, "总评论数应 > 0");
    }
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
    assert_eq!(s.probe.borrow().route_tag, "home", "播放页 Back 应回首页");
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

const SCENARIOS: &[(&str, fn(&mut Scenario) -> Result<(), String>)] = &[
    ("boot", boot),
    ("nav", nav),
    ("comments", comments),
    ("rapid-switch", rapid_switch),
];

/// 解析 `--selftest [scenario]`;非 selftest 调用返回 `None` 走正常启动。
pub fn scenario_from_args() -> Option<String> {
    let mut args = std::env::args().skip(1);
    match args.next()?.as_str() {
        "--selftest" => Some(args.next().unwrap_or_else(|| "all".to_string())),
        _ => None,
    }
}

/// 隔离数据目录:不读不写用户真实凭据(AGENTS.md #7,顺带覆盖访客态路径),
/// settings 固定 FullPlayer——否则用户的 HalfPane 配置会让 start_playback
/// 走半窗分支(route 合法地留在 Home),路由断言失效。
fn prepare_hermetic_data_dir() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = exe
        .parent()
        .ok_or_else(|| "exe 无父目录".to_string())?
        .join("selftest-data");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(
        dir.join("settings.json"),
        r#"{"quality":"Q1080","codec":"AvcFirst","home_open_mode":"FullPlayer","autoplay":true,"hardware_decode":true,"volume":30}"#,
    )
    .map_err(|e| e.to_string())?;
    // 2024 edition 的 set_var 为 unsafe;此处在组件挂载前的主线程单次调用,无并发读。
    unsafe { std::env::set_var("BILIPLAY_DATA_DIR", &dir) };
    Ok(())
}

/// 场景执行器:独立 panic hook(落盘 crash.log,不弹窗),逐场景隔离挂载。
pub fn run(scenario: &str) {
    if let Err(err) = prepare_hermetic_data_dir() {
        println!("[FAIL] 隔离数据目录准备失败:{err}");
        std::process::exit(1);
    }
    std::panic::set_hook(Box::new(|info| {
        eprintln!("[panic] {info}");
        let log_path = bili_core::resolve_data_dir().join("crash.log");
        if let Some(parent) = log_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&log_path, format!("QAQ-Bx selftest panic:\n{info}"));
    }));

    let mut failed = 0usize;
    for (name, func) in SCENARIOS {
        if scenario != "all" && scenario != *name {
            continue;
        }
        let start = Instant::now();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
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
