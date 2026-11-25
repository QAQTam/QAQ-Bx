//! 窗口/布局/竖屏域:WindowResized 渲染签名门控 + resize 风暴统计(ResizeStormTrack,
//! 结构体同文件)+ settled 去抖收口、ShellInvalidateLayout、竖屏停靠(TogglePortraitDock/
//! PortraitSynced,窗口约束见 util.rs portrait_window_constraints)。
//!
//! 本文件由 P3 拆解生成:match 臂与配套方法自原 update()/impl AppShell 逐字搬运,
//! 消息路由见 handlers/mod.rs(穷尽 match)。单元测试锚点见 shell/tests.rs。

use super::super::*;

impl AppShell {
    /// window 域消息臂(12 变体,与 message.rs 分组注释对应);未命中变体静默忽略。
    pub(super) fn handle_window(&mut self, message: Message, context: &ComponentContext<Self>) -> bool {
        match message {
            Message::WindowResized(size) => {
                self.track_resize_storm();
                let prev_layout = self.viewport_layout;
                if self.is_fullscreen {
                    // 全屏期间窗口被系统扩至屏幕分辨率(如 1920x1080):冻结底层
                    // `viewport_layout` 与首页网格,避免退出全屏首帧闪回桌面双栏布局
                    // 及连续二次拆装 XAML 视觉树导致卡顿;仅更新全屏 HUD 宽度并探针。
                    if size.width.is_finite() && size.width > 0.0 {
                        self.fullscreen_window_width = size.width;
                    }
                    // 真实全屏高度同步喂给竖屏单列播放页(视频面满铺用),走同一
                    // settled 收敛:dispatch 重绘后单列树按新高度原地改属性。
                    if size.height.is_finite() && size.height > 0.0 {
                        self.fullscreen_window_height = size.height;
                    }
                    // #5:HUD 宽度改按需读取——拖拽帧不再重绘,去抖到期经普通
                    // dispatch 的 settled 消息统一收敛静默累计的新宽度。
                    if self.grid_rebuild_timer.is_none() {
                        self.grid_rebuild_timer =
                            Some(context.set_local_timeout(GRID_REBUILD_DEBOUNCE, || {
                                Message::WindowResizeSettled
                            }));
                    }
                    if !self.fullscreen_sync_in_flight {
                        let gen_id = self.fullscreen_gen;
                        let _ = context.run_window(move |window| {
                            let active =
                                crate::winrt_ext::is_window_fullscreen(window.as_raw()).unwrap_or(true);
                            Message::FullscreenProbeResult { gen_id, active }
                        });
                    }
                    return true;
                }
                let prev_cards = prev_layout.cards;
                let next_layout =
                    ViewportLayout::for_window(size.width, size.height, self.settings.portrait_mode);
                self.viewport_layout = next_layout;
                self.fullscreen_window_width = next_layout.window_width;
                self.home_viewport_w = next_layout.content_width();
                self.home_viewport_h = next_layout.content_height();

                if !next_layout.is_mobile() {
                    self.more_menu_open = false;
                }
                if next_layout.shows_inline_title_search() {
                    self.search_overlay_open = false;
                }
                if !next_layout.uses_whisper_drilldown() {
                    self.whisper_detail_open = false;
                }
                if next_layout.is_mobile() && self.home_half_pane {
                    self.home_half_pane = false;
                    if self.now_playing.is_some() && self.route == Route::Home {
                        self.history.push(Route::Home);
                        self.route = Route::Player;
                        crate::trace::note("route->player (移动布局半屏收起)");
                    }
                }
                let structural = next_layout.should_rebuild_grid(prev_cards);
                if structural || next_layout.metrics_refresh_needed(prev_cards) {
                    // 轻量字段即时更新(探针/分页目标依赖);VirtualSource 重建
                    // 去抖 150ms。#4 后行 key 不含卡宽:纯宽度收口是同 key 原地
                    // diff(改行高/封面高属性),仅结构变化才拆挂重建。
                    self.home_state.grid_cols = next_layout.cards.cols;
                    self.pending_grid_rebuild = true;
                    self.pending_grid_structural |= structural;
                    self.grid_rebuild_timer =
                        Some(context.set_local_timeout(GRID_REBUILD_DEBOUNCE, || {
                            Message::WindowResizeSettled
                        }));
                }
                // #5:silent 尺寸泵下根 view 不会自动重跑;仅结构签名翻转时显式
                // 请求重绘(档内拖框交给 XAML 自动布局,静止后 settled 统一收敛)。
                if resize_render_signature(next_layout)
                    != resize_render_signature(prev_layout)
                {
                    let _ = context.sender().send(Message::ShellInvalidateLayout);
                }
            }
            Message::ShellInvalidateLayout => {
                // 空操作:状态已在 silent 路径落定,dispatch 的重绘即目的。
            }
            Message::WindowResizeSettled => {
                self.close_resize_storm("settled");
                // #7:resize 期间预览锚点已失真(尺寸泵静默,不重绘);静止即收净。
                self.peek_hide_now();
                // §8.2 收口:auto_refresh 关闭后,内容结构档位翻转不经 TitleBar
                // 自身尺寸/属性变化;settled(150ms 静止)时新内容已渲染,这里
                // 补算一次拖拽区。元素未发布/已退役时 reactor 侧静默忽略。
                let tier = title_bar_tier_signature(self.viewport_layout);
                if tier != self.title_bar_tier {
                    self.title_bar_tier = tier;
                    let _ = self.title_bar_ref.recompute_drag_regions();
                }
                self.grid_rebuild_timer = None;
                if self.pending_grid_rebuild {
                    self.pending_grid_rebuild = false;
                    if std::mem::take(&mut self.pending_grid_structural) {
                        // 结构性重建:行重组,游标作废;纯宽度收口保留游标。
                        self.home_active_rows.clear();
                        self.home_anchor_row = 0;
                        self.home_last_evict_row = 0;
                    }
                    self.rebuild_home_source();
                }
            }
            Message::TogglePortraitDock => {
                if !self.settings.portrait_mode {
                    return true;
                }
                let docked = !self.portrait_docked;
                let restore = self.portrait_dock_restore.take();
                let current_dips = self.viewport_layout.window_width;
                let target_dips = PORTRAIT_MAX_WIDTH_DIPS;
                let _ = context.run_window(move |window| {
                    let raw = window.as_raw();
                    if docked {
                        let prev = crate::winrt_ext::dock_window_right(
                            raw,
                            target_dips,
                            current_dips,
                        )
                        .or(restore);
                        Message::PortraitSynced {
                            docked: true,
                            restore: prev,
                        }
                    } else if let Some(prev) = restore {
                        crate::winrt_ext::restore_window_bounds(raw, prev);
                        Message::PortraitSynced {
                            docked: false,
                            restore: None,
                        }
                    } else {
                        Message::PortraitSynced {
                            docked: false,
                            restore: None,
                        }
                    }
                });
            }
            Message::PortraitSynced { docked, restore } => {
                self.portrait_docked = docked;
                self.portrait_dock_restore = restore;
            }
            _ => {}
        }
        false
    }

    /// E14:记录一次 resize;风暴因间隔超限自然收口时输出摘要到 stderr。
    pub(in crate::shell) fn track_resize_storm(&mut self) {
        if let Some(summary) = self.resize_storm.on_resized(Instant::now()) {
            eprintln!("{summary}");
        }
    }


    /// E14:显式收口当前风暴(去抖 settled)。
    pub(in crate::shell) fn close_resize_storm(&mut self, reason: &str) {
        if let Some(summary) = self.resize_storm.close(reason) {
            eprintln!("{summary}");
        }
    }


    /// 竖屏模式:应用/恢复系统最大化没收与 presenter 宽度硬约束(窗口未就绪时逐消息重试)。
    pub(in crate::shell) fn sync_portrait_maximizable(&mut self, context: &ComponentContext<Self>) {
        let Some(enabled) = self.maximizable_pending else {
            return;
        };
        self.maximizable_pending = None;
        let docked = self.portrait_docked;
        let restore = self.portrait_dock_restore;
        let portrait = !enabled;
        let current_dips = self.viewport_layout.window_width;
        let started = context.run_window(move |window| {
            let raw = window.as_raw();
            crate::winrt_ext::set_maximizable(raw, enabled);
            crate::winrt_ext::enforce_portrait_window_policy(
                raw,
                layout::MIN_WINDOW_WIDTH,
                layout::MIN_WINDOW_HEIGHT,
                portrait.then_some(PORTRAIT_MAX_WIDTH_DIPS),
                current_dips,
                portrait,
            );
            Message::PortraitSynced { docked, restore }
        });
        if !started {
            self.maximizable_pending = Some(enabled);
        }
    }


}

/// E14:拖框风暴跟踪——连续 `WindowResized`(间隔 ≤250ms)视为一次风暴;收口时输出
/// `RESIZE-STORM n=… span_ms=… max_gap_ms=…` 到 stderr,供 E14 场景量化 UI 线程
/// 节拍(F1 合并生效度:n 即风暴期实际消费的消息数,对比脚本侧产出的步数)。
/// 纯计数器,不改变任何处理路径。
#[derive(Debug, Default)]
pub(in crate::shell) struct ResizeStormTrack {
    pub(in crate::shell) open: Option<ResizeStorm>,
}


#[derive(Debug)]
pub(in crate::shell) struct ResizeStorm {
    pub(in crate::shell) start: Instant,
    pub(in crate::shell) last: Instant,
    pub(in crate::shell) count: u32,
    pub(in crate::shell) max_gap_ms: f64,
}


impl ResizeStormTrack {
    /// 事件间隔超过该值视为风暴结束。
    const STORM_GAP_MS: f64 = 250.0;
    /// 孤立 resize 不输出(阈值防噪声)。
    const MIN_BURST: u32 = 3;

    /// 记录一次 resize;若上一场风暴已因间隔超限自然结束,先返回其摘要。
    pub(in crate::shell) fn on_resized(&mut self, now: Instant) -> Option<String> {
        let closed = self.close_stale(now);
        match &mut self.open {
            Some(storm) => {
                let gap = now.duration_since(storm.last).as_secs_f64() * 1000.0;
                storm.max_gap_ms = storm.max_gap_ms.max(gap);
                storm.last = now;
                storm.count += 1;
            }
            None => {
                self.open = Some(ResizeStorm {
                    start: now,
                    last: now,
                    count: 1,
                    max_gap_ms: 0.0,
                });
            }
        }
        closed
    }

    /// 显式收口(去抖 settled / 新风暴开槽);不足 `MIN_BURST` 不输出。
    pub(in crate::shell) fn close(&mut self, reason: &str) -> Option<String> {
        let storm = self.open.take()?;
        if storm.count < Self::MIN_BURST {
            return None;
        }
        let span = storm.last.duration_since(storm.start).as_secs_f64() * 1000.0;
        Some(format!(
            "RESIZE-STORM n={} span_ms={:.0} max_gap_ms={:.0} ({reason})",
            storm.count, span, storm.max_gap_ms
        ))
    }

    /// 间隔超限的上一场风暴收口。
    pub(in crate::shell) fn close_stale(&mut self, now: Instant) -> Option<String> {
        let last = self.open.as_ref()?.last;
        let gap = now.duration_since(last).as_secs_f64() * 1000.0;
        if gap > Self::STORM_GAP_MS {
            self.close("gap")
        } else {
            None
        }
    }
}


