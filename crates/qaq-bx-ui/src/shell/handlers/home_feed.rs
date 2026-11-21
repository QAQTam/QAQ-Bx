//! 首页 feed 域:HomeLoaded 吸收(骨架槽分配/去重/合并)、行激活(HomeRowActive)、
//! 刷新与重试(HomeRefresh/HomeRetry/HomeFailed)、预拉批(spawn_feed_batch/apply_feed_page)、
//! 挂载脉冲(mount_pulse_active)。
//!
//! 本文件由 P3 拆解生成:match 臂与配套方法自原 update()/impl AppShell 逐字搬运,
//! 消息路由见 handlers/mod.rs(穷尽 match)。单元测试锚点见 shell/tests.rs。

use super::super::*;

impl AppShell {
    /// home_feed 域消息臂(5 变体,与 message.rs 分组注释对应);未命中变体静默忽略。
    pub(super) fn handle_home_feed(&mut self, message: Message, context: &ComponentContext<Self>) -> bool {
        match message {
            Message::HomeRefresh => {
                self.feed_gen = self.feed_gen.wrapping_add(1);
                self.peek_hide_now();
                self.feed_epoch.store(self.feed_gen, Ordering::SeqCst);
                self.image_cache.clear();
                self.pending_covers.clear();
                self.cover_inflight.clear();
                self.cover_flush_timer = None;
                self.home_anchor_row = 0;
                self.home_last_evict_row = 0;
                self.home_active_rows.clear();
                self.home_slots.clear();
                self.home_slot_by_bvid.clear();
                self.home_index.clear();
                self.home_next_slot_seq = 0;
                self.home_pending_skeletons = 0;
                self.home_state.stream_gen = self.feed_gen;
                self.home_state.grid_cols = self.viewport_layout.cards.cols;
                self.home_state.page_count = 0;
                self.home_state.video_count = 0;
                self.home_state.reached_end = false;
                self.home_state.error = None;

                let target = feed_pager::logical_page_target_for(
                    self.viewport_layout.cards,
                    self.home_viewport_h,
                );
                self.allocate_skeleton_slots(target);

                if self.home_state.loading {
                    self.home_refresh_pending = true;
                    return true;
                }
                self.home_refresh_pending = false;
                self.request_feed_batch(FeedKind::Refresh, target, true, context);
            }
            Message::HomeRowActive {
                rev,
                row_idx,
                active,
            } => {
                if rev != self.current_row_revision() {
                    return true;
                }
                if active {
                    self.home_active_rows.insert(row_idx);
                } else {
                    self.home_active_rows.remove(&row_idx);
                }
                let cols = self.home_state.grid_cols.max(1);
                let total_rows = self.home_slots.len().div_ceil(cols);
                // #7:预览卡所在行被回收(滚动划出)→ 锚点失效,立即收起。
                if !active
                    && let Some(idx) = self
                        .peek_mounted_bvid()
                        .and_then(|bvid| self.home_slot_by_bvid.get(&bvid).copied())
                    && idx / cols == row_idx
                {
                    self.peek_hide_now();
                }
                self.home_anchor_row = self
                    .home_active_rows
                    .iter()
                    .next_back()
                    .copied()
                    .unwrap_or(0)
                    .min(total_rows.saturating_sub(1));

                let vis_rows =
                    feed_pager::visible_rows_for(self.viewport_layout.cards, self.home_viewport_h);
                let evict_stride = vis_rows.saturating_mul(2).max(4);
                if self.home_anchor_row.abs_diff(self.home_last_evict_row) >= evict_stride {
                    self.sync_offscreen_covers(context);
                }

                if active {
                    if std::env::var_os("QAQ_BX_TRACE_HOME").is_some() {
                        eprintln!(
                            "[qaq-bx-ui] row active: row={}/{} anchor={} active_cnt={} slots={} loading={}",
                            row_idx,
                            total_rows,
                            self.home_anchor_row,
                            self.home_active_rows.len(),
                            self.home_slots.len(),
                            self.home_state.loading
                        );
                    }
                    self.maybe_prefetch_next_page(context);
                }
            }
            Message::HomeRetry => {
                let replace = matches!(self.home_last_kind, FeedKind::First | FeedKind::Refresh);
                let target = if self.home_last_target > 0 {
                    self.home_last_target
                } else {
                    feed_pager::logical_page_target_for(
                        self.viewport_layout.cards,
                        self.home_viewport_h,
                    )
                };
                if replace {
                    self.home_slots.clear();
                    self.home_slot_by_bvid.clear();
                    self.home_index.clear();
                    self.home_next_slot_seq = 0;
                    self.home_pending_skeletons = 0;
                }
                self.allocate_skeleton_slots(target);
                self.request_feed_batch(self.home_last_kind, target, replace, context);
            }
            Message::HomeLoaded {
                seq,
                cards,
                exhausted,
                replace,
            } => {
                self.home_state.loading = false;
                // #7:换一换/清流整源重建,在飞预览锚点即刻失效。
                if replace {
                    self.peek_hide_now();
                }
                if self.home_refresh_pending {
                    self.home_refresh_pending = false;
                    let target =
                        self.home_pending_skeletons
                            .max(feed_pager::logical_page_target_for(
                                self.viewport_layout.cards,
                                self.home_viewport_h,
                            ));
                    self.request_feed_batch(FeedKind::Refresh, target, true, context);
                    return true;
                }
                if seq != self.feed_gen {
                    return true;
                }
                if replace && cards.is_empty() && !exhausted {
                    self.truncate_unfilled_skeletons();
                    self.home_state.error = Some("推荐流暂无内容，点击重试".to_string());
                    return true;
                }
                self.home_state.error = None;
                if !cards.is_empty() {
                    self.home_state.page_count = if replace {
                        1
                    } else {
                        self.home_state.page_count + 1
                    };
                }

                // 原地填入已分配的骨架槽位框架；未填满尾部多余槽位一并收起。
                let mut cursor = self
                    .home_slots
                    .iter()
                    .position(|slot| matches!(slot, HomeSlot::Skeleton { .. }))
                    .unwrap_or(self.home_slots.len());

                for mut item in cards {
                    if let Some(url) = item.pic_url.clone() {
                        if let Some(cached) = self.image_cache.get(&url) {
                            item.cover = Some(cached);
                        } else {
                            self.spawn_cover_fetch(item.bvid.clone(), url, seq, context);
                        }
                    }

                    let bvid = item.bvid.clone();
                    self.home_index.insert(bvid.clone(), item.clone());
                    // #6:槽位共享 Rc<VideoItem>,行分块复制只碰引用计数;
                    // 封面驱逐/回填 copy-on-write 仅触及的条目发生真实拷贝。
                    let item = Rc::new(item);

                    if cursor < self.home_slots.len() {
                        let slot_key = self.home_slots[cursor].slot_key().to_string();
                        self.home_slots[cursor] = HomeSlot::Video { slot_key, item };
                        self.home_slot_by_bvid.insert(bvid, cursor);
                        cursor += 1;
                    } else {
                        let next_seq = self.home_next_slot_seq;
                        self.home_next_slot_seq += 1;
                        let slot_key = format!("slot-{}-{}", self.feed_gen, next_seq);
                        self.home_slots.push(HomeSlot::Video { slot_key, item });
                        self.home_slot_by_bvid.insert(bvid, cursor);
                        cursor += 1;
                    }
                }

                // 收起该批未用到或流耗尽后的多余骨架槽位，防止底部出现死区。
                if cursor < self.home_slots.len() {
                    self.home_slots.truncate(cursor);
                }
                self.home_pending_skeletons = 0;
                self.home_state.video_count = self.home_slots.len();
                self.rebuild_home_source();
                if std::env::var_os("QAQ_BX_TRACE_HOME").is_some() {
                    eprintln!(
                        "[qaq-bx-ui] batch loaded: total_videos={}, page_count={}, exhausted={}",
                        self.home_state.video_count, self.home_state.page_count, exhausted
                    );
                }
                if exhausted || self.home_slots.len() >= MAX_HOME_SLOTS {
                    self.home_state.reached_end = true;
                } else {
                    let cols = self.home_state.grid_cols.max(1);
                    let total_rows = self.home_slots.len().div_ceil(cols);
                    self.home_anchor_row = self
                        .home_active_rows
                        .iter()
                        .next_back()
                        .copied()
                        .unwrap_or(0)
                        .min(total_rows.saturating_sub(1));
                    self.maybe_prefetch_next_page(context);
                }
            }
            Message::HomeFailed { seq, reason } => {
                self.home_state.loading = false;
                if self.home_refresh_pending {
                    self.home_refresh_pending = false;
                    let target =
                        self.home_pending_skeletons
                            .max(feed_pager::logical_page_target_for(
                                self.viewport_layout.cards,
                                self.home_viewport_h,
                            ));
                    self.request_feed_batch(FeedKind::Refresh, target, true, context);
                    return true;
                }
                if seq != self.feed_gen {
                    return true;
                }
                self.truncate_unfilled_skeletons();
                self.home_state.error = Some(reason);
            }
            _ => {}
        }
        false
    }

    pub(in crate::shell) fn current_row_revision(&self) -> u64 {
        video_card::home_row_revision(self.home_state.stream_gen, self.viewport_layout.cards)
    }


    pub(in crate::shell) fn rebuild_home_source(&mut self) {
        let rev = self.current_row_revision();
        let focused = self.gamepad_active.then_some(self.home_focus_idx);
        self.home_source = video_card::home_virtual_source(
            &self.home_slots,
            self.viewport_layout.cards,
            rev,
            focused,
            &self.home_on_open,
            &self.home_on_hover,
            &self.home_on_hover_report,
            &self.home_on_row_active,
            &self.unplayable,
        );
    }

    // ---- #7 悬停预览卡生命周期 ----


    pub(in crate::shell) fn maybe_prefetch_next_page(&mut self, context: &ComponentContext<Self>) {
        if self.home_state.loading
            || self.home_state.error.is_some()
            || self.home_state.reached_end
            || self.home_refresh_pending
            || self.home_slots.len() >= MAX_HOME_SLOTS
        {
            return;
        }
        let cols = self.home_state.grid_cols.max(1);
        let total_rows = self.home_slots.len().div_ceil(cols);
        if total_rows == 0 || self.home_anchor_row + 1 + HOME_PREFETCH_LEAD_ROWS < total_rows {
            return;
        }
        let batch_size = feed_pager::aligned_next_batch_for(
            self.home_slots.len(),
            self.viewport_layout.cards,
            self.home_viewport_h,
        )
        .min(MAX_HOME_SLOTS.saturating_sub(self.home_slots.len()));
        if batch_size > 0 {
            self.allocate_skeleton_slots(batch_size);
            self.request_feed_batch(FeedKind::Next, batch_size, false, context);
        }
    }


    /// 展示总长(秒):引擎 duration 优先,卡片数据兜底。
    pub(in crate::shell) fn display_duration(&self, item: &VideoItem) -> u32 {
        self.engine_duration
            .map(|d| d.round().max(0.0) as u32)
            .unwrap_or(item.duration_secs)
    }


    /// 预分配固定尺寸骨架槽位框架（先框架、后填数据，保证布局几何稳定）。
    pub(in crate::shell) fn allocate_skeleton_slots(&mut self, count: usize) {
        let gen_id = self.feed_gen;
        for _ in 0..count {
            let seq = self.home_next_slot_seq;
            self.home_next_slot_seq += 1;
            self.home_slots.push(HomeSlot::Skeleton {
                slot_key: format!("slot-{gen_id}-{seq}"),
            });
        }
        self.home_pending_skeletons += count;
        self.rebuild_home_source();
    }


    /// 收起尾部尚未填充的骨架槽位（流耗尽定格最后一页或出错撤回时调用）。
    pub(in crate::shell) fn truncate_unfilled_skeletons(&mut self) {
        let before = self.home_slots.len();
        while matches!(self.home_slots.last(), Some(HomeSlot::Skeleton { .. })) {
            self.home_slots.pop();
        }
        self.home_pending_skeletons = 0;
        if self.home_slots.len() != before {
            self.rebuild_home_source();
        }
    }


    /// 发起一次逻辑页批请求：视口自适应目标条数 + 合批 `ps<=30` + 代次回投。
    pub(in crate::shell) fn request_feed_batch(
        &mut self,
        kind: FeedKind,
        target_count: usize,
        replace: bool,
        context: &ComponentContext<Self>,
    ) {
        if self.home_state.loading {
            return;
        }
        self.home_state.loading = true;
        self.home_state.error = None;
        self.home_last_kind = kind;
        self.home_last_target = target_count;
        spawn_feed_batch(
            &self.feed_client,
            &self.feed_session,
            kind,
            target_count,
            replace,
            self.feed_gen,
            context,
        );
    }


}

pub(in crate::shell) fn apply_feed_page<T>(
    slice: &mut ListSliceState<T>,
    cursor_slot: &mut Option<FeedCursor>,
    append: bool,
    res: Result<qaq_bx_core::notify::FeedPage<T>, String>,
    avatar_of: impl Fn(&T) -> &str,
) -> Vec<String> {
    slice.loading = false;
    slice.loaded_once = true;
    match res {
        Ok(page) => {
            let urls: Vec<String> = page
                .items
                .iter()
                .map(|i| avatar_of(i).to_string())
                .collect();
            if append {
                slice.items.extend(page.items);
            } else {
                slice.items = page.items;
            }
            *cursor_slot = page.next_cursor;
            slice.has_more = page.has_more;
            slice.error = None;
            urls
        }
        Err(err) => {
            slice.error = Some(err);
            Vec::new()
        }
    }
}


/// 挂载脉冲门控:全播放页 **或** 首页半屏抽屉打开时,swapchain 未挂载成功
/// 就要持续重试(250ms 一拍经 `mount_tick` 驱动 `VideoSurface::try_mount`)。
/// 半屏抽屉 route 停留在 Home——旧门控只认 `route == Player`,冷启动时面板
/// metrics 抢在引擎产出 swapchain 之前触发一次就再无第二次机会,首个视频
/// 永久黑屏;`ClosePlayer` 丢弃引擎后重开又新建引擎,同样复发。
pub(in crate::shell) fn mount_pulse_active(
    route: Route,
    home_half_pane: bool,
    is_fullscreen: bool,
    engine_attached: bool,
    park: bool,
) -> bool {
    // S2 tier1:park(播放层垫底)也算可见——后台装载/注入必须被 250ms 节拍
    // 持续驱动,否则在设置页切引擎后的定点续播要等到回播放页才注入。
    (park || route == Route::Player || home_half_pane || is_fullscreen) && !engine_attached
}


pub(in crate::shell) fn spawn_feed_batch(
    feed_client: &Arc<FeedClient>,
    feed_session: &Arc<Mutex<FeedSession>>,
    kind: FeedKind,
    target_count: usize,
    replace: bool,
    seq: u64,
    context: &ComponentContext<AppShell>,
) {
    let client = feed_client.clone();
    let session = feed_session.clone();
    context.spawn_background(move |_| {
        match block_on(fetch_rcmd_batch(&client, &session, kind, target_count)) {
            Ok(batch) => Message::HomeLoaded {
                seq,
                cards: batch.cards.iter().map(video_item).collect(),
                exhausted: batch.exhausted,
                replace,
            },
            Err(error) => Message::HomeFailed {
                seq,
                reason: error.to_string(),
            },
        }
    });
}


