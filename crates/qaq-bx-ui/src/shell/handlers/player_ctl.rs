//! 播放器控制域:开播入口(OpenVideo/OpenVideoTarget)、开关/进度/音量/静音/弹幕开关、
//! 全屏三态(Enter/Exit/Toggle + Synced/Probe 探测回环)、半窗三态(Expand/Close/Resize)、
//! 引擎切换(switch_player_engine/fallback_native_to_mpv)、相关推荐(RelatedLoaded/Retry)、
//! 停播与可见性仲裁(stop_playback/reconcile_playback_visibility)。
//!
//! 本文件由 P3 拆解生成:match 臂与配套方法自原 update()/impl AppShell 逐字搬运,
//! 消息路由见 handlers/mod.rs(穷尽 match)。单元测试锚点见 shell/tests.rs。

use super::super::*;

impl AppShell {
    /// player_ctl 域消息臂(23 变体,与 message.rs 分组注释对应);未命中变体静默忽略。
    pub(super) fn handle_player_ctl(&mut self, message: Message, context: &ComponentContext<Self>) -> bool {
        match message {
            Message::RelatedLoaded { seq, bvid, res } => {
                self.absorb_related(seq, bvid, res, context);
            }
            Message::RelatedRetry => {
                if let Some(item) = self.now_playing.clone() {
                    self.fetch_related(self.playback_gen, item.bvid, context);
                }
            }
            Message::OpenVideo(bvid) => {
                self.open_video(&bvid, context);
                self.sync_smtc(context);
            }
            Message::OpenVideoTarget(target) => {
                self.open_video_target(target, context);
                self.sync_smtc(context);
            }
            Message::TogglePlay => {
                let want_play = !self.playing;
                self.apply_playback_want(want_play, context);
                self.sync_smtc(context);
            }
            Message::Seek(value) => {
                self.position = value.round().clamp(0.0, f64::from(u32::MAX)) as u32;
                if let Some(player) = self.native_sink() {
                    let _ = player.seek(value.max(0.0));
                    return true;
                }
                if let Some(engine) = &self.engine {
                    engine.seek(value.max(0.0));
                }
            }
            Message::SetVolume(value) => {
                let vol = value.round().clamp(0.0, 100.0) as u32;
                if vol > 0 {
                    self.volume_before_mute = vol;
                }
                if vol != self.volume {
                    self.volume = vol;
                    if let Some(player) = self.native_sink() {
                        let _ = player.set_volume(f64::from(self.volume));
                    } else if let Some(engine) = &self.engine {
                        engine.set_volume(f64::from(self.volume));
                    }
                }
                if self.settings.volume != vol {
                    self.settings.volume = vol;
                    self.schedule_settings_save(context);
                }
            }
            Message::ToggleMute => {
                let target = if self.volume == 0 {
                    self.volume_before_mute.max(10)
                } else {
                    self.volume_before_mute = self.volume;
                    0
                };
                self.volume = target;
                if let Some(player) = self.native_sink() {
                    let _ = player.set_volume(f64::from(target));
                } else if let Some(engine) = &self.engine {
                    engine.set_volume(f64::from(target));
                }
                if self.settings.volume != target {
                    self.settings.volume = target;
                    self.schedule_settings_save(context);
                }
            }
            Message::ToggleDanmaku => {
                self.settings.danmaku_enabled = !self.settings.danmaku_enabled;
                self.danmaku_config(
                    self.settings.danmaku_enabled,
                    self.settings.danmaku_opacity,
                    self.settings.danmaku_max_lanes,
                    &self.settings.danmaku_font_family,
                    self.settings.danmaku_font_scale,
                );
                self.schedule_settings_save(context);
            }
            Message::EnterFullscreen => {
                self.enter_fullscreen(context);
            }
            Message::ExitFullscreen => {
                self.exit_fullscreen(context);
            }
            Message::ToggleFullscreen => {
                if self.is_fullscreen {
                    self.exit_fullscreen(context);
                } else {
                    self.enter_fullscreen(context);
                }
            }
            Message::FullscreenSynced { gen_id, active } => {
                if gen_id == self.fullscreen_gen {
                    self.fullscreen_sync_in_flight = false;
                    if !active {
                        if self.is_fullscreen {
                            self.clear_native_remount_throttle();
                        }
                        self.is_fullscreen = false;
                    }
                }
            }
            Message::FullscreenProbeResult { gen_id, active } => {
                if gen_id == self.fullscreen_gen && self.is_fullscreen && !active {
                    self.clear_native_remount_throttle();
                    self.is_fullscreen = false;
                }
            }
            Message::PlayerHotkey(hotkey) => match hotkey {
                pages::fullscreen::PlayerHotkey::ToggleFullscreen => {
                    if self.is_fullscreen {
                        self.exit_fullscreen(context);
                    } else {
                        self.enter_fullscreen(context);
                    }
                }
                pages::fullscreen::PlayerHotkey::ExitFullscreen => {
                    self.exit_fullscreen(context);
                }
                pages::fullscreen::PlayerHotkey::TogglePlay => {
                    self.update(Message::TogglePlay, context);
                }
                pages::fullscreen::PlayerHotkey::SeekDelta(delta) => {
                    if self.is_player_visible() {
                        let max_secs = self
                            .now_playing
                            .as_ref()
                            .map(|item| self.display_duration(item))
                            .unwrap_or(0);
                        let next = if delta < 0 {
                            self.position.saturating_sub(delta.unsigned_abs())
                        } else {
                            self.position.saturating_add(delta as u32)
                        };
                        let clamped = if max_secs > 0 {
                            next.min(max_secs)
                        } else {
                            next
                        };
                        self.update(Message::Seek(f64::from(clamped)), context);
                    }
                }
                pages::fullscreen::PlayerHotkey::VolumeDelta(delta) => {
                    if self.is_player_visible() {
                        let next = if delta < 0 {
                            self.volume.saturating_sub(delta.unsigned_abs())
                        } else {
                            (self.volume + delta as u32).min(100)
                        };
                        self.update(Message::SetVolume(f64::from(next)), context);
                    }
                }
                pages::fullscreen::PlayerHotkey::ToggleMute => {
                    if self.is_player_visible() {
                        self.update(Message::ToggleMute, context);
                    }
                }
                pages::fullscreen::PlayerHotkey::ToggleDanmaku => {
                    if self.is_player_visible() {
                        self.update(Message::ToggleDanmaku, context);
                    }
                }
            },
            Message::ExpandHalfPane => {
                self.home_half_pane = false;
                if self.route != Route::Player {
                    self.history.push(self.route);
                    self.route = Route::Player;
                    crate::trace::note("route->player (半屏展开)");
                }
            }
            Message::CloseHalfPane => {
                self.stop_playback(context);
            }
            Message::ResizeHalfPane(width) => {
                let clamped = width.clamp(
                    pages::player::HALF_PANE_MIN_WIDTH,
                    pages::player::HALF_PANE_MAX_WIDTH,
                );
                if (self.settings.half_pane_width - clamped).abs() >= 1.0 {
                    self.settings.half_pane_width = clamped;
                    self.schedule_settings_save(context);
                }
            }
            _ => {}
        }
        false
    }

    pub(in crate::shell) fn ensure_engine(&mut self) -> Arc<VideoEngine> {
        if let Some(engine) = &self.engine {
            return engine.clone();
        }
        let engine = VideoEngine::with_config(video_config_from(&self.settings));
        self.engine = Some(engine.clone());
        engine
    }


    /// M7:当前**生效**播放引擎(env 强制覆盖 > 设置项)。
    pub(in crate::shell) fn effective_player_engine(&self) -> PlayerEngine {
        PlayerEngine::resolve(self.engine_env_override, self.settings.player_engine)
    }


    /// M7:生效引擎是否为系统媒体引擎。直播装载不看它(见 `start_live_playback`)。
    pub(in crate::shell) fn native_enabled(&self) -> bool {
        self.effective_player_engine() == PlayerEngine::Native
    }


    /// M7:正在播的是否为直播间(FLV;唯一不受播放引擎设置影响的通路)。
    pub(in crate::shell) fn current_is_live(&self) -> bool {
        self.now_playing
            .as_ref()
            .is_some_and(|item| item.bvid.starts_with(LIVE_BVID_PREFIX))
    }


    /// 播放页画质/分P 换流时 native 是否接管(与装载入口同一闸)。
    pub(in crate::shell) fn native_active(&self) -> bool {
        self.native_enabled()
    }


    /// S3:播放引擎设置变更的收口(常驻播放层落地后当次生效)。
    /// mpv→native:mpv 静音停解码(引擎对象保留作回退),系统媒体引擎按当前
    /// 进度定点续播;native→mpv:MPE 源卸下,libmpv 同进度接管,透明层/SMTC
    /// 写权随之交回。直播(FLV,MF 不支持)即使设成 native 也永不接管。
    /// 没有可切换会话(未开播/解析在途)只记账,下一次装载按新引擎走。
    pub(in crate::shell) fn switch_player_engine(&mut self, context: &ComponentContext<Self>) {
        let to_native = self.native_enabled();
        let target = self.effective_player_engine().tag();
        let has_session =
            self.now_playing.is_some() && self.current_qn != 0 && self.playback_dash.is_some();
        match engine_switch_plan(to_native, self.current_is_live(), has_session) {
            EngineSwitchAction::RecordOnly => {
                if !to_native {
                    self.teardown_native_side(context);
                }
                eprintln!("[qaq-bx-ui] player engine -> {target}(记账完成,下一次装载生效)");
            }
            EngineSwitchAction::LiveKeepMpv => {
                self.teardown_native_side(context);
                eprintln!("[qaq-bx-ui] player engine -> {target}(直播恒 mpv,不接管当前流)");
            }
            EngineSwitchAction::ReloadNative => {
                let Some(choice) = self.current_choice() else {
                    eprintln!("[qaq-bx-ui] player engine: {target}(当前档位无可选分轨)");
                    return;
                };
                let start = self
                    .engine
                    .as_ref()
                    .map(|engine| engine.status().time_pos)
                    .unwrap_or(0.0)
                    .max(f64::from(self.position));
                let want_play = self.playing;
                eprintln!(
                    "[qaq-bx-ui] player engine switch: mpv -> {target} resume={start:.1} play={want_play}"
                );
                self.video_loading = true;
                if self.native_load_choice(&choice, want_play, start) {
                    self.current_video_url = choice.video.base_url;
                    self.current_audio_url = choice.audio.base_url;
                    self.player_notice = None;
                    self.reseed_danmaku(start, context);
                    self.sync_smtc(context);
                } else {
                    self.mpv_load_choice(&choice, start, want_play, context);
                }
            }
            EngineSwitchAction::ReloadMpv => {
                self.switch_to_mpv_now(context);
            }
        }
    }


    /// S4:native 通路解码失败 → 自动回退 mpv(同分轨同进度接管)。
    /// 会话级按 codec 桶记忆;用户设置项不动(装上系统扩展后下次启动即恢复 native)。
    pub(in crate::shell) fn fallback_native_to_mpv(&mut self, reason: &str, context: &ComponentContext<Self>) {
        let bucket = self
            .playback_dash
            .as_ref()
            .and_then(|dash| {
                dash.video
                    .iter()
                    .find(|v| v.base_url == self.current_video_url)
            })
            .map(|v| v.codecs.split('.').next().unwrap_or_default().to_string())
            .unwrap_or_default();
        if !bucket.is_empty() && !self.native_fallback_codecs.contains(&bucket) {
            self.native_fallback_codecs.push(bucket.clone());
            eprintln!(
                "[qaq-bx-ui] native fallback: codec {bucket} 解码失败,记入本会话回退名单"
            );
        }
        eprintln!("[qaq-bx-ui] native fallback -> mpv: {reason}");
        if !self.switch_to_mpv_now(context) {
            // Bug 2A：仅当无可用分轨导致 mpv 也无法接管时才设 player_notice（否则会卸载 VideoSurface 只剩音频）。
            self.player_notice = Some(format!("系统媒体引擎解码失败({reason})"));
            self.playing = false;
            self.video_loading = false;
            self.media_load_pending = false;
        }
    }


    /// S3/S4 共享:native → mpv 立即接管(同分轨同进度;常驻播放层里换 surface 分支)。
    pub(in crate::shell) fn switch_to_mpv_now(&mut self, context: &ComponentContext<Self>) -> bool {
        let target = self.effective_player_engine().tag();
        let Some(choice) = self.current_choice() else {
            eprintln!("[qaq-bx-ui] player engine: {target}(当前档位无可选分轨)");
            self.teardown_native_side(context);
            return false;
        };
        let start = self.native_position().max(f64::from(self.position));
        let want_play = self.playing;
        eprintln!(
            "[qaq-bx-ui] player engine switch: native -> {target} resume={start:.1} play={want_play}"
        );
        self.mpv_load_choice(&choice, start, want_play, context);
        self.current_video_url = choice.video.base_url;
        self.current_audio_url = choice.audio.base_url;
        self.media_probe_at = Some(Instant::now());
        self.failed_video_prefixes.clear();
        self.video_fallback_done = false;
        self.audio_fallback_done = false;
        self.player_notice = None;
        self.video_loading = true;
        self.playing = want_play;
        self.reseed_danmaku(start, context);
        self.sync_smtc(context);
        true
    }


    /// M7:把 native 通路的附属资源整体下线(切回 mpv / 直播在播时共用)。
    pub(in crate::shell) fn teardown_native_side(&mut self, context: &ComponentContext<Self>) {
        if let Some(player) = self.native_player.clone() {
            let _ = player.stop();
        }
        self.native_playback = false;
        self.native_load_epoch = None;
        self.native_video_probe_at = None;
        self.native_stall_probe_at = None;
        self.release_stage();
        self.reset_smtc_writer(false);
        self.sync_smtc(context);
    }


    /// 相关推荐拉取：随播放代次发起，旧 bvid 的迟到响应在吸收端双校验丢弃。
    pub(in crate::shell) fn fetch_related(&mut self, seq: u64, bvid: String, context: &ComponentContext<Self>) {
        self.related = pages::player::RelatedPane {
            bvid: bvid.clone(),
            loading: true,
            // F3:换代 +1;absorb/封面回填同契约,PlayerPage 据此重建相关虚拟源。
            rev: self.related.rev.wrapping_add(1),
            ..pages::player::RelatedPane::default()
        };
        let client = self.feed_client.clone();
        let cache = self.image_cache.clone();
        context.spawn_background(move |_| {
            let res = block_on(client.related(&bvid)).map_err(|e| e.to_string());
            let items = match &res {
                Ok(cards) => cards
                    .iter()
                    .map(|card| {
                        let mut item = archive_video_item(card);
                        if let Some(url) = &item.pic_url {
                            item.cover = cache.get(url);
                        }
                        item
                    })
                    .collect(),
                Err(_) => Vec::new(),
            };
            Message::RelatedLoaded {
                seq,
                bvid,
                res: match res {
                    Ok(_) => Ok(items),
                    Err(err) => Err(err),
                },
            }
        });
    }


    pub(in crate::shell) fn absorb_related(
        &mut self,
        seq: u64,
        bvid: String,
        res: Result<Vec<VideoItem>, String>,
        context: &ComponentContext<Self>,
    ) {
        // 代次 + 目标双校验：快速切视频时迟到的旧响应一律丢弃。
        if seq != self.playback_gen || self.related.bvid != bvid {
            return;
        }
        self.related.loading = false;
        // F3:换代 +1(loading 翻转与 error 路径共同覆盖)。
        self.related.rev = self.related.rev.wrapping_add(1);
        let items = match res {
            Ok(items) => items,
            Err(err) => {
                self.related.error = Some(err);
                return;
            }
        };
        for mut item in items {
            // 接口不保证剔除当前稿件，防御性过滤自身。
            if item.bvid == self.related.bvid {
                continue;
            }
            if item.cover.is_none()
                && let Some(url) = &item.pic_url
            {
                if let Some(cached) = self.image_cache.get(url) {
                    item.cover = Some(cached);
                } else {
                    self.spawn_cover_fetch(item.bvid.clone(), url.clone(), self.feed_gen, context);
                }
            }
            self.related_index.insert(item.bvid.clone(), item.clone());
            self.related.items.push(item);
        }
        self.related.loaded_once = true;
        self.related.rev = self.related.rev.wrapping_add(1);
    }


    pub(in crate::shell) fn is_player_visible(&self) -> bool {
        self.route == Route::Player
            || (self.route == Route::Home && self.home_half_pane)
            || (self.is_fullscreen && self.now_playing.is_some())
    }


    pub(in crate::shell) fn can_enter_fullscreen(&self) -> bool {
        self.now_playing.is_some()
            && self.engine.is_some()
            && (self.route == Route::Player || (self.route == Route::Home && self.home_half_pane))
    }


    pub(in crate::shell) fn sync_window_fullscreen(&mut self, fullscreen: bool, context: &ComponentContext<Self>) {
        self.fullscreen_gen = self.fullscreen_gen.wrapping_add(1);
        let gen_id = self.fullscreen_gen;
        let portrait = self.settings.portrait_mode;
        let current_dips = self.viewport_layout.window_width;
        let started = context.run_window(move |window| {
            let raw = window.as_raw();
            crate::winrt_ext::set_window_fullscreen(raw, fullscreen);
            // 全屏 presenter 与 OverlappedPresenter 的最大化开关/宽度约束正交,但退出回
            // Default presenter 时新 presenter 的设置可能复位——进出都重按一次(幂等)。
            if portrait {
                crate::winrt_ext::set_maximizable(raw, false);
                if !fullscreen {
                    crate::winrt_ext::enforce_portrait_window_policy(
                        raw,
                        layout::MIN_WINDOW_WIDTH,
                        layout::MIN_WINDOW_HEIGHT,
                        Some(PORTRAIT_MAX_WIDTH_DIPS),
                        current_dips,
                        true,
                    );
                }
            }
            Message::FullscreenSynced {
                gen_id,
                active: fullscreen,
            }
        });
        self.fullscreen_sync_in_flight = started;
    }


    pub(in crate::shell) fn clear_native_remount_throttle(&self) {
        if let Some(player) = self.native_sink() {
            player.allow_immediate_remount();
        }
    }


    pub(in crate::shell) fn enter_fullscreen(&mut self, context: &ComponentContext<Self>) {
        if self.is_fullscreen || !self.can_enter_fullscreen() {
            return;
        }
        self.clear_native_remount_throttle();
        self.more_menu_open = false;
        self.search_overlay_open = false;
        self.is_fullscreen = true;
        self.fullscreen_window_width = self.viewport_layout.window_width;
        self.sync_window_fullscreen(true, context);
        self.sync_key_route_ctx();
    }


    pub(in crate::shell) fn exit_fullscreen(&mut self, context: &ComponentContext<Self>) {
        if !self.is_fullscreen {
            return;
        }
        self.clear_native_remount_throttle();
        self.is_fullscreen = false;
        self.fullscreen_window_width = self.viewport_layout.window_width;
        self.sync_window_fullscreen(false, context);
        self.sync_key_route_ctx();
    }


    pub(in crate::shell) fn stop_playback(&mut self, context: &ComponentContext<Self>) {
        self.exit_fullscreen(context);
        self.advance_playback_gen();
        self.pending_open_preresolve = None;
        self.now_playing = None;
        self.now_playing_desc = None;
        self.meta_fetch_inflight_seq = None;
        self.playback_avatar_dirty = false;
        self.playback_avatar_timer = None;
        self.playing = false;
        self.position = 0;
        self.engine_duration = None;
        self.player_notice = None;
        self.video_loading = false;
        self.current_meta = None;
        self.timer = None;
        self.home_half_pane = false;
        self.comments_session.reset(0, 0);
        self.refresh_comments_snapshot();
        self.history.retain(|r| *r != Route::Player);
        if let Some(engine) = self.engine.take() {
            engine.stop();
            drop(engine);
        }
        // M7:关播即下线 native 附属资源(常驻层线程 + 独立 D3D device;直播/设置页
        // 走不到引擎切换收口,这里统一收)。
        if let Some(player) = &self.native_player {
            let _ = player.stop();
        }
        self.native_playback = false;
        self.media_load_pending = false;
        self.native_load_epoch = None;
        self.mpv_load_epoch = None;
        self.native_video_probe_at = None;
        self.native_stall_probe_at = None;
        self.media_probe_at = None;
        self.release_stream_proxy_session();
        self.release_stage();
        self.sync_smtc(context);
    }


    pub(in crate::shell) fn reconcile_playback_visibility(&mut self, context: &ComponentContext<Self>) {
        if self.is_fullscreen
            && (self.now_playing.is_none() || self.engine.is_none() || !self.is_player_visible())
        {
            self.exit_fullscreen(context);
        }
        if self.now_playing.is_some() && !self.is_player_visible() {
            if self.park_playback {
                // S2 tier1(park 开启时):离开播放页不关播——播放层垫底继续
                // 解码/出声,回播放页原样续上。产品默认关(用户决策),走 else 关播。
            } else {
                self.stop_playback(context);
            }
        }
        self.sync_key_route_ctx();
    }


}

