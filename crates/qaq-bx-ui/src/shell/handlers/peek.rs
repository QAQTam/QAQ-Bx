//! 悬停预览卡域:PeekMachine 三拍定时(预取/弹出/宽限,见 #7)、hover 上报与撤销、
//! 简介预取(fetch_peek_desc)、卡片动作(PeekPlay/PeekWatchLater/PeekClose)、媒体预解析
//! 缓存(cache/take_fresh_preresolved,ResolveProfileKey 维度,供开播免等待)。
//!
//! 本文件由 P3 拆解生成:match 臂与配套方法自原 update()/impl AppShell 逐字搬运,
//! 消息路由见 handlers/mod.rs(穷尽 match)。单元测试锚点见 shell/tests.rs。

use super::super::*;

impl AppShell {
    /// peek 域消息臂(17 变体,与 message.rs 分组注释对应);未命中变体静默忽略。
    pub(super) fn handle_peek(&mut self, message: Message, context: &ComponentContext<Self>) -> bool {
        match message {
            Message::PreResolveVideo(bvid) => {
                // 已判定不可播(充电/付费未解锁):不再重复发 playurl。
                if self.unplayable.contains_key(&bvid) {
                    return true;
                }
                let Some(item) = self.home_index.get(&bvid).cloned() else {
                    return true;
                };
                if self
                    .now_playing
                    .as_ref()
                    .is_some_and(|cur| cur.bvid == bvid)
                {
                    return true;
                }
                let nav = self.auth_service.nav_client();
                let key = resolve_profile_key(&self.settings, &nav);
                if self
                    .preresolved
                    .get(&bvid)
                    .is_some_and(|c| c.key == key && c.fetched_at.elapsed() < PRERESOLVE_TTL)
                {
                    return true;
                }
                if self.preresolve_inflight.contains_key(&bvid)
                    || self.preresolve_inflight.len() >= MAX_PRERESOLVE_INFLIGHT
                {
                    return true;
                }
                self.preresolve_inflight.insert(bvid.clone(), key);

                let auth = self.auth_service.clone();
                let (max_qn, codecs) = resolve_params_for(&self.settings, &nav);
                context.spawn_background(move |_| {
                    let client =
                        PlayerClient::with_nav(auth.http(), auth.credential_provider(), nav);
                    let res = block_on(client.resolve_full_with_cid(
                        &item.bvid,
                        item.aid,
                        item.cid,
                        &item.title,
                        i64::from(item.duration_secs),
                        max_qn,
                        codecs,
                    ))
                    .map_err(|e| e.to_string());
                    Message::MediaPreResolved {
                        bvid: item.bvid,
                        key,
                        result: res,
                    }
                });
            }
            Message::MediaPreResolved { bvid, key, result } => {
                self.preresolve_inflight.remove(&bvid);
                let matched_open = self.pending_open_preresolve.as_ref().is_some_and(
                    |(pending_bvid, gen_id, pending_key)| {
                        pending_bvid == &bvid && *gen_id == self.playback_gen && *pending_key == key
                    },
                );
                if matched_open {
                    self.pending_open_preresolve = None;
                }
                match result {
                    Ok(media) => {
                        if std::env::var_os("QAQ_BX_TRACE_HOME").is_some() {
                            eprintln!(
                                "[qaq-bx-ui] preresolved cached (no autoplay): bvid={}, qn={}, route={:?}",
                                bvid, media.choice.video.id, self.route
                            );
                        }
                        if matched_open {
                            self.apply_resolved_media(media.clone(), self.playback_gen, context);
                        }
                        self.cache_preresolved(bvid, key, media);
                    }
                    Err(reason) => {
                        // 试看降级(充电/付费未解锁):登记不可播并刷新卡面角标。
                        if is_trial_downgrade_reason(&reason) {
                            self.unplayable.entry(bvid.clone()).or_insert(reason.clone());
                            self.rebuild_home_source();
                        }
                        if matched_open {
                            self.player_notice = Some(format!("视频解析失败:{reason}"));
                            self.playing = false;
                            self.video_loading = false;
                        }
                    }
                }
            }

            // ---- #7 悬停预览卡 ----
            Message::CardHover(report) => self.handle_card_hover(report, context),
            Message::PeekPrefetchTick(seq) => {
                if let Some(bvid) = self.peek.on_prefetch_tick(seq) {
                    self.fetch_peek_desc(&bvid.to_string(), context);
                }
            }
            Message::PeekPopTick(seq) => self.handle_peek_pop(seq, context),
            Message::PeekGraceTick(seq) => {
                if self.peek.on_grace_tick(seq) {
                    self.peek_notice = None;
                    self.peek_anim_timer = None;
                    self.peek_anim_pending = false;
                    if std::env::var_os("QAQ_BX_TRACE_HOME").is_some() {
                        eprintln!("[qaq-bx-ui] peek hide");
                    }
                }
            }
            Message::PeekAnimTick => {
                // 翻牌帧:此后视图发布 Scale 1.15/Opacity 终值(composition 隐式过渡)。
                if self.peek.phase_open() {
                    self.peek_anim_pending = false;
                }
            }
            Message::PeekPointerEntered => {
                if self.peek.on_peek_enter() {
                    self.peek_grace_timer = None;
                }
            }
            Message::PeekPointerExited => {
                if let Some(seq) = self.peek.on_peek_exit() {
                    self.peek_grace_timer =
                        Some(context.set_local_timeout(peek::PEEK_GRACE, move || {
                            Message::PeekGraceTick(seq)
                        }));
                }
            }
            Message::PeekDescReady { bvid, desc } => {
                self.peek_desc_inflight.remove(&bvid);
                self.peek_desc.entry(bvid).or_insert(desc);
            }
            Message::PeekPlay(bvid) => {
                // 点「播放」消费了预览意图:静默复位,悬停功能继续可用。
                self.peek_hide_now();
                self.peek_dismissal.reset();
                self.open_video(&bvid, context);
                self.sync_smtc(context);
            }
            Message::PeekWatchLater(bvid) => {
                if !matches!(self.auth, AuthState::LoggedIn(_)) {
                    self.peek_notice = Some("未登录,「稍后再看」不可用".to_string());
                } else {
                    let auth = self.auth_service.clone();
                    let nav = auth.nav_client();
                    context.spawn_background(move |_| {
                        let client =
                            PlayerClient::with_nav(auth.http(), auth.credential_provider(), nav);
                        let error = block_on(client.add_watch_later(&bvid)).err();
                        Message::PeekWatchLaterDone {
                            bvid,
                            error: error.map(|err| err.to_string()),
                        }
                    });
                }
            }
            Message::PeekWatchLaterDone { bvid, error } => {
                // 仅当前预览仍为该稿件时展示回执(已收起则静默丢弃)。
                if self.peek_mounted_bvid().as_deref() == Some(bvid.as_str()) {
                    self.peek_notice = Some(match error {
                        None => "已加入「稍后再看」".to_string(),
                        Some(reason) => format!("稍后再看失败:{reason}"),
                    });
                }
            }
            Message::PeekClose => {
                // 仅静默当前卡:指针离开它或换卡即恢复,不关闭整个首页的预览。
                let bvid = self.peek_mounted_bvid();
                self.peek_dismissal.close(bvid);
                self.peek_hide_now();
            }

            // ---- 设置 ----
            _ => {}
        }
        false
    }

    /// 预览卡生效面:仅首页推荐流(非半窗/非浮层/非全屏/非紧凑卡),且设置允许;
    /// ✕ 的单卡静默在 [`handle_card_hover`](Self::handle_card_hover) 按 bvid 判定。
    pub(in crate::shell) fn peek_allowed(&self) -> bool {
        self.settings.hover_preview
            && self.route == Route::Home
            && !self.is_fullscreen
            && !self.home_half_pane
            && !self.search_overlay_open
            && !self.more_menu_open
            && !self.viewport_layout.cards.compact_card
    }


    /// 立即收起并作废全部在飞定时器(路由切换/尺寸变化/回收等外部作废路径)。
    pub(in crate::shell) fn peek_hide_now(&mut self) {
        self.peek_prefetch_timer = None;
        self.peek_pop_timer = None;
        self.peek_grace_timer = None;
        self.peek_anim_timer = None;
        self.peek_anim_pending = false;
        self.peek_notice = None;
        self.peek.cancel();
    }


    /// 卡片进入/离开上报的统一处置(指针与焦点共用)。
    pub(in crate::shell) fn handle_card_hover(&mut self, report: HoverReport, context: &ComponentContext<Self>) {
        if !self.peek_allowed() {
            return;
        }
        if report.entering {
            // ✕ 关过的卡:指针未离开前不重弹;进入别的卡即清除静默。
            if !self.peek_dismissal.on_enter(&report.bvid) {
                return;
            }
            if std::env::var_os("QAQ_BX_TRACE_HOME").is_some() {
                eprintln!("[qaq-bx-ui] peek enter {}", report.bvid);
            }
            if let Some(seq) = self.peek.on_card_enter(report.bvid, report.rect) {
                self.peek_notice = None;
                self.peek_prefetch_timer =
                    Some(context.set_local_timeout(peek::PEEK_PREFETCH, move || {
                        Message::PeekPrefetchTick(seq)
                    }));
                self.peek_pop_timer = Some(context.set_local_timeout(peek::PEEK_POP, move || {
                    Message::PeekPopTick(seq)
                }));
            }
        } else {
            self.peek_dismissal.on_exit(&report.bvid);
            match self.peek.on_card_exit(&report.bvid) {
                PeekExitAction::CancelPending => {
                    self.peek_prefetch_timer = None;
                    self.peek_pop_timer = None;
                }
                PeekExitAction::StartGrace(seq) => {
                    self.peek_grace_timer =
                        Some(context.set_local_timeout(peek::PEEK_GRACE, move || {
                            Message::PeekGraceTick(seq)
                        }));
                }
                PeekExitAction::Ignored => {}
            }
        }
    }


    /// 1s 弹出:焦点触发的上报没有 pointer 锚点,此刻经焦点元素解析矩形。
    pub(in crate::shell) fn handle_peek_pop(&mut self, seq: u64, context: &ComponentContext<Self>) {
        let Some(bvid) = self.peek.on_pop_tick(seq) else {
            return;
        };
        if !self.peek.is_mounted() {
            let cards = self.viewport_layout.cards;
            match crate::winrt_ext::focused_element_rect(cards.card_width, cards.card_height)
                .map(|(x, y, w, h)| CardRect { x, y, w, h })
            {
                Some(rect) => self.peek.attach_rect(rect),
                None => {
                    self.peek.cancel();
                    return;
                }
            }
        }
        if std::env::var_os("QAQ_BX_TRACE_HOME").is_some() {
            eprintln!("[qaq-bx-ui] peek pop {bvid}");
        }
        // 300ms 预取可能未发(进入即缓存命中/失败),弹出前兜底再试一次。
        self.fetch_peek_desc(&bvid.to_string(), context);
        if self.settings.reduce_animations {
            self.peek_anim_pending = false;
        } else {
            self.peek_anim_pending = true;
            self.peek_anim_timer =
                Some(context.set_local_timeout(peek::PEEK_ANIM_SETTLE, || {
                    Message::PeekAnimTick
                }));
        }
    }


    /// 简介预取(view API;缓存 None 值 = 已失败,不再重发)。
    pub(in crate::shell) fn fetch_peek_desc(&mut self, bvid: &str, context: &ComponentContext<Self>) {
        if self.peek_desc.contains_key(bvid) || !self.peek_desc_inflight.insert(bvid.to_string()) {
            return;
        }
        if self.peek_desc.len() >= MAX_PEEK_DESC_CACHE {
            self.peek_desc.clear();
            self.peek_desc_inflight.clear();
        }
        let auth = self.auth_service.clone();
        let nav = auth.nav_client();
        let owned = bvid.to_string();
        context.spawn_background(move |_| {
            let client = PlayerClient::with_nav(auth.http(), auth.credential_provider(), nav);
            let desc = block_on(client.video_meta(&owned))
                .ok()
                .and_then(|meta| meta.desc)
                .filter(|text| !text.trim().is_empty());
            Message::PeekDescReady {
                bvid: owned,
                desc,
            }
        });
    }


    /// PeekCard 当前挂载内容(探针与视图共用判定)。
    pub(in crate::shell) fn peek_mounted_bvid(&self) -> Option<String> {
        self.peek.mounted_content().map(|(bvid, _)| bvid.to_string())
    }


    pub(in crate::shell) fn take_fresh_preresolved(
        &mut self,
        bvid: &str,
        expected_key: ResolveProfileKey,
    ) -> Option<CachedMedia> {
        let entry = self.preresolved.get(bvid)?.clone();
        if entry.key == expected_key && entry.fetched_at.elapsed() < PRERESOLVE_TTL {
            Some(entry)
        } else {
            self.preresolved.remove(bvid);
            None
        }
    }


    pub(in crate::shell) fn cache_preresolved(&mut self, bvid: String, key: ResolveProfileKey, media: ResolvedMedia) {
        if self.preresolved.contains_key(&bvid) {
            self.preresolved_order.retain(|k| k != &bvid);
        } else if self.preresolved.len() >= MAX_PRERESOLVE_CACHE
            && let Some(evicted) = self.preresolved_order.pop_front()
        {
            self.preresolved.remove(&evicted);
        }
        self.preresolved_order.push_back(bvid.clone());
        self.preresolved.insert(
            bvid,
            CachedMedia {
                key,
                media,
                fetched_at: Instant::now(),
            },
        );
    }


}

