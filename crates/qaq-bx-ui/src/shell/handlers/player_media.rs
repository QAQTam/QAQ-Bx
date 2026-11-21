//! 播放器媒体域:媒体解析与流选择(MediaReady/SelectQuality/SelectPage/PageStreamReady/
//! MediaFailed)、视频元数据(VideoMetaLoaded)、进度心跳(PositionTick 250ms)、
//! native/mpv 装载与卡载门控(native_load_choice/mpv_load_* + *_gate_settled/should_show_loading
//! 纯函数)、代理传输(ensure_stream_proxy/prepare_dash_transport_urls)。
//!
//! 本文件由 P3 拆解生成:match 臂与配套方法自原 update()/impl AppShell 逐字搬运,
//! 消息路由见 handlers/mod.rs(穷尽 match)。单元测试锚点见 shell/tests.rs。

use super::super::*;

impl AppShell {
    /// player_media 域消息臂(10 变体,与 message.rs 分组注释对应);未命中变体静默忽略。
    pub(super) fn handle_player_media(&mut self, message: Message, context: &ComponentContext<Self>) -> bool {
        match message {
            Message::PositionTick => {
                // v0.3:native 引擎状态单源(无 has_media/missing_track 探测)。
                if let Some(player) = self.native_sink() {
                    let status = player.status();
                    // 心跳诊断锚(F4 风格):播放态每 30s 一行,验证 pos 推进/时长有效。
                    if status.playing
                        && (status.position_secs as u64).is_multiple_of(30)
                        && status.position_secs % 1.0 < 0.5
                    {
                        eprintln!(
                            "[qaq-bx-ui] native heartbeat pos={:.1} dur={:.1}",
                            status.position_secs, status.duration_secs
                        );
                    }
                    // 装载在途(工作线程建源)保持遮罩,别用旧会话的状态提前解除。
                    // 新视频装载闸门:解析在途或建源在途期间,旧会话快照一律
                    // 冻结;新会话信号 =「本次装载世代命中 && !loading」
                    // (loading 单独不可靠:快速建源时首个节拍前就翻回 false,
                    // 旧信号会让闸门永不解除 → 进度冻结/遮罩常驻/全屏黑屏)。
                    if self.media_load_pending
                        && native_load_gate_settled(
                            self.native_load_epoch,
                            status.injected_seq,
                            status.error.is_some(),
                        )
                    {
                        self.media_load_pending = false;
                        self.native_load_epoch = None;
                    }
                    let loading_now = native_should_show_loading(&status, self.media_load_pending);
                    self.video_loading = loading_now;
                    if !self.media_load_pending {
                        self.position = status
                            .position_secs
                            .round()
                            .clamp(0.0, f64::from(u32::MAX)) as u32;
                        self.playing = status.playing
                            || (loading_now
                                && status.wants_play
                                && !status.finished
                                && status.error.is_none());
                        if status.duration_secs > 0.0 {
                            self.engine_duration = Some(status.duration_secs);
                        }
                        if loading_now && !status.playing && status.wants_play && status.error.is_none()
                        {
                            if self.native_stall_probe_at.is_none() {
                                self.native_stall_probe_at = Some(Instant::now());
                            }
                        } else {
                            self.native_stall_probe_at = None;
                        }
                        // 弹幕透明层时基:节拍喂真实播放位置与播放态(暂停即冻结);
                        // 层内用墙钟外推到满刷新率,与 mpv 通道的弹幕手感一致。
                        if let Some(stage) = self.stage_sink() {
                            stage.set_time(status.position_secs);
                            stage.set_playing(status.playing);
                        }
                        // Bug 2E：native 通路已出声/走时但 NaturalVideoWidth/Height == 0 持续 ≥3s → 回退 mpv。
                        let native_active = !status.loading
                            && status.error.is_none()
                            && (status.playing || status.position_secs > 0.0);
                        if native_active
                            && (status.natural_video_width == 0 || status.natural_video_height == 0)
                        {
                            if self.native_video_probe_at.is_none() {
                                self.native_video_probe_at = Some(Instant::now());
                            }
                        } else {
                            self.native_video_probe_at = None;
                        }
                        if missing_native_video_track(
                            status.natural_video_width,
                            status.natural_video_height,
                            native_active,
                            self.native_video_probe_at.map(|at| at.elapsed()),
                        ) {
                            self.native_video_probe_at = None;
                            self.fallback_native_to_mpv(
                                "系统媒体引擎无视频画面(仅音频/解码缺失)",
                                context,
                            );
                        } else if native_load_stalled(
                            status.wants_play,
                            loading_now,
                            status.playing,
                            self.native_stall_probe_at.map(|at| at.elapsed()),
                        ) {
                            self.native_stall_probe_at = None;
                            self.fallback_native_to_mpv(
                                "系统媒体引擎加载超时",
                                context,
                            );
                        }
                    } else {
                        self.native_stall_probe_at = None;
                    }
                    if let Some(reason) = &status.error {
                        // S4:native 通路失败不再只弹通知 —— 自动回退 mpv 同进度接管。
                        self.fallback_native_to_mpv(reason, context);
                    }
                    // 挂载节拍:engine.attached 恒 false(mpv idle),持续驱动
                    // NativeSurface 重试注入(500ms 节流一次足够)。
                    if mount_pulse_active(
                        self.route,
                        self.home_half_pane,
                        self.is_fullscreen,
                        false,
                        self.park_playback && self.now_playing.is_some(),
                    ) {
                        self.mount_tick = self.mount_tick.wrapping_add(1);
                    }
                    self.sync_smtc(context);
                    if self.now_playing.is_some() {
                        self.timer =
                            Some(context.set_local_timeout(PLAYBACK_TICK, || Message::PositionTick));
                    } else {
                        self.timer = None;
                    }
                    return true;
                }
                // 引擎快照 → UI 单源(乐观更新的 TogglePlay/Seek 在此被真实值校正)。
                // Arc 克隆出借用(F4 回退需要 &mut self)。
                let engine = self.engine.clone();
                if let Some(engine) = engine {
                    let status = engine.status();
                    // 新视频装载闸门:仅当本次 mpv_load_epoch 已被引擎受理(load_seq >= epoch)
                    // 或引擎报错时解除,隔离切视频窗口期间旧媒体的残余位置/时长快照。
                    let engine_err = engine.error();
                    if self.media_load_pending
                        && mpv_load_gate_settled(
                            self.mpv_load_epoch,
                            status.load_seq,
                            engine_err.is_some(),
                        )
                    {
                        self.media_load_pending = false;
                        self.mpv_load_epoch = None;
                    }
                    self.video_loading =
                        mpv_should_show_loading(&status, self.media_load_pending, engine_err.is_some());
                    if !self.media_load_pending && (status.has_media() || status.file_loaded) {
                        self.position =
                            status.time_pos.round().clamp(0.0, f64::from(u32::MAX)) as u32;
                        self.playing = !status.pause && !status.eof;
                        if status.duration > 0.0 {
                            self.engine_duration = Some(status.duration);
                        }
                    }
                    // F4 首播音画缺失探测:媒体就绪后计时,持续缺轨 3s 自动回退。
                    if !self.media_load_pending
                        && status.has_media()
                        && self.media_probe_at.is_none()
                    {
                        self.media_probe_at = Some(Instant::now());
                    }
                    if let Some(track) = missing_media_track(
                        &status.video_format,
                        &status.audio_codec,
                        status.has_media() && !self.media_load_pending,
                        self.media_probe_at.map(|at| at.elapsed()),
                    ) {
                        // 触发一次即清探测点,回退重载会重置。
                        self.media_probe_at = None;
                        match track {
                            MissingTrack::Video => {
                                if !self.video_fallback_done {
                                    self.video_fallback_done = true;
                                    self.apply_video_fallback(context);
                                }
                            }
                            MissingTrack::Audio => {
                                if !self.audio_fallback_done {
                                    self.audio_fallback_done = true;
                                    self.apply_audio_fallback(context);
                                }
                            }
                        }
                    }
                    // 仅当 SwapChain 尚未挂载成功时按 250ms 递增驱动 VideoSurface::try_mount()。
                    if mount_pulse_active(
                        self.route,
                        self.home_half_pane,
                        self.is_fullscreen,
                        engine.attached(),
                        self.park_playback && self.now_playing.is_some(),
                    ) {
                        self.mount_tick = self.mount_tick.wrapping_add(1);
                    }
                    // 引擎错误(初始化/挂载/设备级)一次性上屏。
                    if self.player_notice.is_none()
                        && let Some(reason) = engine.error()
                    {
                        self.player_notice = Some(reason);
                        self.video_loading = false;
                        // 引擎级错误:装载闸门解除(失败不再等新会话快照)。
                        self.media_load_pending = false;
                        self.mpv_load_epoch = None;
                    }
                }
                self.sync_smtc(context);
                // 续拍:仍持有播放项才继续(Back 到首页后 mini_player 也要状态)。
                if self.now_playing.is_some() {
                    self.timer =
                        Some(context.set_local_timeout(PLAYBACK_TICK, || Message::PositionTick));
                } else {
                    self.timer = None;
                }
            }
            Message::MediaReady { seq, media } => {
                if seq != self.playback_gen {
                    return true; // 过期解析结果直接丢弃，防止切视频后串台覆盖
                }
                self.pending_open_preresolve = None;
                self.apply_resolved_media(media, seq, context);
            }
            Message::SelectQuality(qn) => {
                if qn == self.current_qn {
                    return true;
                }
                let Some(dash) = self.playback_dash.clone() else {
                    return true;
                };
                let Some(choice) = select_stream_by_qn(&dash, qn, self.settings.codec.priority())
                else {
                    return true;
                };
                // 定点换流:记住当前进度与播放态,mpv `start` 直达,弹幕/相关推荐不动。
                // v0.3 spike:native 走重载 + SetPosition;若新档位触发 native 回退则无缝交回 mpv。
                let was_native = self.native_active();
                let start = if was_native {
                    self.native_position().max(f64::from(self.position))
                } else {
                    self.engine
                        .as_ref()
                        .map(|engine| engine.status().time_pos)
                        .unwrap_or(0.0)
                };
                self.video_loading = true;
                self.native_stall_probe_at = None;
                if was_native && self.native_load_choice(&choice, self.playing, start) {
                    self.current_qn = qn;
                    self.current_video_url = choice.video.base_url.clone();
                    self.current_audio_url = choice.audio.base_url.clone();
                    self.media_probe_at = None;
                    self.native_video_probe_at = None;
                    self.player_notice = None;
                    return true;
                }
                self.mpv_load_choice(&choice, start, self.playing, context);
                self.current_qn = qn;
                self.current_video_url = choice.video.base_url.clone();
                self.current_audio_url = choice.audio.base_url.clone();
                self.media_probe_at = Some(Instant::now());
                self.player_notice = None;
                if was_native {
                    self.reseed_danmaku(start, context);
                }
            }
            Message::SelectPage(cid) => {
                // U3 分P切换:重取 playurl(cid) → 定点换流 + 弹幕/进度联动。
                if cid == 0 || cid == self.current_cid {
                    return true;
                }
                let Some(item) = self.now_playing.clone() else {
                    return true;
                };
                self.video_loading = true;
                self.native_stall_probe_at = None;
                let nav = self.auth_service.nav_client();
                let (max_qn, codecs) = resolve_params_for(&self.settings, &nav);
                let auth = self.auth_service.clone();
                let gen_id = self.playback_gen;
                context.spawn_background(move |_| {
                    let client = PlayerClient::with_nav(auth.http(), auth.credential_provider(), nav);
                    let result = block_on(client.resolve_full_with_cid(
                        &item.bvid,
                        item.aid,
                        cid,
                        &item.title,
                        i64::from(item.duration_secs),
                        max_qn,
                        codecs,
                    ))
                    .map_err(|err| err.to_string());
                    Message::PageStreamReady { gen_id, cid, result }
                });
            }
            Message::PageStreamReady { gen_id, cid, result } => {
                if gen_id != self.playback_gen {
                    return true;
                }
                match result {
                    Ok(media) => {
                        let ResolvedMedia { choice, dash, .. } = media;
                        // v0.3 spike:native 分支(重载 + SetPosition;native_position
                        // 保持的是旧分P末进度,分P语义本就回零,可接受)。
                        if self.native_active() {
                            if self.native_load_choice(&choice, self.playing, 0.0) {
                                self.current_cid = cid;
                                self.current_qn = choice.video.id;
                                self.current_video_url = choice.video.base_url;
                                self.current_audio_url = choice.audio.base_url;
                                self.playback_dash = Some(dash.clone());
                                self.quality_qns = quality_menu(&dash.video, 0);
                                self.media_probe_at = None;
                                self.native_video_probe_at = None;
                                self.player_notice = None;
                                if let Some(meta) = &mut self.current_meta {
                                    meta.cid = cid;
                                }
                                // 换分P:旧 P 的弹幕先清层(与 mpv 分支同语义)。
                                self.danmaku_replace(Vec::new());
                                self.fetch_danmaku(gen_id, 0.0, context);
                                return true;
                            }
                        }
                        let start = self
                            .engine
                            .as_ref()
                            .map(|engine| engine.status().time_pos)
                            .unwrap_or(0.0);
                        self.mpv_load_choice(&choice, start, self.playing, context);
                        self.current_cid = cid;
                        self.current_qn = choice.video.id;
                        self.current_video_url = choice.video.base_url;
                        self.current_audio_url = choice.audio.base_url;
                        self.playback_dash = Some(dash.clone());
                        self.quality_qns = quality_menu(&dash.video, 0);
                        self.media_probe_at = Some(Instant::now());
                        self.player_notice = None;
                        // 弹幕跟随分P cid 重拉(评论区仍按 aid 不动);预存为
                        // 增量追加,换流前先清空旧 P 的弹幕层。
                        self.danmaku_replace(Vec::new());
                        if let Some(meta) = &mut self.current_meta {
                            meta.cid = cid;
                        }
                        self.fetch_danmaku(gen_id, start, context);
                    }
                    Err(reason) => {
                        self.video_loading = false;
                        self.player_notice = Some(format!("切换分P失败:{reason}"));
                    }
                }
            }
            Message::VideoMetaLoaded { seq, res } => {
                if seq != self.playback_gen {
                    return true;
                }
                if self.meta_fetch_inflight_seq == Some(seq) {
                    self.meta_fetch_inflight_seq = None;
                }
                match res {
                    Ok(meta) => self.absorb_video_meta(&meta, seq, context),
                    Err(err) => {
                        if self.comments_session.aid == 0 {
                            let sort = self.comments_session.sort;
                            let bucket = self.comments_session.bucket_mut(sort);
                            bucket.loaded_once = true;
                            bucket.error = Some(err);
                            self.refresh_comments_snapshot();
                        }
                    }
                }
            }
            Message::MediaFailed { seq, reason } => {
                if seq != self.playback_gen {
                    return true;
                }
                // 直点路径命中试看降级:登记不可播(回到首页时卡面带角标)。
                if is_trial_downgrade_reason(&reason)
                    && let Some(bvid) = self.now_playing.as_ref().map(|i| i.bvid.clone())
                {
                    self.unplayable.entry(bvid).or_insert(reason.clone());
                    self.rebuild_home_source();
                }
                self.player_notice = Some(format!("视频解析失败:{reason}"));
                self.playing = false;
                self.video_loading = false;
                // 装载闸门解除:失败不再等引擎快照(没有新会话可等)。
                self.media_load_pending = false;
                self.native_load_epoch = None;
                self.mpv_load_epoch = None;
                self.native_video_probe_at = None;
            }
            _ => {}
        }
        false
    }

    pub(in crate::shell) fn ensure_stream_proxy(&mut self) -> Option<Arc<StreamProxy>> {
        if let Some(proxy) = &self.stream_proxy {
            return Some(Arc::clone(proxy));
        }
        match StreamProxy::start(
            crate::runtime::shared_runtime().handle().clone(),
            Arc::clone(&self.cdn_health),
        ) {
            Ok(proxy) => {
                self.stream_proxy = Some(Arc::clone(&proxy));
                Some(proxy)
            }
            Err(err) => {
                eprintln!("[qaq-bx-ui] stream-proxy start failed, fallback direct url: {err}");
                None
            }
        }
    }


    pub(in crate::shell) fn release_stream_proxy_session(&mut self) {
        if let Some(session_id) = self.proxy_session_id.take()
            && let Some(proxy) = &self.stream_proxy
        {
            proxy.unregister_session(session_id);
        }
    }


    /// 为 DASH 音视频流生成实际传输 URL（默认开启本地并发代理与多 CDN 容灾；关闭或建代理失败回退直连最优 CDN）。
    pub(in crate::shell) fn prepare_dash_transport_urls(
        &mut self,
        video: &VideoStream,
        audio: &AudioStream,
    ) -> (String, String) {
        let prefer_official = self.settings.prefer_official_cdn;
        let v_ranked = self
            .cdn_health
            .rank_candidates(&video.candidate_urls(prefer_official));
        let a_ranked = self
            .cdn_health
            .rank_candidates(&audio.candidate_urls(prefer_official));
        let v_fallback = v_ranked
            .first()
            .cloned()
            .unwrap_or_else(|| video.base_url.clone());
        let a_fallback = a_ranked
            .first()
            .cloned()
            .unwrap_or_else(|| audio.base_url.clone());

        self.release_stream_proxy_session();
        if self.settings.multi_thread_preload
            && !v_ranked.is_empty()
            && !a_ranked.is_empty()
            && let Some(proxy) = self.ensure_stream_proxy()
        {
            let v_count = v_ranked.len();
            let a_count = a_ranked.len();
            let pair = proxy.register_dash_pair(v_ranked, a_ranked, true);
            self.proxy_session_id = Some(pair.session_id);
            eprintln!(
                "[qaq-bx-ui] stream-proxy route qn={} v_cands={} a_cands={} prefer_upos={}",
                video.id, v_count, a_count, prefer_official
            );
            return (pair.video_url, pair.audio_url);
        }
        (v_fallback, a_fallback)
    }


    /// v0.3:native 引擎接管装载(系统 DASH 管线;UI 线程只提交请求)。
    /// 返回 false = native 未启用,调用方走 mpv 路径。MPD→MediaSource 的网络+
    /// 解析在引擎装载线程,产物由 `NativeSurface` 的挂载节拍注入;失败经
    /// `status().error` 在 PositionTick 落到 `player_notice`,不在此阻塞。
    pub(in crate::shell) fn native_load_choice(
        &mut self,
        choice: &qaq_bx_core::player::StreamChoice,
        autoplay: bool,
        start_secs: f64,
    ) -> bool {
        if !self.native_enabled() {
            return false;
        }
        // S4:本会话回退记忆 —— 该 codec 桶 native 解码失败过(系统扩展缺失等),
        // 直接走 mpv,不给用户第二次黑屏/报错。
        let codec_bucket = choice.video.codecs.split('.').next().unwrap_or_default().to_string();
        if !codec_bucket.is_empty() && self.native_fallback_codecs.contains(&codec_bucket) {
            eprintln!(
                "[qaq-bx-ui] native load skip: codec {codec_bucket} 本会话已回退 mpv"
            );
            return false;
        }
        let (video_transport, audio_transport) =
            self.prepare_dash_transport_urls(&choice.video, &choice.audio);
        let player = match &self.native_player {
            Some(player) => player.clone(),
            None => {
                let player = Arc::new(NativePlayer::new());
                self.native_player = Some(player.clone());
                player
            }
        };
        let mpd = qaq_bx_core::mpd::build_mpd(&choice.video, Some(&choice.audio));
        // 诊断锚:系统媒体管线只吃 MF 支持的编码,这里把选中分轨的 codec/mime 留痕。
        eprintln!(
            "[qaq-bx-ui] native load qn={} codec={} mime={} w={} h={}",
            choice.video.id, choice.video.codecs, choice.video.mime_type, choice.video.width,
            choice.video.height
        );
        if let Err(e) = player.load(
            &mpd,
            &video_transport,
            Some(&audio_transport),
            autoplay,
            start_secs,
        ) {
            eprintln!("[qaq-bx-ui] native engine load failed: {e}");
            let _ = player.stop();
            self.native_playback = false;
            self.native_load_epoch = None;
            self.native_video_probe_at = None;
            self.native_stall_probe_at = None;
            self.release_stage();
            return false;
        }
        // Bug 2B：进入 native 通道前停掉 mpv 解码并清 mpv 世代，防止双引擎同播（音频串音/资源竞争）。
        if let Some(engine) = &self.engine {
            engine.stop();
        }
        self.mpv_load_epoch = None;
        self.media_probe_at = None;
        self.native_video_probe_at = None;
        self.native_stall_probe_at = None;
        if !self.native_playback {
            self.reset_smtc_writer(true);
        }
        // 弹幕改由透明层承接(视频面归 MPE,mpv 引擎保持 idle)。
        self.ensure_stage();
        self.native_playback = true;
        self.media_load_pending = true;
        self.video_loading = true;
        // 记账本次装载世代:begin() 在 load() 内同步自增,快照即新世代。
        self.native_load_epoch = Some(player.status().request_seq);
        // 音量先记账(未 attach 时 SetVolume 无对象,attach 时由引擎统一施加)。
        let _ = player.set_volume(f64::from(self.volume));
        self.playing = autoplay;
        true
    }


    /// v0.3:native 引擎当前进度(换画质/换分P 的定点续播起点)。
    pub(in crate::shell) fn native_position(&self) -> f64 {
        self.native_sink()
            .map(|player| player.status().position_secs)
            .unwrap_or(0.0)
    }


    /// 经 CDN 择优与本地并发代理装入一对 DASH 分轨到 mpv。
    pub(in crate::shell) fn mpv_load_choice(
        &mut self,
        choice: &StreamChoice,
        start_secs: f64,
        autoplay: bool,
        context: &ComponentContext<Self>,
    ) -> u64 {
        let (video_transport, audio_transport) =
            self.prepare_dash_transport_urls(&choice.video, &choice.audio);
        self.mpv_load_streams(
            &video_transport,
            Some(&audio_transport),
            start_secs,
            autoplay,
            context,
        )
    }


    /// Bug 2B：切回/下发 mpv 装载单源，确保停掉 native 侧并记录 `mpv_load_epoch` 与 `media_load_pending`。
    pub(in crate::shell) fn mpv_load_streams(
        &mut self,
        video_url: &str,
        audio_url: Option<&str>,
        start_secs: f64,
        autoplay: bool,
        context: &ComponentContext<Self>,
    ) -> u64 {
        self.teardown_native_side(context);
        let engine = self.ensure_engine();
        engine.set_volume(f64::from(self.volume));
        let seq = engine.load_with_options(video_url, audio_url, start_secs, autoplay);
        self.media_load_pending = true;
        self.video_loading = true;
        self.mpv_load_epoch = Some(seq);
        self.native_load_epoch = None;
        self.native_video_probe_at = None;
        self.native_stall_probe_at = None;
        seq
    }


    /// S3:当前档位在 DASH 里的分轨对照(切引擎定点续播用)。
    /// 先按直链精确复用实际在播的那一对轨道(F4 回退过的编码桶不被切回去),
    /// 找不到再退回「当前档位 + 编码偏好」重选(同画质菜单口径)。
    pub(in crate::shell) fn current_choice(&self) -> Option<StreamChoice> {
        let dash = self.playback_dash.as_ref()?;
        let video = dash
            .video
            .iter()
            .find(|v| v.base_url == self.current_video_url);
        let audio = dash
            .audio
            .iter()
            .find(|a| a.base_url == self.current_audio_url);
        if let (Some(video), Some(audio)) = (video, audio) {
            return Some(StreamChoice {
                video: video.clone(),
                audio: audio.clone(),
            });
        }
        select_stream_by_qn(dash, self.current_qn, self.settings.codec.priority())
    }


    /// 进入播放页:递增播放代次 + 惰性创建引擎 + 优先命中 Hover 预解析缓存 + 否则后台解析(cid 直出仅 1 跳)。
    pub(in crate::shell) fn start_playback(&mut self, item: VideoItem, context: &ComponentContext<Self>) {
        self.more_menu_open = false;
        self.search_overlay_open = false;
        if self.route == Route::Home
            && !self.viewport_layout.is_mobile()
            && self.settings.home_open_mode == qaq_bx_core::settings::HomeOpenMode::HalfPane
        {
            self.home_half_pane = true;
        } else {
            self.home_half_pane = false;
            if self.route != Route::Player {
                self.history.push(self.route);
            }
            self.route = Route::Player;
            crate::trace::note("route->player (打开视频)");
        }
        self.advance_playback_gen();
        self.pending_open_preresolve = None;
        self.current_meta = None;
        self.playback_dash = None;
        self.quality_qns = Vec::new();
        self.current_qn = 0;
        self.current_video_url.clear();
        self.current_audio_url.clear();
        self.media_probe_at = None;
        self.failed_video_prefixes.clear();
        self.video_fallback_done = false;
        self.audio_fallback_done = false;
        self.now_playing_desc = None;
        self.meta_fetch_inflight_seq = None;
        self.playback_avatar_dirty = false;
        self.playback_avatar_timer = None;
        // 视频引擎仍是播放页的渲染前提(native 模式下它保持 idle)。
        if let Some(engine) = &self.engine {
            engine.stop();
        } else {
            self.ensure_engine();
        }
        // 清弹幕层:投递口按当前模式分流(native → 透明层;mpv → 引擎同帧)。
        self.danmaku_replace(Vec::new());
        // U1:新视频重置评论输入态。
        self.comment_draft.clear();
        self.composer_notice = None;
        // U3:新视频重置分P态。
        self.pages_menu.clear();
        self.current_cid = 0;
        self.now_playing = Some(item.clone());
        self.playing = self.settings.autoplay;
        self.position = 0;
        self.engine_duration = None;
        // 若仍留在 native 通路只停播放会话（保留 stage/surface 避免同页切集表面闪断）；
        // 切非 native 或后续回退 mpv 时由 teardown_native_side 收口释放。
        if self.native_enabled() {
            if let Some(player) = &self.native_player {
                let _ = player.stop();
            }
        } else {
            self.teardown_native_side(context);
        }
        // 新视频装载闸门:解析在途期间引擎仍持有旧媒体快照,PositionTick
        // 不得把旧时长/进度灌进 UI(见字段注释)。
        self.media_load_pending = true;
        self.native_load_epoch = None;
        self.mpv_load_epoch = None;
        self.native_video_probe_at = None;
        self.native_stall_probe_at = None;
        self.release_stream_proxy_session();
        self.player_notice = None;
        self.video_loading = true;
        self.comments_session.reset(item.aid, item.author_mid);
        self.refresh_comments_snapshot();
        self.schedule_tick(context);
        self.kickoff_video_extras(self.playback_gen, context);
        // 右栏相关推荐：真实 archive/related 随播放代次拉取（快速切视频由代次校验作废旧响应）。
        self.fetch_related(self.playback_gen, item.bvid.clone(), context);

        let nav = self.auth_service.nav_client();
        let current_key = resolve_profile_key(&self.settings, &nav);

        // 1) 命中新鲜 Hover 预解析缓存：直接送入 mpv，0ms 等待解析。
        if let Some(cached) = self.take_fresh_preresolved(&item.bvid, current_key) {
            let seq = self.playback_gen;
            self.apply_resolved_media(cached.media, seq, context);
            return;
        }

        // 2) 若同名 Hover 预解析正在飞且配置匹配：直接接管在飞请求结果，不重复发 playurl。
        if self.preresolve_inflight.get(&item.bvid).copied() == Some(current_key) {
            self.pending_open_preresolve =
                Some((item.bvid.clone(), self.playback_gen, current_key));
            return;
        }

        spawn_resolve_media(
            &self.auth_service,
            &self.settings,
            item,
            self.playback_gen,
            context,
        );
    }


    pub(in crate::shell) fn advance_playback_gen(&mut self) -> u64 {
        self.playback_gen = self.playback_gen.wrapping_add(1);
        if self.playback_gen == 0 {
            self.playback_gen = 1;
        }
        self.playback_epoch
            .store(self.playback_gen, Ordering::SeqCst);
        self.playback_gen
    }


    /// 播放页画质菜单视图((qn, 标签) 降序;F2)。
    pub(in crate::shell) fn quality_menu_view(&self) -> Vec<(i64, String)> {
        self.quality_qns
            .iter()
            .map(|qn| (*qn, quality_label(*qn)))
            .collect()
    }


    /// U3 分P菜单视图。
    pub(in crate::shell) fn pages_menu_view(&self) -> Vec<(u64, String)> {
        self.pages_menu.clone()
    }


    pub(in crate::shell) fn absorb_video_meta(&mut self, meta: &VideoMeta, seq: u64, context: &ComponentContext<Self>) {
        if seq != self.playback_gen {
            return;
        }
        if let Some(desc) = &meta.desc
            && !desc.trim().is_empty()
        {
            self.now_playing_desc = Some(desc.clone());
        }
        if let Some(cur) = self.now_playing.as_mut() {
            if cur.aid == 0 && meta.aid > 0 {
                cur.aid = meta.aid;
            }
            if cur.cid == 0 && meta.cid > 0 {
                cur.cid = meta.cid;
            }
            if cur.title.is_empty() || cur.title == cur.bvid {
                cur.title = meta.title.clone();
            }
            if cur.duration_secs == 0 && meta.duration > 0 {
                cur.duration_secs = meta.duration as u32;
            }
            if let Some(owner) = &meta.owner {
                if cur.author_mid == 0 && owner.mid > 0 {
                    cur.author_mid = owner.mid;
                }
                if cur.author.is_empty() && !owner.name.is_empty() {
                    cur.author = owner.name.clone();
                }
                if cur.author_face_url.is_none() {
                    cur.author_face_url = owner.face.as_deref().and_then(normalize_face_url);
                }
            }
            // 互动计数回填（搜索卡/最小模型缺 stat 时经 view 补齐；已有真实值不覆盖）。
            if let Some(stat) = &meta.stat {
                if cur.play_count == 0 {
                    cur.play_count = stat.view.unsigned_abs();
                }
                if cur.danmaku_count == 0 {
                    cur.danmaku_count = stat.danmaku.unsigned_abs();
                }
                if cur.like_count.is_none() {
                    cur.like_count = stat.like.map(|v| v.unsigned_abs());
                }
                if cur.coin_count.is_none() {
                    cur.coin_count = stat.coin.map(|v| v.unsigned_abs());
                }
                if cur.favorite_count.is_none() {
                    cur.favorite_count = stat.favorite.map(|v| v.unsigned_abs());
                }
                if cur.share_count.is_none() {
                    cur.share_count = stat.share.map(|v| v.unsigned_abs());
                }
            }
        }
        if self.comments_session.aid == 0 && meta.aid > 0 {
            self.comments_session.aid = meta.aid;
        }
        if self.comments_session.upper_mid == 0
            && let Some(owner) = &meta.owner
            && owner.mid > 0
        {
            self.comments_session.upper_mid = owner.mid;
        }
        self.refresh_comments_snapshot();
        self.kickoff_video_extras(seq, context);
    }


    pub(in crate::shell) fn kickoff_video_extras(&mut self, seq: u64, context: &ComponentContext<Self>) {
        if seq != self.playback_gen {
            return;
        }
        let Some(item) = self.now_playing.clone() else {
            return;
        };
        if let Some(face) = item.author_face_url.as_deref() {
            self.spawn_playback_avatars(std::iter::once(face), seq, context);
        }
        if item.aid > 0 {
            let sort = self.comments_session.sort;
            let bucket = self.comments_session.bucket(sort);
            if !bucket.loaded_once && !bucket.loading && bucket.error.is_none() {
                self.fetch_comments(sort, false, context);
            }
        }
        // cid 直出跳过 resolve() 内的 view，但缺 aid 或缺 UP 主头像时补拉一次 view。
        if item.cid > 0
            && (item.aid == 0 || item.author_face_url.is_none())
            && self.meta_fetch_inflight_seq != Some(seq)
        {
            self.meta_fetch_inflight_seq = Some(seq);
            let auth = self.auth_service.clone();
            let nav = auth.nav_client();
            let bvid = item.bvid;
            context.spawn_background(move |_| {
                let client = PlayerClient::with_nav(auth.http(), auth.credential_provider(), nav);
                let res = block_on(client.video_meta(&bvid)).map_err(|e| e.to_string());
                Message::VideoMetaLoaded { seq, res }
            });
        }
    }


    pub(in crate::shell) fn apply_resolved_media(
        &mut self,
        media: ResolvedMedia,
        seq: u64,
        context: &ComponentContext<Self>,
    ) {
        let ResolvedMedia { meta, choice, dash } = media;
        self.absorb_video_meta(&meta, seq, context);
        self.current_qn = choice.video.id;
        self.current_cid = meta.cid;
        self.pages_menu = pages_menu_from(&meta.pages);
        self.playback_dash = Some(dash.clone());
        self.quality_qns = quality_menu(&dash.video, 0);
        self.current_video_url = choice.video.base_url.clone();
        self.current_audio_url = choice.audio.base_url.clone();
        self.media_probe_at = Some(Instant::now());
        // v0.3 spike:native 引擎接管时,mpv 引擎仍需创建(播放页以 engine 存在
        // 为渲染前提),但不 loadfile —— 保持 idle。
        let _ = self.ensure_engine();
        if self.native_load_choice(&choice, self.settings.autoplay, 0.0) {
            self.playing = self.settings.autoplay;
            self.current_meta = Some(meta);
            self.fetch_danmaku(seq, 0.0, context);
            return;
        }
        self.mpv_load_choice(
            &choice,
            0.0,
            self.settings.autoplay,
            context,
        );
        self.playing = self.settings.autoplay;
        self.current_meta = Some(meta);
        self.fetch_danmaku(seq, 0.0, context);
    }


    /// F4:视频轨解码失败自动回退——按失败桶重选编码,记忆进度无缝换流。
    pub(in crate::shell) fn apply_video_fallback(&mut self, context: &ComponentContext<Self>) {
        let Some(dash) = self.playback_dash.clone() else {
            return;
        };
        let failed_prefix = dash
            .video
            .iter()
            .find(|v| v.base_url == self.current_video_url)
            .and_then(|v| v.codecs.split('.').next())
            .unwrap_or_default()
            .to_string();
        if !failed_prefix.is_empty() && !self.failed_video_prefixes.contains(&failed_prefix) {
            self.failed_video_prefixes.push(failed_prefix);
        }
        let failed: Vec<&str> = self
            .failed_video_prefixes
            .iter()
            .map(String::as_str)
            .collect();
        let Some(choice) =
            select_stream_excluding(&dash, 0, self.settings.codec.priority(), &failed)
        else {
            // Bug 2D：所有视频编码桶均失败时必须停掉 mpv（否则 UI 显示错误提示同时音轨仍继续出声）。
            if let Some(engine) = &self.engine {
                engine.stop();
            }
            self.mpv_load_epoch = None;
            self.media_load_pending = false;
            self.video_loading = false;
            self.player_notice = Some(
                "视频轨无法解码(所有编码桶均失败)。可尝试切换清晰度/编码或更新系统解码组件。"
                    .to_string(),
            );
            self.playing = false;
            return;
        };
        let start = self
            .engine
            .as_ref()
            .map(|engine| engine.status().time_pos)
            .unwrap_or(0.0);
        self.mpv_load_choice(&choice, start, self.playing, context);
        eprintln!(
            "[qaq-bx-ui] video-track fallback: {} -> {}",
            self.failed_video_prefixes.join("+"),
            choice.video.codecs
        );
        self.current_video_url = choice.video.base_url;
        self.current_audio_url = choice.audio.base_url;
        self.current_qn = choice.video.id;
        self.media_probe_at = Some(Instant::now());
        self.player_notice = None;
    }


    /// F4:音频轨缺失自动回退——换次高带宽音轨,原画质不动。
    pub(in crate::shell) fn apply_audio_fallback(&mut self, context: &ComponentContext<Self>) {
        let Some(dash) = self.playback_dash.clone() else {
            return;
        };
        let Some(audio) = select_audio_excluding(&dash, &self.current_audio_url) else {
            // Bug 2D：无备选音轨时保留正常播放的视频画面（不能设置 player_notice 卸载 VideoSurface）。
            eprintln!("[qaq-bx-ui] audio-track fallback: 无备选音轨,保留当前视频播放");
            return;
        };
        let start = self
            .engine
            .as_ref()
            .map(|engine| engine.status().time_pos)
            .unwrap_or(0.0);
        let fallback_audio_url = audio.base_url.clone();
        let fallback_audio_id = audio.id;
        if let Some(video) = dash
            .video
            .iter()
            .find(|v| v.base_url == self.current_video_url)
            .or_else(|| dash.video.iter().find(|v| v.id == self.current_qn))
            .cloned()
        {
            let choice = StreamChoice { video, audio };
            self.mpv_load_choice(&choice, start, self.playing, context);
        } else {
            self.mpv_load_streams(
                &self.current_video_url.clone(),
                Some(&fallback_audio_url),
                start,
                self.playing,
                context,
            );
        }
        eprintln!(
            "[qaq-bx-ui] audio-track fallback: {} -> id={}",
            self.current_audio_url, fallback_audio_id
        );
        self.current_audio_url = fallback_audio_url;
        self.media_probe_at = Some(Instant::now());
        self.player_notice = None;
    }


}

#[derive(Clone)]
pub(in crate::shell) struct CachedMedia {
    pub(in crate::shell) key: ResolveProfileKey,
    pub(in crate::shell) media: ResolvedMedia,
    pub(in crate::shell) fetched_at: Instant,
}


/// F4 首播音画缺失探测:媒体已就绪(时长>0)但解码轨道缺失持续 ≥3s → 报缺哪条轨。
/// 「无画面有声音」= video-format 空;「有画面无声音」= audio-codec 空。
pub(crate) fn missing_media_track(
    video_format: &str,
    audio_codec: &str,
    has_media: bool,
    probing_for: Option<Duration>,
) -> Option<MissingTrack> {
    const PROBE_GRACE: Duration = Duration::from_secs(3);
    if !has_media {
        return None;
    }
    if probing_for.is_none_or(|elapsed| elapsed < PROBE_GRACE) {
        return None;
    }
    let video_missing = video_format.trim().is_empty();
    let audio_missing = audio_codec.trim().is_empty();
    match (video_missing, audio_missing) {
        (true, _) => Some(MissingTrack::Video),
        (false, true) => Some(MissingTrack::Audio),
        (false, false) => None,
    }
}


/// F4 探测结论(驱动自动回退方向)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MissingTrack {
    /// 视频轨未解出(典型:编码器缺失,如 AV1-only 流)。
    Video,
    /// 音频轨未解出(典型:外挂音轨拉取失败或杜比解码缺失)。
    Audio,
}


/// native 装载闸门是否放行：
/// - `epoch == None`：PlayURL 仍在解析、尚未提交 `NativePlayer::load`，保持闸门隔离旧视频状态；
/// - `has_error` 或 `injected_seq >= Some(epoch)`：新会话已注入接管或失败，解除闸门。
pub(crate) fn native_load_gate_settled(
    epoch: Option<u64>,
    injected_seq: Option<u64>,
    has_error: bool,
) -> bool {
    match epoch {
        Some(ep) => has_error || injected_seq.is_some_and(|inj| inj >= ep),
        None => false,
    }
}


/// mpv 装载闸门是否放行：
/// - `epoch == None`：PlayURL 仍在解析、尚未提交 `loadfile`，保持闸门隔离旧快照；
/// - `has_error` 或 `load_seq >= epoch`：引擎线程已受理新代次并刷新状态快照，解除闸门。
pub(crate) fn mpv_load_gate_settled(
    epoch: Option<u64>,
    load_seq: u64,
    has_error: bool,
) -> bool {
    match epoch {
        Some(ep) => has_error || load_seq >= ep,
        None => false,
    }
}


/// native 是否仍在加载/打开/缓冲（供 UI 显示居中 `ProgressRing`）。
/// 只有明确处于 Playing、或主动 Pause 且已有媒体打开、或报错/结束时才熄灭；
/// Opening / Buffering / 尚未 MediaOpened 时必须保持加载动画，避免黑屏无转圈。
pub(crate) fn native_should_show_loading(
    status: &qaq_bx_video::native::NativeStatus,
    gate_pending: bool,
) -> bool {
    if status.error.is_some() || status.finished {
        return false;
    }
    if gate_pending || status.loading || status.opening || status.buffering {
        return true;
    }
    if status.playing {
        return false;
    }
    if status.paused {
        return status.wants_play && !status.media_opened && status.natural_video_width == 0;
    }
    status.wants_play || !status.media_opened
}


/// native 通路起播或缓冲卡死超过阈值（12s），触发回退 mpv。
pub(crate) fn native_load_stalled(
    wants_play: bool,
    loading_active: bool,
    playing: bool,
    stalled_for: Option<Duration>,
) -> bool {
    const NATIVE_STALL_TIMEOUT: Duration = Duration::from_secs(12);
    if !wants_play || !loading_active || playing {
        return false;
    }
    stalled_for.is_some_and(|elapsed| elapsed >= NATIVE_STALL_TIMEOUT)
}


/// mpv 是否仍在加载/缓冲中（含 `paused-for-cache` 与首帧前阶段）。
pub(crate) fn mpv_should_show_loading(
    status: &qaq_bx_video::engine::PlaybackStatus,
    gate_pending: bool,
    has_error: bool,
) -> bool {
    if has_error || status.eof {
        return false;
    }
    if gate_pending || status.buffering {
        return true;
    }
    if !status.file_loaded && !status.has_media() {
        return true;
    }
    !status.playback_started && !status.pause && status.time_pos <= 0.0
}


/// Bug 2E：native 通路已非 loading 且在播/走时，但 `NaturalVideoWidth/Height == 0`
/// 持续 ≥3s → 仅音频无视频画面（触发回退 mpv）。
pub(crate) fn missing_native_video_track(
    natural_w: u32,
    natural_h: u32,
    active: bool,
    probing_for: Option<Duration>,
) -> bool {
    const PROBE_GRACE: Duration = Duration::from_secs(3);
    if !active || (natural_w > 0 && natural_h > 0) {
        return false;
    }
    probing_for.is_some_and(|elapsed| elapsed >= PROBE_GRACE)
}


/// U3 分P菜单:(cid, "P{page} {part}");少于 2 个分P返回空(不渲染选择器)。
pub(crate) fn pages_menu_from(pages: &[VideoPage]) -> Vec<(u64, String)> {
    let mut menu: Vec<(u64, String)> = pages
        .iter()
        .filter(|page| page.cid > 0)
        .map(|page| {
            let label = if page.part.trim().is_empty() {
                format!("P{}", page.page.max(1))
            } else {
                format!("P{} {}", page.page.max(1), page.part.trim())
            };
            (page.cid, label)
        })
        .collect();
    if menu.len() < 2 {
        menu.clear();
    }
    menu
}


pub(in crate::shell) fn spawn_resolve_media(
    auth_service: &Arc<Auth>,
    settings: &AppSettings,
    item: VideoItem,
    seq: u64,
    context: &ComponentContext<AppShell>,
) {
    let auth = auth_service.clone();
    let nav = auth.nav_client();
    let (max_qn, codecs) = resolve_params_for(settings, &nav);
    context.spawn_background(move |_| {
        let client = PlayerClient::with_nav(auth.http(), auth.credential_provider(), nav);
        let res = block_on(client.resolve_full_with_cid(
            &item.bvid,
            item.aid,
            item.cid,
            &item.title,
            i64::from(item.duration_secs),
            max_qn,
            codecs,
        ));
        match res {
            Ok(media) => Message::MediaReady { seq, media },
            Err(error) => Message::MediaFailed {
                seq,
                reason: error.to_string(),
            },
        }
    });
}


