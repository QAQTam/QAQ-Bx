//! 设置与存储域:UpdateSettings 全量设置写回(引擎/画质/弹幕/竖屏即时生效链)、
//! 300ms 防抖落盘(FlushSettingsSave/SettingsSaved)、缓存一键清空(ClearCaches→CachesCleared,
//! 白名单纪律:credentials/settings/非 .bin 绝不动,见 docs/Update-0.6.0.md D 项)。
//!
//! 本文件由 P3 拆解生成:match 臂与配套方法自原 update()/impl AppShell 逐字搬运,
//! 消息路由见 handlers/mod.rs(穷尽 match)。单元测试锚点见 shell/tests.rs。

use super::super::*;

impl AppShell {
    /// settings 域消息臂(8 变体,与 message.rs 分组注释对应);未命中变体静默忽略。
    pub(super) fn handle_settings(&mut self, message: Message, context: &ComponentContext<Self>) -> bool {
        match message {
            Message::ClearCaches => {
                // 内存层立即清(封面/头像 LRU + 待flush批次);已挂到卡片上的
                // 位图与卡片共享 Arc,继续显示不受影响。磁盘层+票据走后台线程。
                self.image_cache.clear();
                self.avatar_cache.clear();
                self.pending_covers.clear();
                self.cache_notice = None;
                let auth = self.auth_service.clone();
                context.spawn_background(move |_| {
                    Message::CachesCleared(auth.clear_caches_blocking())
                });
            }
            Message::CachesCleared(freed) => {
                self.cache_notice = Some(format!(
                    "已清空缓存,释放 {}",
                    pages::settings::fmt_bytes(freed)
                ));
            }
            Message::UpdateSettings(next) => {
                let next = next.normalized();
                if next == self.settings {
                    return true;
                }
                // #7:预览开关/动画开关翻转:收掉在飞预览;重新开启预览解除手动静默。
                if next.hover_preview != self.settings.hover_preview
                    || next.reduce_animations != self.settings.reduce_animations
                {
                    self.peek_hide_now();
                    if next.hover_preview {
                        self.peek_dismissal.reset();
                    }
                }
                // 竖屏模式(实验)翻转:切换系统最大化没收与宽度硬约束;
                // 开启时当帧将 `viewport_layout` 收敛入竖屏宽度带 [390, 859] 并同步重建网格,
                // 关闭时若在贴边态先还原贴边前几何,再交由 pending 恢复最大化钮。
                if next.portrait_mode != self.settings.portrait_mode {
                    self.maximizable_pending = Some(!next.portrait_mode);
                    if next.portrait_mode {
                        let prev_cards = self.viewport_layout.cards;
                        let next_layout = ViewportLayout::for_window(
                            self.viewport_layout.window_width,
                            self.viewport_layout.window_height,
                            true,
                        );
                        self.viewport_layout = next_layout;
                        if !self.is_fullscreen {
                            self.fullscreen_window_width = next_layout.window_width;
                        }
                        self.home_viewport_w = next_layout.content_width();
                        self.home_viewport_h = next_layout.content_height();
                        self.home_half_pane = false;
                        let structural = next_layout.should_rebuild_grid(prev_cards);
                        if structural || next_layout.metrics_refresh_needed(prev_cards) {
                            self.home_state.grid_cols = next_layout.cards.cols;
                            self.pending_grid_rebuild = false;
                            self.pending_grid_structural = false;
                            self.grid_rebuild_timer = None;
                            if structural {
                                self.home_active_rows.clear();
                                self.home_anchor_row = 0;
                                self.home_last_evict_row = 0;
                            }
                            self.rebuild_home_source();
                        }
                    } else if self.portrait_docked {
                        let restore = self.portrait_dock_restore.take();
                        self.portrait_docked = false;
                        if let Some(prev) = restore {
                            context.run_window(move |window| {
                                crate::winrt_ext::restore_window_bounds(window.as_raw(), prev);
                                Message::PortraitSynced {
                                    docked: false,
                                    restore: None,
                                }
                            });
                        }
                    }
                    self.sync_portrait_maximizable(context);
                }
                // M7:切引擎要在写入新设置**之后**判定(生效引擎取自 settings)。
                let engine_before = self.effective_player_engine();
                if next.volume != self.settings.volume {
                    self.volume = next.volume;
                    if let Some(player) = self.native_sink() {
                        let _ = player.set_volume(f64::from(next.volume));
                    } else if let Some(engine) = &self.engine {
                        engine.set_volume(f64::from(next.volume));
                    }
                }
                if next.hardware_decode != self.settings.hardware_decode
                    && let Some(engine) = &self.engine
                {
                    engine.set_hardware_decode(next.hardware_decode);
                }
                let danmaku_style_changed = next.danmaku_enabled != self.settings.danmaku_enabled
                    || (next.danmaku_opacity - self.settings.danmaku_opacity).abs() > f32::EPSILON
                    || next.danmaku_max_lanes != self.settings.danmaku_max_lanes
                    || next.danmaku_font_family != self.settings.danmaku_font_family
                    || (next.danmaku_font_scale - self.settings.danmaku_font_scale).abs()
                        > f32::EPSILON;
                if danmaku_style_changed {
                    // 样式下发按当前承接方分流(native 透明层 / mpv 视频引擎)。
                    self.danmaku_config(
                        next.danmaku_enabled,
                        next.danmaku_opacity,
                        next.danmaku_max_lanes,
                        &next.danmaku_font_family,
                        next.danmaku_font_scale,
                    );
                }
                if danmaku_filter_from(&next) != danmaku_filter_from(&self.settings) {
                    // 规则变化 → 承接线程编译并对已缓存弹幕全量重滤(热更新)。
                    self.danmaku_filter(danmaku_filter_from(&next));
                }
                let transport_changed = next.prefer_official_cdn != self.settings.prefer_official_cdn
                    || next.multi_thread_preload != self.settings.multi_thread_preload;
                self.settings = next;
                // M7:播放引擎变更 → 收口(只记账,下一次装载按新引擎;直播不受影响)。
                let engine_changed = self.effective_player_engine() != engine_before;
                if engine_changed {
                    self.switch_player_engine(context);
                } else if transport_changed
                    && self.now_playing.is_some()
                    && self.current_qn != 0
                    && !self.current_is_live()
                    && let Some(choice) = self.current_choice()
                {
                    let start = if self.native_playback {
                        self.native_position().max(f64::from(self.position))
                    } else {
                        self.engine
                            .as_ref()
                            .map(|engine| engine.status().time_pos)
                            .unwrap_or(0.0)
                            .max(f64::from(self.position))
                    };
                    let want_play = self.playing;
                    if !self.native_load_choice(&choice, want_play, start) {
                        self.mpv_load_choice(&choice, start, want_play, context);
                    }
                }
                self.schedule_settings_save(context);
            }
            Message::FlushSettingsSave => {
                self.settings_save_timer = None;
                let store = self.settings_store.clone();
                let snapshot = self.settings.clone();
                context.spawn_background(move |_| {
                    Message::SettingsSaved(store.save(&snapshot).err().map(|e| e.to_string()))
                });
            }
            Message::SettingsSaved(err) => {
                self.settings_notice = err.map(|e| format!("保存设置失败：{e}"));
            }

            // ---- 登录 ----
            _ => {}
        }
        false
    }

    /// 去抖调度配置落盘（滑块拖动不频繁写盘，稳定后 300ms 原子写一次）。
    pub(in crate::shell) fn schedule_settings_save(&mut self, context: &ComponentContext<Self>) {
        self.settings_save_timer =
            Some(context.set_local_timeout(SETTINGS_SAVE_DEBOUNCE, || Message::FlushSettingsSave));
    }


}

