//! 手柄域:GamepadInput(Some/None) 事件流、handle_gamepad_action 焦点导航与快捷键、
//! focused_home_video 焦点解析。
//!
//! 本文件由 P3 拆解生成:match 臂与配套方法自原 update()/impl AppShell 逐字搬运,
//! 消息路由见 handlers/mod.rs(穷尽 match)。单元测试锚点见 shell/tests.rs。

use super::super::*;

impl AppShell {
    /// gamepad 域消息臂(2 变体,与 message.rs 分组注释对应);未命中变体静默忽略。
    pub(super) fn handle_gamepad(&mut self, message: Message, context: &ComponentContext<Self>) -> bool {
        match message {
            Message::GamepadInput(Some(ev)) => {
                match ev {
                    GamepadEvent::Connected(connected) => {
                        if self.gamepad_active != connected {
                            self.gamepad_active = connected;
                            self.rebuild_home_source();
                        }
                    }
                    GamepadEvent::Action(action) => {
                        if !self.gamepad_active {
                            self.gamepad_active = true;
                            self.rebuild_home_source();
                        }
                        self.handle_gamepad_action(action, context);
                    }
                }
                Self::listen_gamepad(&self.gamepad_rx, context);
            }
            Message::GamepadInput(None) => {}

            // ---- 首页推荐流与 Hover 预解析 ----
            _ => {}
        }
        false
    }

    pub(in crate::shell) fn listen_gamepad(
        rx: &Arc<Mutex<std::sync::mpsc::Receiver<GamepadEvent>>>,
        context: &ComponentContext<Self>,
    ) {
        let rx = Arc::clone(rx);
        context.spawn_background(move |_| {
            let ev = rx.lock().ok().and_then(|guard| guard.recv().ok());
            Message::GamepadInput(ev)
        });
    }


    pub(in crate::shell) fn focused_home_video(&self) -> Option<&VideoItem> {
        let mut videos = self.home_slots.iter().filter_map(|slot| match slot {
            HomeSlot::Video { item, .. } => Some(&**item),
            HomeSlot::Skeleton { .. } => None,
        });
        let count = self
            .home_slots
            .iter()
            .filter(|slot| matches!(slot, HomeSlot::Video { .. }))
            .count();
        if count == 0 {
            return None;
        }
        videos.nth(self.home_focus_idx.min(count - 1))
    }


    pub(in crate::shell) fn handle_gamepad_action(&mut self, action: GamepadAction, context: &ComponentContext<Self>) {
        match action {
            GamepadAction::PrevTab => self.cycle_nav_route(-1),
            GamepadAction::NextTab => self.cycle_nav_route(1),
            GamepadAction::Back => {
                if self.is_fullscreen {
                    self.exit_fullscreen(context);
                } else {
                    self.navigate_back();
                }
            }
            GamepadAction::TogglePlay => {
                self.update(Message::TogglePlay, context);
            }
            GamepadAction::Refresh => {
                if self.is_player_visible() {
                    self.update(Message::ToggleFullscreen, context);
                } else if self.route == Route::Home {
                    self.update(Message::HomeRefresh, context);
                    self.home_focus_idx = 0;
                }
            }
            GamepadAction::Confirm => {
                if self.route == Route::Player {
                    self.update(Message::TogglePlay, context);
                } else if self.route == Route::Home
                    && let Some(bvid) = self.focused_home_video().map(|v| v.bvid.clone())
                {
                    self.open_video(&bvid, context);
                    self.sync_smtc(context);
                }
            }
            GamepadAction::Left => {
                if self.route == Route::Player {
                    let target = f64::from(self.position.saturating_sub(10));
                    self.update(Message::Seek(target), context);
                } else if self.route == Route::Home {
                    let next = self.home_focus_idx.saturating_sub(1);
                    if next != self.home_focus_idx {
                        self.home_focus_idx = next;
                        self.rebuild_home_source();
                    }
                }
            }
            GamepadAction::Right => {
                if self.route == Route::Player {
                    let target = f64::from(self.position.saturating_add(10));
                    self.update(Message::Seek(target), context);
                } else if self.route == Route::Home {
                    let count = self
                        .home_slots
                        .iter()
                        .filter(|s| matches!(s, HomeSlot::Video { .. }))
                        .count();
                    if count > 0 {
                        let next = (self.home_focus_idx + 1).min(count - 1);
                        if next != self.home_focus_idx {
                            self.home_focus_idx = next;
                            self.rebuild_home_source();
                        }
                    }
                }
            }
            GamepadAction::Up => {
                if self.route == Route::Player {
                    let vol = f64::from((self.volume + 5).min(100));
                    self.update(Message::SetVolume(vol), context);
                } else if self.route == Route::Home {
                    let cols = self.home_state.grid_cols.max(1);
                    let next = self.home_focus_idx.saturating_sub(cols);
                    if next != self.home_focus_idx {
                        self.home_focus_idx = next;
                        self.rebuild_home_source();
                        let step = self.viewport_layout.cards.cell_height();
                        if !crate::winrt_ext::scroll_focused_viewer_by(-step) {
                            gamepad::inject_mouse_wheel(240);
                        }
                    }
                }
            }
            GamepadAction::Down => {
                if self.route == Route::Player {
                    let vol = f64::from(self.volume.saturating_sub(5));
                    self.update(Message::SetVolume(vol), context);
                } else if self.route == Route::Home {
                    let cols = self.home_state.grid_cols.max(1);
                    let count = self
                        .home_slots
                        .iter()
                        .filter(|s| matches!(s, HomeSlot::Video { .. }))
                        .count();
                    if count > 0 {
                        let next = (self.home_focus_idx + cols).min(count - 1);
                        if next != self.home_focus_idx {
                            self.home_focus_idx = next;
                            self.rebuild_home_source();
                            let step = self.viewport_layout.cards.cell_height();
                            if !crate::winrt_ext::scroll_focused_viewer_by(step) {
                                gamepad::inject_mouse_wheel(-240);
                            }
                        }
                    }
                }
            }
        }
    }


}

