//! QAQ-Bx CLI —— 插件系统组装层。
//!
//! 子命令：
//!   qaq-bx login    终端扫码登录（二维码 + 轮询）
//!   qaq-bx status   读取凭据并调用 nav 接口验证登录态
//!   qaq-bx logout   登出（清除本地凭据）
//!
//! 这里演示了组装方式：凭据存储 → 登录插件 →（未来）解析插件注入 CredentialProvider。

use std::{sync::Arc, time::Duration};

use anyhow::{bail, Context as _};
use qaq_bx_core::{
    auth::LoginEvent,
    credentials::FileCredentialStore,
    plugin::{PluginContext, PluginRegistry},
};
use qaq_bx_plugin_login::{QrAuthPlugin, CREDENTIALS_FILE};
use qrcode::render::unicode;
use qrcode::QrCode;

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/157.0.0.0 Safari/537.36";

fn main() -> anyhow::Result<()> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("构建 tokio runtime 失败")?
        .block_on(async_main())
}

fn build_http() -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(UA)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(15))
        .build()?)
}

async fn build_registry() -> anyhow::Result<(PluginRegistry, Arc<QrAuthPlugin>)> {
    let data_dir = qaq_bx_core::resolve_data_dir();
    let store = Arc::new(FileCredentialStore::new(data_dir.join(CREDENTIALS_FILE)));
    let auth = Arc::new(QrAuthPlugin::new(store));

    let mut registry = PluginRegistry::new();
    registry.register(auth.clone());

    let http = build_http()?;

    registry
        .init_all(&PluginContext { http, data_dir })
        .await
        .map_err(|e| anyhow::anyhow!("插件初始化失败: {e}"))?;

    Ok((registry, auth))
}

async fn async_main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let cmd = args.next().unwrap_or_else(|| "help".into());

    match cmd.as_str() {
        "login" => cmd_login().await,
        "status" => cmd_status().await,
        "refresh" => cmd_refresh().await,
        "logout" => cmd_logout().await,
        "ticket" => cmd_ticket().await,
        "feed" => cmd_feed().await,
        "playurl" => cmd_playurl(args.next()).await,
        "swrender" => cmd_swrender(args.next()).await,
        "d3drender" => cmd_d3drender(args.next()).await,
        "dm" => cmd_dm(args.next()).await,
        "help" | "--help" | "-h" => {
            print_help();
            Ok(())
        }
        other => {
            print_help();
            bail!("未知子命令: {other}")
        }
    }
}

fn print_help() {
    println!(
        "QAQ-Bx —— 第三方哔哩哔哩客户端（插件式）\n\n\
         用法: qaq-bx <子命令>\n\n\
         子命令:\n  \
         login    扫码登录（终端显示二维码，手机 App 扫码确认）\n  \
         status   查看当前登录态（调用 nav 接口验证凭据）\n  \
         refresh  Cookie 续期（服务端标记后才执行；平时为无操作）\n  \
         logout   登出并清除本地凭据\n  \
         ticket   验证/刷新 bili_ticket（GenWebTicket HMAC-SHA256 + 持久化）\n  \
         feed     拉取推荐流/热门/剧集卡（feed 契约接线验证）\n  \
         playurl  播放地址 DASH 解析与选流（用法: playurl <bvid>）\n  \
         swrender SW 渲染验证（用法: swrender <视频URL或文件> 输出 sw-frame.png）\n  \
         help     显示本帮助"
    );
}

async fn cmd_ticket() -> anyhow::Result<()> {
    use qaq_bx_core::auth::AuthPlugin as _;
    use qaq_bx_core::ticket::{FileTicketStore, TicketManager, TICKET_FILE};

    let (_registry, auth) = build_registry().await?;
    let data_dir = qaq_bx_core::resolve_data_dir();
    let store = Arc::new(FileTicketStore::new(data_dir.join(TICKET_FILE)));
    let manager = TicketManager::new(build_http()?, auth.credentials(), Some(store));
    let ticket = manager.ensure().await.map_err(|e| anyhow::anyhow!("{e}"))?;
    let wbi = manager.wbi_urls().is_some();
    println!(
        "bili_ticket 有效 (len={}, created_at={}, ttl={}s, has_nav_wbi={}) → {}",
        ticket.ticket.len(),
        ticket.created_at,
        ticket.ttl,
        wbi,
        data_dir.join(TICKET_FILE).display()
    );
    Ok(())
}

async fn cmd_login() -> anyhow::Result<()> {
    let (_registry, auth) = build_registry().await?;

    use qaq_bx_core::auth::AuthPlugin as _;
    let mut flow = auth.login().await.map_err(|e| anyhow::anyhow!("{e}"))?;

    // 终端二维码（无第三方图床，内容即 generate 返回的 data.url）
    let code = QrCode::new(flow.qr_content().as_bytes()).context("二维码编码失败")?;
    let qr_text: String = code
        .render::<unicode::Dense1x2>()
        .dark_color(unicode::Dense1x2::Dark)
        .light_color(unicode::Dense1x2::Light)
        .quiet_zone(true)
        .build();
    println!("{qr_text}");
    println!("请用哔哩哔哩 App 扫码并在手机上确认（二维码 ~3 分钟有效）…\n");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
    loop {
        if tokio::time::Instant::now() > deadline {
            bail!("等待超时，请重新运行 login");
        }
        match flow.poll().await.map_err(|e| anyhow::anyhow!("{e}"))? {
            LoginEvent::WaitingScan => {
                println!("[等待扫码] …");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            LoginEvent::Scanned => {
                println!("[已扫码] 请在手机上点击确认登录");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            LoginEvent::Expired => bail!("二维码已失效，请重新运行 login"),
            LoginEvent::Success(creds) => {
                println!(
                    "\n登录成功 ✔  mid={}  凭据已保存",
                    creds.dede_user_id
                );
                return Ok(());
            }
        }
    }
}

async fn cmd_status() -> anyhow::Result<()> {
    let (_registry, auth) = build_registry().await?;

    use qaq_bx_core::auth::AuthPlugin as _;
    let provider = auth.credentials();
    let creds = provider.get().await.map_err(|e| anyhow::anyhow!("{e}"))?;
    println!(
        "本地凭据: mid={} 创建于 {} sessdata={}B{}",
        creds.dede_user_id,
        fmt_unix(creds.created_at),
        creds.sessdata.len(),
        if creds.is_expired() { "（临近过期/已过期）" } else { "" }
    );

    // 用真实接口验证凭据（CDP 逆向中已实测的 nav 端点）
    #[derive(serde::Deserialize)]
    struct NavData {
        #[serde(default, rename = "isLogin")]
        is_login: bool,
        #[serde(default)]
        mid: u64,
        #[serde(default)]
        uname: String,
        #[serde(default)]
        level_info: LevelInfo,
    }
    #[derive(serde::Deserialize, Default)]
    struct LevelInfo {
        #[serde(default)]
        current_level: u8,
    }
    #[derive(serde::Deserialize)]
    struct NavResp {
        code: i64,
        #[serde(default)]
        message: String,
        #[serde(default)]
        data: Option<NavData>,
    }

    let http = reqwest::Client::builder().user_agent(UA).build()?;
    let resp = http
        .get("https://api.bilibili.com/x/web-interface/nav")
        .header(reqwest::header::COOKIE, creds.cookie_header())
        .send()
        .await?;
    let resp: NavResp = resp.json().await?;

    match (resp.code, resp.data) {
        (0, Some(d)) if d.is_login => {
            println!("服务端确认 ✔  已登录: {} (mid={}, Lv{})", d.uname, d.mid, d.level_info.current_level);
            if let Ok(true) = auth.needs_refresh().await {
                println!("⚠️  服务端标记 Cookie 需要续期 —— 运行 `qaq-bx refresh`");
            }
        }
        (0, Some(d)) => println!("服务端返回未登录（凭据可能已失效）mid={}", d.mid),
        (code, _) => bail!("nav 接口错误 {code}: {}", resp.message),
    }
    Ok(())
}

async fn cmd_refresh() -> anyhow::Result<()> {
    let (_registry, auth) = build_registry().await?;

    let need = auth.needs_refresh().await.map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("服务端判定需要续期: {need}");
    if !need {
        println!("凭据仍在 180 天有效期内，无需续期（续期仅在服务端标记后执行）");
        return Ok(());
    }
    let creds = auth.refresh().await.map_err(|e| anyhow::anyhow!("{e}"))?;
    println!(
        "续期完成 ✔  mid={}  新 SESSDATA={}B  refresh_token={}B（已落盘）",
        creds.dede_user_id,
        creds.sessdata.len(),
        creds.refresh_token.as_ref().map_or(0, |t| t.len())
    );
    Ok(())
}

async fn cmd_logout() -> anyhow::Result<()> {
    let (_registry, auth) = build_registry().await?;
    use qaq_bx_core::auth::AuthPlugin as _;
    auth.logout().await.map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("已登出并清除本地凭据");
    Ok(())
}

/// feed 契约接线验证：真实网络拉取推荐流/热门/剧集卡（契约 v1，api/openapi）。
async fn cmd_feed() -> anyhow::Result<()> {
    use qaq_bx_core::auth::AuthPlugin as _;
    let (_registry, auth) = build_registry().await?;

    let http = reqwest::Client::builder()
        .user_agent(UA)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(15))
        .build()?;
    let client = qaq_bx_core::feed::FeedClient::new(http, auth.credentials());

    println!("── 首页推荐 rcmd（WBI 签名，Rust 侧实弹）──");
    let cards = client
        .rcmd(&qaq_bx_core::feed::RcmdQuery {
            ps: Some(12),
            fresh_idx: Some(1),
            fresh_idx_1h: Some(1),
            ..Default::default()
        })
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let av_count = cards.iter().filter(|c| c.is_av()).count();
    println!(
        "code=0 cards={} (av={}, ad/live={})  ← Rust WBI 签名通过",
        cards.len(),
        av_count,
        cards.len() - av_count
    );
    for card in cards.iter().filter(|c| c.is_av()).take(3) {
        println!(
            "  ▶ {} ({})  UP:{}  播放:{}  时长:{}s",
            card.title.as_deref().unwrap_or("?"),
            card.bvid.as_deref().unwrap_or("?"),
            card.owner.as_ref().map_or("?", |o| o.name.as_str()),
            card.stat.as_ref().map_or(0, |s| s.view),
            card.duration.unwrap_or(0),
        );
    }

    println!("\n── 综合热门 popular（无签名）──");
    let list = client.popular(1, 5).await.map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("code=0 list={}", list.len());
    for c in list.iter().take(2) {
        println!("  ▶ {} ({})  播放:{}", c.title, c.bvid, c.stat.view);
    }

    println!("\n── 剧集卡 ogv（season_type=1 番剧）──");
    let seasons = client.ogv(1, 3).await.map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("code=0 items={}", seasons.len());
    for s in seasons.iter().take(2) {
        println!("  ◆ {} (ep{}, ssid{})", s.title, s.epid, s.season_id);
    }
    Ok(())
}

/// playurl 实弹：view → cid → playurl(WBI) → DASH 档位表 + 选流。
/// 顺带把 code=0 响应存档为行为锚（.qaqh/research/samples/playurl-ok.json）。
async fn cmd_playurl(bvid: Option<String>) -> anyhow::Result<()> {
    use qaq_bx_core::auth::AuthPlugin as _;
    use qaq_bx_core::player::PlayerClient;

    let bvid = bvid.unwrap_or_else(|| "BV1GJ411x7h7".into());
    let (_registry, auth) = build_registry().await?;
    let http = reqwest::Client::builder()
        .user_agent(UA)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(15))
        .build()?;
    let client = PlayerClient::new(http, auth.credentials());

    let meta = client.video_meta(&bvid).await.map_err(|e| anyhow::anyhow!("{e}"))?;
    println!(
        "── view code=0 ──\n  {} (aid={})  cid(P1)={}  分P={} 个",
        meta.title,
        meta.aid,
        meta.cid,
        meta.pages.len().max(1)
    );

    let play = client.playurl(&bvid, meta.cid).await.map_err(|e| anyhow::anyhow!("{e}"))?;
    let dash = play
        .dash
        .as_ref()
        .with_context(|| qaq_bx_core::player::dash_missing_reason(&play))?;
    println!(
        "── playurl code=0 fnval=16 ──\n  下发档位={}  accept={:?}",
        play.quality, play.accept_description
    );
    println!("  ── 视频流（{} 条）──", dash.video.len());
    for v in &dash.video {
        println!(
            "    id={:<4} {:<14} {:>4}x{:<4} fps={:<5} {:>5}k  {}",
            v.id,
            v.codecs.split('.').next().unwrap_or("?"),
            v.width,
            v.height,
            v.frame_rate,
            v.bandwidth / 1000,
            v.base_url.split('/').nth(2).unwrap_or("?"),
        );
    }
    println!("  ── 音频流（{} 条）──", dash.audio.len());
    for a in &dash.audio {
        println!("    id={:<6} {:>5}k", a.id, a.bandwidth / 1000);
    }

    // QAQ-Bx 默认策略：1080P60 上限，avc1 优先（RX560 无 AV1 硬解）。
    let choice = qaq_bx_core::player::select_stream(dash, 116, &["avc1", "hev1", "av01"])
        .context("选流失败：无可选流")?;
    println!(
        "── 选中（≤116 avc1 优先）──\n  视频 id={} {} {}x{} fps={} {}k\n  音频 id={} {}k\n  video {}\n  audio {}",
        choice.video.id,
        choice.video.codecs,
        choice.video.width,
        choice.video.height,
        choice.video.frame_rate,
        choice.video.bandwidth / 1000,
        choice.audio.id,
        choice.audio.bandwidth / 1000,
        choice.video.base_url,
        choice.audio.base_url,
    );

    // 行为锚存档：结构化重序列化（字段名与响应一致）。
    let sample = serde_json::json!({
        "url": format!("https://api.bilibili.com/x/player/wbi/playurl?bvid={bvid}&cid={}&qn=0&fnval=16&fnver=0", meta.cid),
        "captured_at": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        "json": { "code": 0, "data": { "quality": play.quality, "accept_description": play.accept_description, "dash": dash } },
    });
    let path = ".qaqh/research/samples/playurl-ok.json";
    std::fs::write(path, serde_json::to_string_pretty(&sample)?)?;
    println!("\n样本已存档 → {path}");
    Ok(())
}

/// 弹幕实弹:view + 全分段 seg.so(WBI)→ 手写 protobuf 解析 → 统计打印。
/// 无参时默认 BV1GJ411x7h7。样本存档 `.qaqh/research/samples/dm-seg1.bin`。
async fn cmd_dm(bvid: Option<String>) -> anyhow::Result<()> {
    use qaq_bx_core::auth::AuthPlugin as _;
    use qaq_bx_core::danmaku::DanmakuClient;
    use qaq_bx_core::player::PlayerClient;

    let bvid = bvid.unwrap_or_else(|| "BV1GJ411x7h7".into());
    let (_registry, auth) = build_registry().await?;
    let http = reqwest::Client::builder()
        .user_agent(UA)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(15))
        .build()?;
    let client = PlayerClient::new(http.clone(), auth.credentials());
    let (meta, _choice) = client
        .resolve(&bvid, 116, &["avc1", "hev1", "av01"])
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    println!(
        "── view:id={} aid={} cid={} duration={}s ──",
        meta.bvid, meta.aid, meta.cid, meta.duration
    );

    let dm = DanmakuClient::new(http, auth.credentials());
    let segs = dm
        .all(meta.cid, meta.aid, u64::try_from(meta.duration).unwrap_or(0))
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let total: usize = segs.iter().map(|s| s.len()).sum();
    println!("解析得到 {total} 条弹幕");
    for (seg, elem) in segs
        .iter()
        .flat_map(|s| s.elems().iter().map(move |e| (s, e)))
        .take(10)
    {
        println!(
            "  [{:02}:{:02}] mode={} fs={} #{:06X} {}",
            elem.progress_ms / 60_000,
            elem.progress_ms % 60_000 / 1000,
            elem.mode,
            elem.fontsize,
            elem.color & 0xFF_FF_FF,
            seg.content(elem)
        );
    }
    if let Some((seg, first)) = segs.iter().find_map(|s| s.elems().first().map(|e| (s, e))) {
        std::fs::write(
            ".qaqh/research/samples/dm-seg1.bin",
            format!("{}\n", seg.content(first)),
        )?;
    }
    println!("首条内容已存档 → .qaqh/research/samples/dm-seg1.bin");
    Ok(())
}

/// SW 渲染通路验证:loadfile → mpv_render_context_render(SW)→ 帧缓冲 → PNG。
/// (证明 mpv 解码 + SW 渲染管线;SwapChainPanel 挂载在其后的 UI sample。)
/// 无参时自动走 view+playurl+选流(B 站实弹)。
async fn cmd_swrender(source: Option<String>) -> anyhow::Result<()> {
    use qaq_bx_core::auth::AuthPlugin as _;
    use qaq_bx_core::player::PlayerClient;
    use qaq_bx_video::ffi::{MpvLib, MPV_FORMAT_DOUBLE};
    use qaq_bx_video::player::MpvPlayer;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    let url = match source {
        Some(s) => s,
        None => {
            let (_registry, auth) = build_registry().await?;
            let http = reqwest::Client::builder()
                .user_agent(UA)
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(15))
                .build()?;
            let client = PlayerClient::new(http, auth.credentials());
            let choice = client
                .resolve("BV1GJ411x7h7", 116, &["avc1", "hev1", "av01"])
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?
                .1;
            println!(
                "── playurl 选流:id={} {} {}x{} fps={} ──",
                choice.video.id,
                choice.video.codecs,
                choice.video.width,
                choice.video.height,
                choice.video.frame_rate,
            );
            choice.video.base_url
        }
    };

    let lib = MpvLib::load().map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut player = MpvPlayer::new(lib)?;
    player.set_options(&[
        ("hwdec", "no"),
        ("ao", "null"),
        ("terminal", "no"),
        ("msg-level", "all=info"),
    ])?;
    player.create_render_context_sw()?;

    let (w, h) = (640u32, 360u32);
    let mut buf = vec![0u8; (w as usize) * (h as usize) * 4];

    // time-pos 观察:事件线程累计(渲染循环的进度信号)。
    let time_pos = Arc::new(AtomicU64::new(0)); // 毫秒
    {
        let tp = time_pos.clone();
        player.observe_property(1, "time-pos", MPV_FORMAT_DOUBLE)?;
        player.observe_property(2, "eof-reached", qaq_bx_video::ffi::MPV_FORMAT_FLAG)?;
        player.start_event_thread(Arc::new(move |event| {
            if let qaq_bx_video::player::MpvEvent::PropertyChange { name, value } = event {
                if name == "time-pos" && let qaq_bx_video::player::PropertyValue::Double(s) = value {
                    tp.store((s * 1000.0) as u64, Ordering::Relaxed);
                }
            }
        }))?;
    }

    player.loadfile(&url, None, &[
        ("Referer", "https://www.bilibili.com/"),
        ("User-Agent", UA),
    ])?;

    // 渲染循环:最长 10s,等 time-pos > 500ms 后再多渲几帧保存。
    let start = Instant::now();
    let mut frames = 0usize;
    let mut ready_at: Option<Instant> = None;
    loop {
        player.render_sw(w, h, &mut buf)?;
        frames += 1;
        if time_pos.load(Ordering::Relaxed) > 500 && ready_at.is_none() {
            ready_at = Some(Instant::now());
        }
        if ready_at.is_some_and(|t| t.elapsed() > Duration::from_millis(400))
            || start.elapsed() > Duration::from_secs(10)
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(30));
    }

    image::save_buffer(
        "sw-frame.png",
        &buf,
        w,
        h,
        image::ColorType::Rgba8,
    )
    .map_err(|e| anyhow::anyhow!("PNG 保存失败: {e}"))?;
    let non_black = buf.chunks_exact(4).filter(|p| p[0] + p[1] + p[2] > 30).count();
    println!(
        "SW 渲染完成: frames={frames} time-pos={}ms 非黑像素={}/{} → sw-frame.png",
        time_pos.load(Ordering::Relaxed),
        non_black,
        (w * h) as usize,
    );
    Ok(())
}

/// 简易 unix 时间戳展示（避免引入 chrono；后续可换成统一时间工具）。
fn fmt_unix(unix: i64) -> String {
    format!("{unix}")
}

/// D3D11 渲染通路验证:device + composition swapchain + mpv D3D11 render → 回读 PNG。
/// (composition swapchain 无需窗口,与 SwapChainPanel 挂载同一机制。)
async fn cmd_d3drender(source: Option<String>) -> anyhow::Result<()> {
    use qaq_bx_core::auth::AuthPlugin as _;
    use qaq_bx_core::player::PlayerClient;
    use qaq_bx_video::ffi::{mpv_d3d11_fbo, mpv_d3d11_init_params, MpvLib};
    use qaq_bx_video::player::MpvPlayer;
    use std::time::{Duration, Instant};
    use windows::Win32::d3dcommon::{
        D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
    };
    use windows::Win32::d3d11::{
        D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
        D3D11_CREATE_DEVICE_BGRA_SUPPORT,
        D3D11_MAP_READ, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
        D3D11_USAGE_STAGING, D3D11_CPU_ACCESS_READ,
    };
    use windows::Win32::dxgi::{
        DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
        IDXGIFactory2, IDXGISwapChain1, DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1,
        DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL, DXGI_USAGE_RENDER_TARGET_OUTPUT,
    };

    // 1) playurl 取流(无参时)
    let url = match source {
        Some(s) => s,
        None => {
            let (_registry, auth) = build_registry().await?;
            let http = reqwest::Client::builder()
                .user_agent(UA)
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(15))
                .build()?;
            let client = PlayerClient::new(http, auth.credentials());
            let choice = client
                .resolve("BV1GJ411x7h7", 116, &["avc1", "hev1", "av01"])
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?
                .1;
            println!(
                "── playurl 选流:id={} {} {}x{} fps={} ──",
                choice.video.id, choice.video.codecs, choice.video.width,
                choice.video.height, choice.video.frame_rate,
            );
            choice.video.base_url
        }
    };

    // 2) D3D11 device(BGRA 支持,非 SINGLETHREADED → d3d11va 可用)
    unsafe {
        let mut device: Option<ID3D11Device> = None;
        let mut context: Option<ID3D11DeviceContext> = None;
        let feature_levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            Default::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT as u32,
            Some(feature_levels.as_ptr()),
            feature_levels.len() as u32,
            D3D11_SDK_VERSION as u32,
            Some(&mut device as *mut _),
            None,
            Some(&mut context as *mut _),
        )
        .ok()?;
        let device = device.context("D3D11CreateDevice 失败")?;
        let context = context.context("无 immediate context")?;
        println!("D3D11 device OK (feature level OK)");

        // 3) composition swapchain(flip,BGRA,2 buffer)
        // adapter.GetParent 只回 Factory1;直接建 Factory2(FLIP swapchain 需要)。
        let factory: IDXGIFactory2 =
            windows::Win32::dxgi::CreateDXGIFactory1()
                .map_err(|e| anyhow::anyhow!("CreateDXGIFactory1 失败: {e}"))?;
        println!("GetParent factory OK");
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: 1280,
            Height: 720,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
            Scaling: DXGI_SCALING_STRETCH,
            ..Default::default()
        };
        let swapchain: IDXGISwapChain1 = factory.CreateSwapChainForComposition(&device, &desc, None)?;
        println!("composition swapchain OK (1280x720 BGRA flip x2)");

        // 4) mpv + D3D11 render context
        let lib = MpvLib::load().map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut player = MpvPlayer::new(lib)?;
        player.set_options(&[
            ("hwdec", "d3d11va"),
            ("ao", "null"),
            ("terminal", "no"),
        ])?;
        player.create_render_context_d3d11(&mpv_d3d11_init_params {
            device: windows::core::Interface::as_raw(&device),
        })?;
        println!("mpv D3D11 render context OK");
        player.loadfile(&url, None, &[
            ("Referer", "https://www.bilibili.com/"),
            ("User-Agent", UA),
        ])?;

        // 5) 渲染循环:backbuffer → mpv render → Present
        let backbuffer: ID3D11Texture2D = swapchain.GetBuffer(0)?;
        let fbo = mpv_d3d11_fbo {
            tex: windows::core::Interface::as_raw(&backbuffer),
        };
        let start = Instant::now();
        let mut presents = 0usize;
        while start.elapsed() < Duration::from_secs(6) {
            player.render_d3d11(&fbo)?;
            swapchain.Present(1, 0).ok()?;
            presents += 1;
            std::thread::sleep(Duration::from_millis(40));
        }
        println!("渲染循环: presents={presents}");

        // 6) 回读 staging → PNG
        let mut bd = D3D11_TEXTURE2D_DESC::default();
        backbuffer.GetDesc(&mut bd);
        let sd = D3D11_TEXTURE2D_DESC {
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ as u32,
            MipLevels: 1,
            ArraySize: 1,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            MiscFlags: 0,
            ..bd
        };
        let mut staging_opt: Option<ID3D11Texture2D> = None;
        device
            .CreateTexture2D(&sd, None, Some(&mut staging_opt as *mut _))
            .ok()?;
        let staging: ID3D11Texture2D = staging_opt.context("CreateTexture2D 失败")?;
        context.CopyResource(&staging, &backbuffer);
        let mut mapped = windows::Win32::d3d11::D3D11_MAPPED_SUBRESOURCE::default();
        context
            .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped as *mut _))
            .ok()?;
        let mapped = mapped;
        let mut rgba = vec![0u8; (bd.Width as usize) * (bd.Height as usize) * 4];
        for row in 0..bd.Height as usize {
            let src = mapped.pData.add(row * mapped.RowPitch as usize) as *const u8;
            let dst = &mut rgba[row * bd.Width as usize * 4..(row + 1) * bd.Width as usize * 4];
            std::ptr::copy_nonoverlapping(src, dst.as_mut_ptr(), bd.Width as usize * 4);
        }
        context.Unmap(&staging, 0);
        image::save_buffer("d3d-frame.png", &rgba, bd.Width, bd.Height, image::ColorType::Rgba8)
            .map_err(|e| anyhow::anyhow!("PNG 失败: {e}"))?;
        let non_black = rgba.chunks_exact(4).filter(|p| p[0].max(p[1]).max(p[2]) > 30).count();
        println!(
            "D3D11 渲染完成: 非黑像素={}/{} → d3d-frame.png",
            non_black,
            (bd.Width * bd.Height) as usize,
        );
    }
    Ok(())
}
