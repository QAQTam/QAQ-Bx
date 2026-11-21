//! 导航与全局浮层域:路由切换(NavTagSelected/NavigateCompact/Back)、更多菜单与
//! 搜索浮层开关、抽屉 Pane(PaneOpenChanged/TogglePane)、toast 入口(ToastActivated)、
//! 开播入口(open_video/open_video_target)、演示与 selftest 杂项(DemoTick/SelfTestSync)。
//!
//! 本文件由 P3 拆解生成:match 臂与配套方法自原 update()/impl AppShell 逐字搬运,
//! 消息路由见 handlers/mod.rs(穷尽 match)。单元测试锚点见 shell/tests.rs。

use super::super::*;

impl AppShell {
    /// nav 域消息臂(14 变体,与 message.rs 分组注释对应);未命中变体静默忽略。
    pub(super) fn handle_nav(&mut self, message: Message, context: &ComponentContext<Self>) -> bool {
        match message {
            Message::Back => {
                if self.is_fullscreen {
                    self.exit_fullscreen(context);
                } else {
                    self.navigate_back();
                }
            }
            Message::NavTagSelected(tag) => {
                self.exit_fullscreen(context);
                self.logout_confirm = false;
                self.more_menu_open = false;
                self.search_overlay_open = false;
                if let Some(route) = tag.as_deref().and_then(Route::from_tag)
                    && route != self.route
                {
                    self.whisper_detail_open = false;
                    self.history.push(self.route);
                    self.route = route;
                    crate::trace::note(format!("route->{} (页签点击)", route.tag()));
                    if route == Route::Login {
                        self.ensure_account_loaded(context);
                    }
                    if route == Route::Popular {
                        self.ensure_popular(context);
                    }
                    if route == Route::Live {
                        self.ensure_live(context);
                    }
                    if route == Route::History {
                        self.ensure_history(context);
                    }
                    if route == Route::WatchLater {
                        self.ensure_watch_later(context);
                    }
                    if route == Route::Favorites {
                        self.ensure_favorites(context);
                    }
                }
            }
            Message::PaneOpenChanged(open) => self.pane_open = open,
            Message::TogglePane => self.pane_open = !self.pane_open,
            #[cfg(feature = "selftest")]
            Message::SelfTestSync => {}
            Message::DemoTick => {
                self.handle_demo_tick(context);
            }
            Message::ToastActivated(true) => {
                if let Some(rx) = self.toast_rx.clone() {
                    Self::listen_toast(&rx, context);
                }
                self.update(Message::OpenNotifications, context);
            }
            Message::ToastActivated(false) => {}
            Message::ToggleMoreMenu => {
                self.search_overlay_open = false;
                self.more_menu_open = !self.more_menu_open;
            }
            Message::CloseMoreMenu => {
                self.more_menu_open = false;
            }
            Message::ToggleSearchOverlay => {
                self.more_menu_open = false;
                self.search_overlay_open = !self.search_overlay_open;
            }
            Message::CloseSearchOverlay => {
                self.search_overlay_open = false;
            }
            Message::NavigateCompact(target) => {
                self.exit_fullscreen(context);
                self.more_menu_open = false;
                self.search_overlay_open = false;
                self.home_half_pane = false;
                self.logout_confirm = false;
                if self.route != target {
                    self.whisper_detail_open = false;
                    self.history.push(self.route);
                    self.route = target;
                    crate::trace::note(format!("route->{} (菜单导航)", target.tag()));
                    if target == Route::Login && self.auth_gen > 0 {
                        self.ensure_account_loaded(context);
                    }
                    if target == Route::Popular {
                        self.ensure_popular(context);
                    }
                    if target == Route::Live {
                        self.ensure_live(context);
                    }
                    if target == Route::History {
                        self.ensure_history(context);
                    }
                    if target == Route::WatchLater {
                        self.ensure_watch_later(context);
                    }
                    if target == Route::Favorites {
                        self.ensure_favorites(context);
                    }
                }
            }
            _ => {}
        }
        false
    }

    pub(in crate::shell) fn pick_demo_bvid(&self) -> String {
        self.home_slots
            .iter()
            .find_map(|slot| match slot {
                HomeSlot::Video { item, .. } => Some(item.bvid.clone()),
                _ => None,
            })
            .or_else(|| self.popular.items.first().map(|item| item.bvid.clone()))
            .or_else(|| {
                self.search_state
                    .videos
                    .first()
                    .map(|item| item.bvid.clone())
            })
            .unwrap_or_else(|| "BV1GJ411x7h7".to_string())
    }


    pub(in crate::shell) fn handle_demo_tick(&mut self, context: &ComponentContext<Self>) {
        let actions = {
            let Some(demo) = self.demo.as_mut() else {
                self.demo_timer = None;
                return;
            };
            demo.step()
        };

        for action in actions {
            match action {
                crate::demo::DemoAction::OpenPopular => {
                    self.logout_confirm = false;
                    self.more_menu_open = false;
                    self.search_overlay_open = false;
                    self.home_half_pane = false;
                    if self.route != Route::Popular {
                        self.whisper_detail_open = false;
                        self.history.push(self.route);
                        self.route = Route::Popular;
                        crate::trace::note("route->popular (半屏详情关闭)");
                        self.ensure_popular(context);
                    }
                }
                crate::demo::DemoAction::ToggleMoreMenu => {
                    self.search_overlay_open = false;
                    self.more_menu_open = true;
                }
                crate::demo::DemoAction::CloseMoreMenu => {
                    self.more_menu_open = false;
                }
                crate::demo::DemoAction::OpenSearchOverlay => {
                    self.more_menu_open = false;
                    self.search_overlay_open = true;
                }
                crate::demo::DemoAction::SetSearchDraft(draft) => {
                    self.search_state.draft = draft.to_string();
                }
                crate::demo::DemoAction::SubmitSearch => {
                    self.submit_search(context);
                }
                crate::demo::DemoAction::OpenDemoVideo => {
                    let bvid = self.pick_demo_bvid();
                    self.more_menu_open = false;
                    self.search_overlay_open = false;
                    self.home_half_pane = false;
                    if let Some(demo) = self.demo.as_mut() {
                        demo.player_override = None;
                    }
                    self.open_video(&bvid, context);
                    self.sync_smtc(context);
                }
                crate::demo::DemoAction::PlayerExpandDesc => {
                    if let Some(demo) = self.demo.as_mut() {
                        demo.player_override = Some((1, pages::player::PlayerTab::Intro, true));
                    }
                }
                crate::demo::DemoAction::PlayerShowComments => {
                    if let Some(demo) = self.demo.as_mut() {
                        demo.player_override = Some((2, pages::player::PlayerTab::Comments, false));
                    }
                }
                crate::demo::DemoAction::Complete => {
                    if let Some(demo) = self.demo.as_ref() {
                        demo.request_exit_if_configured();
                    }
                }
            }
        }

        if self.demo.as_ref().is_some_and(|d| !d.is_finished()) {
            self.demo_timer = Some(
                context.set_local_timeout(crate::demo::DEMO_TICK_INTERVAL, || Message::DemoTick),
            );
        } else {
            self.demo_timer = None;
        }
    }


    pub(in crate::shell) fn open_video(&mut self, bvid: &str, context: &ComponentContext<Self>) {
        // #7:进播放页即消费悬停意图:收净预览并复位「手动关闭」静默。
        self.peek_hide_now();
        self.peek_dismissal.reset();
        // 数据源：首页已加载卡 → 搜索/热门/相关推荐索引 → 最小模型（真实标题由 video_meta 回填）。
        let mut item = match self
            .home_index
            .get(bvid)
            .cloned()
            .or_else(|| self.search_index.get(bvid).cloned())
            .or_else(|| self.popular_index.get(bvid).cloned())
            .or_else(|| self.related_index.get(bvid).cloned())
            .or_else(|| self.user_index.get(bvid).cloned())
        {
            Some(item) => item,
            None => minimal_video_item(bvid, ""),
        };
        if item.cover.is_none()
            && let Some(url) = &item.pic_url
        {
            item.cover = self.image_cache.get(url);
        }
        self.start_playback(item, context);
    }


    pub(in crate::shell) fn open_video_target(&mut self, target: OpenTarget, context: &ComponentContext<Self>) {
        if self.home_index.contains_key(&target.bvid)
            || self.popular_index.contains_key(&target.bvid)
            || self.related_index.contains_key(&target.bvid)
        {
            self.open_video(&target.bvid, context);
            return;
        }
        let item = minimal_video_item(&target.bvid, &target.title);
        self.start_playback(item, context);
    }


    pub(in crate::shell) fn listen_toast(
        rx: &Arc<Mutex<std::sync::mpsc::Receiver<()>>>,
        context: &ComponentContext<Self>,
    ) {
        let rx = Arc::clone(rx);
        context.spawn_background(move |_| {
            let ok = rx.lock().ok().is_some_and(|guard| guard.recv().is_ok());
            Message::ToastActivated(ok)
        });
    }


    pub(in crate::shell) fn cycle_nav_route(&mut self, delta: isize) {
        let current_idx = NAV_ROUTES
            .iter()
            .position(|&r| r == self.route)
            .unwrap_or(0) as isize;
        let len = NAV_ROUTES.len() as isize;
        let next_idx = ((current_idx + delta).rem_euclid(len)) as usize;
        let next_route = NAV_ROUTES[next_idx];
        if next_route != self.route {
            self.history.push(self.route);
            self.route = next_route;
            crate::trace::note(format!("route->{}", next_route.tag()));
        }
    }


    pub(in crate::shell) fn navigate_back(&mut self) {
        self.logout_confirm = false;
        self.more_menu_open = false;
        self.search_overlay_open = false;
        if self.viewport_layout.uses_whisper_drilldown()
            && self.route == Route::Login
            && self.whisper_detail_open
        {
            self.whisper_detail_open = false;
            return;
        }
        if let Some(route) = self.history.pop() {
            self.route = route;
            crate::trace::note(format!("route->{} (返回)", route.tag()));
        } else if self.route == Route::Player {
            self.route = Route::Home;
            crate::trace::note("route->home (返回,空历史)");
        }
    }


}

