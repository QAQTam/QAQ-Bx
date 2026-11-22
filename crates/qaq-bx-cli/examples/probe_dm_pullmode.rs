//! seg.so `pull_mode` 参数语义对照:同一分段在 pull_mode=1 / 0 / 缺省 下的
//! 覆盖范围(progress min/max)。用于解释「seg#1 只含开头 ~1 分钟」的现象。
//!
//! 用法: cargo run --example probe_dm_pullmode -- <bvid>（缺省 BV1P3sP6UENP）
use std::{sync::Arc, time::Duration};

use qaq_bx_core::auth::AuthPlugin as _;
use qaq_bx_core::credentials::FileCredentialStore;
use qaq_bx_core::danmaku::parse_segment;
use qaq_bx_core::plugin::{PluginContext, PluginRegistry};
use qaq_bx_core::player::PlayerClient;
use qaq_bx_core::wbi::WbiSigner;
use qaq_bx_plugin_login::{CREDENTIALS_FILE, QrAuthPlugin};

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/157.0.0.0 Safari/537.36";

fn main() {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async_main());
}

fn mmss(ms: i32) -> String {
    format!("{:02}:{:02}", ms / 60_000, ms % 60_000 / 1000)
}

async fn fetch(http: &reqwest::Client, wbi: &WbiSigner, creds: &Arc<dyn qaq_bx_core::credentials::CredentialProvider>, oid: u64, pid: u64, seg: u32, pull_mode: Option<&str>) -> anyhow::Result<()> {
    let mut params: Vec<(&str, String)> = vec![
        ("type", "1".into()),
        ("oid", oid.to_string()),
        ("pid", pid.to_string()),
        ("segment_index", seg.to_string()),
    ];
    let tag;
    match pull_mode {
        Some(m) => {
            params.push(("pull_mode", m.to_string()));
            tag = format!("pull_mode={m}");
        }
        None => tag = "pull_mode=<缺省>".into(),
    }
    let url = format!("https://api.bilibili.com/x/v2/dm/wbi/web/seg.so?{}", wbi.sign_query(&params).await?);
    let mut req = http.get(&url).header("Referer", "https://www.bilibili.com/");
    if let Ok(c) = creds.get().await {
        let mut cookie = format!("SESSDATA={}; bili_jct={}; DedeUserID={}", c.sessdata, c.bili_jct, c.dede_user_id);
        if let Some(b) = &c.buvid3 {
            cookie.push_str(&format!("; buvid3={b}"));
        }
        req = req.header("Cookie", cookie);
    }
    let resp = req.send().await?;
    let status = resp.status();
    let bytes = resp.bytes().await?;
    match parse_segment(bytes) {
        Ok(segdata) => {
            let min = segdata.elems().iter().map(|e| e.progress_ms).min().unwrap_or(0);
            let max = segdata.elems().iter().map(|e| e.progress_ms).max().unwrap_or(0);
            println!(
                "seg#{seg} [{tag}] n={} progress=[{},{}]",
                segdata.len(),
                mmss(min),
                mmss(max)
            );
        }
        Err(e) => println!("seg#{seg} [{tag}] HTTP={status} 解析失败: {e}"),
    }
    Ok(())
}

async fn async_main() {
    let bvid = std::env::args().nth(1).unwrap_or_else(|| "BV1P3sP6UENP".into());
    let data_dir = qaq_bx_core::resolve_data_dir();
    let store = Arc::new(FileCredentialStore::new(data_dir.join(CREDENTIALS_FILE)));
    let auth = Arc::new(QrAuthPlugin::new(store));
    let mut registry = PluginRegistry::new();
    registry.register(auth.clone());
    let http = reqwest::Client::builder()
        .user_agent(UA)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap();
    registry
        .init_all(&PluginContext { http: http.clone(), data_dir })
        .await
        .unwrap();

    let player = PlayerClient::new(http.clone(), auth.credentials());
    let (meta, _choice) = player.resolve(&bvid, 116, &["avc1", "hev1", "av01"]).await.unwrap();
    println!("view: aid={} cid={} duration={}s", meta.aid, meta.cid, meta.duration);

    let nav = Arc::new(qaq_bx_core::account::NavClient::new(http.clone(), auth.credentials()));
    let wbi = WbiSigner::with_nav(nav);
    let creds = auth.credentials();
    for seg in [1u32, 2] {
        for pm in [Some("1"), Some("0"), None] {
            if let Err(e) = fetch(&http, &wbi, &creds, meta.cid, meta.aid, seg, pm).await {
                println!("seg#{seg} {pm:?} 失败: {e}");
            }
        }
    }
}
