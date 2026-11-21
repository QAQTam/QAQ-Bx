//! 认证/登录域:启动探测(AuthProbed)、扫码登录状态机(AuthStartLogin→LoginQrReady→
//! LoginPollTick/LoginPolled)、登出(AuthLogout→AuthLoggedOut)与会话清理(clear_login_session/
//! reset_account_data,登出后各列表/账号态归零)。
//!
//! 本文件由 P3 拆解生成:match 臂与配套方法自原 update()/impl AppShell 逐字搬运,
//! 消息路由见 handlers/mod.rs(穷尽 match)。单元测试锚点见 shell/tests.rs。

use super::super::*;

impl AppShell {
    /// auth 域消息臂(15 变体,与 message.rs 分组注释对应);未命中变体静默忽略。
    pub(super) fn handle_auth(&mut self, message: Message, context: &ComponentContext<Self>) -> bool {
        match message {
            Message::AuthProbed { seq, report } => {
                if seq != self.auth_gen {
                    return true;
                }
                self.auth = report.state;
                if let Some(notice) = report.notice {
                    self.auth_notice = Some(notice);
                }
                if matches!(self.auth, AuthState::LoggedIn(_)) {
                    self.fetch_account_summary(context);
                    if self.route == Route::Login {
                        self.ensure_active_tab_loaded(context);
                    }
                    // 登录落定时已停在列表路由:补触发首屏(ensure_* 自带去重)。
                    match self.route {
                        Route::History => self.ensure_history(context),
                        Route::WatchLater => self.ensure_watch_later(context),
                        Route::Favorites => self.ensure_favorites(context),
                        _ => {}
                    }
                } else {
                    self.reset_account_data();
                }
            }
            Message::AuthStartLogin => {
                if matches!(
                    self.login_flow,
                    LoginFlowState::Starting
                        | LoginFlowState::AwaitingScan { .. }
                        | LoginFlowState::Scanned { .. }
                ) {
                    return true;
                }
                self.logout_confirm = false;
                self.auth_notice = None;
                self.login_flow = LoginFlowState::Starting;
                let auth = self.auth_service.clone();
                let cell = self.flow.clone();
                context.spawn_background(move |_| match auth.start_login_blocking(&cell) {
                    Ok(qr) => Message::LoginQrReady(qr),
                    Err(reason) => Message::LoginStartFailed(reason),
                });
            }
            Message::LoginQrReady(qr) => {
                self.login_flow = LoginFlowState::AwaitingScan { qr };
                self.schedule_login_tick(context);
            }
            Message::LoginStartFailed(reason) => {
                self.login_flow = LoginFlowState::Failed(reason);
            }
            Message::LoginPollTick => {
                let auth = self.auth_service.clone();
                let cell = self.flow.clone();
                context.spawn_background(move |_| Message::LoginPolled(auth.poll_blocking(&cell)));
            }
            Message::LoginPolled(outcome) => match outcome {
                PollOutcome::WaitingScan => self.schedule_login_tick(context),
                PollOutcome::Scanned => {
                    self.login_flow = match &self.login_flow {
                        LoginFlowState::AwaitingScan { qr } | LoginFlowState::Scanned { qr } => {
                            LoginFlowState::Scanned { qr: qr.clone() }
                        }
                        other => other.clone(),
                    };
                    self.schedule_login_tick(context);
                }
                PollOutcome::Expired => {
                    self.login_timer = None;
                    self.login_flow = match &self.login_flow {
                        LoginFlowState::AwaitingScan { qr }
                        | LoginFlowState::Scanned { qr }
                        | LoginFlowState::Expired { qr } => {
                            LoginFlowState::Expired { qr: qr.clone() }
                        }
                        other => other.clone(),
                    };
                }
                PollOutcome::Success => {
                    self.clear_login_session();
                    self.reset_account_data();
                    // 会话已建立 → 递增认证代次重新探测用户信息（头像/昵称/续期状态）。
                    self.auth = AuthState::Unknown;
                    self.auth_gen = self.auth_gen.wrapping_add(1);
                    let auth = self.auth_service.clone();
                    let seq = self.auth_gen;
                    context.spawn_background(move |_| Message::AuthProbed {
                        seq,
                        report: auth.probe_blocking(),
                    });
                }
                PollOutcome::Error(reason) => {
                    self.login_timer = None;
                    self.login_flow = LoginFlowState::Failed(reason);
                }
            },
            Message::AuthCancelLogin => {
                self.logout_confirm = false;
                self.auth_notice = None;
                self.clear_login_session();
            }
            Message::AuthLogout => {
                // 二次确认：登出会作废服务端会话（不可逆）。
                if !self.logout_confirm {
                    self.logout_confirm = true;
                    return true;
                }
                self.logout_confirm = false;
                self.auth_gen = self.auth_gen.wrapping_add(1);
                let auth = self.auth_service.clone();
                context.spawn_background(move |_| {
                    Message::AuthLoggedOut(auth.logout_blocking().err())
                });
            }
            Message::AuthCancelLogout => {
                self.logout_confirm = false;
            }
            Message::AuthLoggedOut(error) => match error {
                Some(reason) => self.auth_notice = Some(format!("退出失败：{reason}")),
                None => {
                    self.auth_gen = self.auth_gen.wrapping_add(1);
                    self.auth_notice = None;
                    self.auth = AuthState::Guest;
                    self.reset_account_data();
                }
            },

            // ---- 个人中心与通知中心 ----
            _ => {}
        }
        false
    }

    /// 登录轮询 2s 节拍链。
    pub(in crate::shell) fn schedule_login_tick(&mut self, context: &ComponentContext<Self>) {
        self.login_timer =
            Some(context.set_local_timeout(Duration::from_secs(2), || Message::LoginPollTick));
    }


    pub(in crate::shell) fn clear_login_session(&mut self) {
        *self.flow.lock().expect("登录会话槽中毒") = None;
        self.login_timer = None;
        self.login_flow = LoginFlowState::Idle;
    }


    pub(in crate::shell) fn reset_account_data(&mut self) {
        self.notify_gen = self.notify_gen.wrapping_add(1);
        self.account_stat = None;
        self.unread_summary = UnreadSummary::default();
        // U5:登录态切换(登录/登出/换号)一律清列表态,翻页页号随之复位。
        self.user_history = UserListState::default();
        self.watch_later = UserListState::default();
        self.favorites = FavPageState::default();
        self.user_index.clear();
        self.user_cover_inflight.clear();
        self.followings = ListSliceState::default();
        self.followings_next_pn = 1;
        self.followers = ListSliceState::default();
        self.followers_next_pn = 1;
        self.notify_replies = ListSliceState::default();
        self.notify_replies_cursor = None;
        self.notify_ats = ListSliceState::default();
        self.notify_ats_cursor = None;
        self.notify_likes = ListSliceState::default();
        self.notify_likes_cursor = None;
        self.notify_systems = ListSliceState::default();
        self.notify_systems_cursor = None;
        self.notify_sessions = ListSliceState::default();
        self.notify_sessions_end_ts = None;
        self.selected_whisper = None;
        self.whisper_detail_open = false;
        self.whisper_threads.clear();
        self.avatar_gen = self.avatar_gen.wrapping_add(1);
        self.avatar_cache.clear();
        self.avatar_inflight.clear();
    }


}

