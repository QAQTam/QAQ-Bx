//! 弹幕 progress 字段语义探测：同一视频逐段拉 seg.so,打印每段
//! progress min/max —— 若第 2 段起 min ≥ 360000 即「全片绝对毫秒」,
//! 若每段都落在 0..360000 即「段相对毫秒」（渲染侧对齐口径的决定性依据）。
//!
//! 用法: cargo run --example probe_dm_progress -- <bvid>（缺省 BV1P3sP6UENP,2872s）
use std::{sync::Arc, time::Duration};

use qaq_bx_core::auth::AuthPlugin as _;
use qaq_bx_core::danmaku::DanmakuClient;
use qaq_bx_core::credentials::FileCredentialStore;
use qaq_bx_core::plugin::{PluginContext, PluginRegistry};
use qaq_bx_core::player::PlayerClient;
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
    let (meta, _choice) = player
        .resolve(&bvid, 116, &["avc1", "hev1", "av01"])
        .await
        .unwrap();
    println!(
        "view: aid={} cid={} duration={}s → {} 段",
        meta.aid,
        meta.cid,
        meta.duration,
        (meta.duration + 359) / 360
    );

    let dm = DanmakuClient::new(http, auth.credentials());
    let segs = dm
        .all(meta.cid, meta.aid, u64::try_from(meta.duration).unwrap_or(0))
        .await
        .unwrap();
    for (i, seg) in segs.iter().enumerate() {
        let Some(first) = seg.elems().first() else {
            println!("seg#{} 空", i + 1);
            continue;
        };
        let min = seg.elems().iter().map(|e| e.progress_ms).min().unwrap();
        let max = seg.elems().iter().map(|e| e.progress_ms).max().unwrap();
        println!(
            "seg#{} n={} progress=[{},{}] 采样:[{} \"{}\"] [{} \"{}\"]",
            i + 1,
            seg.len(),
            mmss(min),
            mmss(max),
            mmss(first.progress_ms),
            seg.content(first),
            mmss(seg.elems()[seg.len() / 2].progress_ms),
            seg.content(&seg.elems()[seg.len() / 2]),
        );
    }
}
