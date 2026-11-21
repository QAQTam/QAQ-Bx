//! 账号/通知/私信/关系域:个人中心(AccountRefresh/AccountStatLoaded/UnreadLoaded)、
//! 通知 Tab 与回复流(Replies/Ats/Likes/SystemsLoaded)、私信会话与线程(SessionsLoaded/
//! WhisperThreadLoaded)、关注粉丝关系(OpenRelations→RelationsLoaded)。
//!
//! 本文件由 P3 拆解生成:match 臂与配套方法自原 update()/impl AppShell 逐字搬运,
//! 消息路由见 handlers/mod.rs(穷尽 match)。单元测试锚点见 shell/tests.rs。

use super::super::*;

impl AppShell {
    /// account_notify 域消息臂(20 变体,与 message.rs 分组注释对应);未命中变体静默忽略。
    pub(super) fn handle_account_notify(&mut self, message: Message, context: &ComponentContext<Self>) -> bool {
        match message {
            Message::OpenNotifications => {
                self.exit_fullscreen(context);
                self.logout_confirm = false;
                self.more_menu_open = false;
                self.search_overlay_open = false;
                self.whisper_detail_open = false;
                if self.route != Route::Login {
                    self.history.push(self.route);
                    self.route = Route::Login;
                    crate::trace::note("route->login (账号入口)");
                }
                if self.notify_tab.unread(self.unread_summary) == 0
                    && let Some(unread_tab) = NotifyTab::ALL
                        .into_iter()
                        .find(|tab| tab.unread(self.unread_summary) > 0)
                {
                    self.notify_tab = unread_tab;
                }
                self.ensure_account_loaded(context);
            }
            Message::AccountRefresh => {
                if !matches!(self.auth, AuthState::LoggedIn(_)) {
                    return true;
                }
                let tab = self.notify_tab;
                let selected = self.selected_whisper;
                self.reset_account_data();
                self.notify_tab = tab;
                self.fetch_account_summary(context);
                self.fetch_notify_tab(tab, false, context);
                if tab == NotifyTab::Whisper
                    && let Some((tid, stype)) = selected
                {
                    self.selected_whisper = Some((tid, stype));
                    self.fetch_whisper_thread(tid, stype, context);
                }
            }
            Message::AccountStatLoaded { seq, stat } => {
                if seq == self.notify_gen && stat.is_some() {
                    self.account_stat = stat;
                }
            }
            Message::UnreadLoaded { seq, summary } => {
                if seq == self.notify_gen
                    && let Some(s) = summary
                {
                    self.unread_summary = s;
                }
            }
            Message::SelectNotifyTab(tab) => {
                self.notify_tab = tab;
                self.whisper_detail_open = false;
                self.ensure_active_tab_loaded(context);
            }
            Message::LoadMoreNotifyTab(tab) => {
                self.fetch_notify_tab(tab, true, context);
            }
            Message::RetryNotifyTab(tab) => {
                let append = match tab {
                    NotifyTab::Reply => !self.notify_replies.items.is_empty(),
                    NotifyTab::At => !self.notify_ats.items.is_empty(),
                    NotifyTab::Like => !self.notify_likes.items.is_empty(),
                    NotifyTab::System => !self.notify_systems.items.is_empty(),
                    NotifyTab::Whisper => !self.notify_sessions.items.is_empty(),
                };
                self.fetch_notify_tab(tab, append, context);
            }
            Message::RepliesLoaded { seq, append, res } => {
                if seq != self.notify_gen {
                    return true;
                }
                let urls = apply_feed_page(
                    &mut self.notify_replies,
                    &mut self.notify_replies_cursor,
                    append,
                    res,
                    |i| &i.sender_avatar,
                );
                self.spawn_notify_avatars(urls.iter().map(String::as_str), context);
            }
            Message::AtsLoaded { seq, append, res } => {
                if seq != self.notify_gen {
                    return true;
                }
                let urls = apply_feed_page(
                    &mut self.notify_ats,
                    &mut self.notify_ats_cursor,
                    append,
                    res,
                    |i| &i.sender_avatar,
                );
                self.spawn_notify_avatars(urls.iter().map(String::as_str), context);
            }
            Message::LikesLoaded { seq, append, res } => {
                if seq != self.notify_gen {
                    return true;
                }
                let urls = apply_feed_page(
                    &mut self.notify_likes,
                    &mut self.notify_likes_cursor,
                    append,
                    res,
                    |i| &i.sender_avatar,
                );
                self.spawn_notify_avatars(urls.iter().map(String::as_str), context);
            }
            Message::SystemsLoaded { seq, append, res } => {
                if seq != self.notify_gen {
                    return true;
                }
                self.notify_systems.loading = false;
                self.notify_systems.loaded_once = true;
                match res {
                    Ok(page) => {
                        if append {
                            let existing: HashSet<u64> =
                                self.notify_systems.items.iter().map(|i| i.id).collect();
                            for item in page.items {
                                if !existing.contains(&item.id) {
                                    self.notify_systems.items.push(item);
                                }
                            }
                        } else {
                            self.notify_systems.items = page.items;
                        }
                        self.notify_systems_cursor = page.next_cursor;
                        self.notify_systems.has_more = page.has_more;
                        self.notify_systems.error = None;
                    }
                    Err(err) => self.notify_systems.error = Some(err),
                }
            }
            Message::SessionsLoaded { seq, append, res } => {
                if seq != self.notify_gen {
                    return true;
                }
                self.notify_sessions.loading = false;
                self.notify_sessions.loaded_once = true;
                match res {
                    Ok(page) => {
                        let urls: Vec<String> = page
                            .sessions
                            .iter()
                            .map(|s| s.talker_avatar.clone())
                            .collect();
                        if append {
                            self.notify_sessions.items.extend(page.sessions);
                        } else {
                            self.notify_sessions.items = page.sessions;
                        }
                        self.notify_sessions_end_ts = page.end_ts;
                        self.notify_sessions.has_more = page.has_more;
                        self.notify_sessions.error = None;
                        self.spawn_notify_avatars(urls.iter().map(String::as_str), context);
                        if self.selected_whisper.is_none()
                            && let Some(first) = self.notify_sessions.items.first()
                        {
                            let key = (first.talker_id, first.session_type);
                            self.selected_whisper = Some(key);
                            if !self.whisper_threads.contains_key(&key) {
                                self.fetch_whisper_thread(key.0, key.1, context);
                            }
                        }
                    }
                    Err(err) => self.notify_sessions.error = Some(err),
                }
            }
            Message::SelectWhisperSession((talker_id, session_type)) => {
                self.selected_whisper = Some((talker_id, session_type));
                self.whisper_detail_open = true;
                if !self
                    .whisper_threads
                    .contains_key(&(talker_id, session_type))
                {
                    self.fetch_whisper_thread(talker_id, session_type, context);
                }
            }
            Message::CloseWhisperDetail => {
                self.whisper_detail_open = false;
            }
            Message::RefreshWhisperThread((talker_id, session_type)) => {
                self.fetch_whisper_thread(talker_id, session_type, context);
            }
            Message::WhisperThreadLoaded {
                seq,
                talker_id,
                session_type,
                res,
            } => {
                if seq != self.notify_gen {
                    return true;
                }
                let entry = self
                    .whisper_threads
                    .entry((talker_id, session_type))
                    .or_default();
                entry.loading = false;
                match res {
                    Ok(msgs) => {
                        entry.messages = msgs;
                        entry.error = None;
                    }
                    Err(err) => entry.error = Some(err),
                }
            }
            Message::OpenRelations(kind) => {
                let (slice, _) = self.relation_slot(kind);
                if !slice.loading && !slice.loaded_once {
                    self.fetch_relations(kind, false, context);
                }
            }
            Message::RefreshRelations(kind) => {
                self.fetch_relations(kind, false, context);
            }
            Message::LoadMoreRelations(kind) => {
                self.fetch_relations(kind, true, context);
            }
            Message::RelationsLoaded {
                seq,
                kind,
                append,
                res,
            } => {
                if seq != self.notify_gen {
                    return true;
                }
                let urls: Option<Vec<String>> = {
                    let (slice, next_pn) = self.relation_slot_mut(kind);
                    slice.loading = false;
                    slice.loaded_once = true;
                    match res {
                        Ok(page) => {
                            let faces: Vec<String> =
                                page.items.iter().map(|u| u.face.clone()).collect();
                            if append {
                                let existing: HashSet<u64> =
                                    slice.items.iter().map(|u| u.mid).collect();
                                for u in page.items {
                                    if !existing.contains(&u.mid) {
                                        slice.items.push(u);
                                    }
                                }
                            } else {
                                slice.items = page.items;
                            }
                            *next_pn = page.next_pn;
                            slice.has_more = page.has_more;
                            slice.error = None;
                            if page.total > 0 {
                                let stat = self.account_stat.get_or_insert_with(Default::default);
                                match kind {
                                    RelationKind::Following => stat.following = page.total,
                                    RelationKind::Follower => stat.follower = page.total,
                                }
                            }
                            Some(faces)
                        }
                        Err(err) => {
                            slice.error = Some(err);
                            None
                        }
                    }
                };
                if let Some(urls) = urls {
                    self.spawn_notify_avatars(urls.iter().map(String::as_str), context);
                }
            }
            _ => {}
        }
        false
    }

    pub(in crate::shell) fn fetch_account_summary(&mut self, context: &ComponentContext<Self>) {
        let seq = self.notify_gen;
        let auth = self.auth_service.clone();
        let nav = auth.nav_client();
        context.spawn_background(move |_| {
            let stat = block_on(nav.fetch_stat()).ok();
            Message::AccountStatLoaded { seq, stat }
        });
        let auth_u = self.auth_service.clone();
        context.spawn_background(move |_| {
            let summary = block_on(auth_u.notify_client().fetch_unread()).ok();
            Message::UnreadLoaded { seq, summary }
        });
    }


    pub(in crate::shell) fn relation_slot(&self, kind: RelationKind) -> (&ListSliceState<RelationUser>, u32) {
        match kind {
            RelationKind::Following => (&self.followings, self.followings_next_pn),
            RelationKind::Follower => (&self.followers, self.followers_next_pn),
        }
    }


    pub(in crate::shell) fn relation_slot_mut(
        &mut self,
        kind: RelationKind,
    ) -> (&mut ListSliceState<RelationUser>, &mut u32) {
        match kind {
            RelationKind::Following => (&mut self.followings, &mut self.followings_next_pn),
            RelationKind::Follower => (&mut self.followers, &mut self.followers_next_pn),
        }
    }


    pub(in crate::shell) fn fetch_relations(
        &mut self,
        kind: RelationKind,
        append: bool,
        context: &ComponentContext<Self>,
    ) {
        let AuthState::LoggedIn(user) = &self.auth else {
            return;
        };
        let mid = user.mid;
        let seq = self.notify_gen;
        let nav = self.auth_service.nav_client();
        let pn = {
            let (slice, next_pn) = self.relation_slot_mut(kind);
            if slice.loading {
                return;
            }
            slice.loading = true;
            slice.error = None;
            if append { (*next_pn).max(1) } else { 1 }
        };
        context.spawn_background(move |_| Message::RelationsLoaded {
            seq,
            kind,
            append,
            res: block_on(nav.fetch_relations(mid, kind, pn, RELATION_PAGE_SIZE))
                .map_err(|e| e.to_string()),
        });
    }


    pub(in crate::shell) fn ensure_account_loaded(&mut self, context: &ComponentContext<Self>) {
        if !matches!(self.auth, AuthState::LoggedIn(_)) {
            return;
        }
        if self.account_stat.is_none() {
            self.fetch_account_summary(context);
        }
        self.ensure_active_tab_loaded(context);
    }


    pub(in crate::shell) fn ensure_active_tab_loaded(&mut self, context: &ComponentContext<Self>) {
        if !matches!(self.auth, AuthState::LoggedIn(_)) {
            return;
        }
        let already = match self.notify_tab {
            NotifyTab::Reply => self.notify_replies.loaded_once || self.notify_replies.loading,
            NotifyTab::At => self.notify_ats.loaded_once || self.notify_ats.loading,
            NotifyTab::Like => self.notify_likes.loaded_once || self.notify_likes.loading,
            NotifyTab::System => self.notify_systems.loaded_once || self.notify_systems.loading,
            NotifyTab::Whisper => self.notify_sessions.loaded_once || self.notify_sessions.loading,
        };
        if !already {
            self.fetch_notify_tab(self.notify_tab, false, context);
        }
    }


    pub(in crate::shell) fn fetch_notify_tab(&mut self, tab: NotifyTab, append: bool, context: &ComponentContext<Self>) {
        if !matches!(self.auth, AuthState::LoggedIn(_)) {
            return;
        }
        let seq = self.notify_gen;
        let client = self.auth_service.notify_client();
        match tab {
            NotifyTab::Reply => {
                if self.notify_replies.loading {
                    return;
                }
                self.notify_replies.loading = true;
                self.notify_replies.error = None;
                let cursor = if append {
                    self.notify_replies_cursor
                } else {
                    None
                };
                context.spawn_background(move |_| Message::RepliesLoaded {
                    seq,
                    append,
                    res: block_on(client.fetch_replies(cursor)).map_err(|e| e.to_string()),
                });
            }
            NotifyTab::At => {
                if self.notify_ats.loading {
                    return;
                }
                self.notify_ats.loading = true;
                self.notify_ats.error = None;
                let cursor = if append { self.notify_ats_cursor } else { None };
                context.spawn_background(move |_| Message::AtsLoaded {
                    seq,
                    append,
                    res: block_on(client.fetch_ats(cursor)).map_err(|e| e.to_string()),
                });
            }
            NotifyTab::Like => {
                if self.notify_likes.loading {
                    return;
                }
                self.notify_likes.loading = true;
                self.notify_likes.error = None;
                let cursor = if append {
                    self.notify_likes_cursor
                } else {
                    None
                };
                context.spawn_background(move |_| Message::LikesLoaded {
                    seq,
                    append,
                    res: block_on(client.fetch_likes(cursor)).map_err(|e| e.to_string()),
                });
            }
            NotifyTab::System => {
                if self.notify_systems.loading {
                    return;
                }
                self.notify_systems.loading = true;
                self.notify_systems.error = None;
                let cursor = if append {
                    self.notify_systems_cursor
                } else {
                    None
                };
                context.spawn_background(move |_| Message::SystemsLoaded {
                    seq,
                    append,
                    res: block_on(client.fetch_system_notifies(cursor)).map_err(|e| e.to_string()),
                });
            }
            NotifyTab::Whisper => {
                if self.notify_sessions.loading {
                    return;
                }
                self.notify_sessions.loading = true;
                self.notify_sessions.error = None;
                let end_ts = if append {
                    self.notify_sessions_end_ts
                } else {
                    None
                };
                context.spawn_background(move |_| Message::SessionsLoaded {
                    seq,
                    append,
                    res: block_on(client.fetch_sessions(end_ts)).map_err(|e| e.to_string()),
                });
            }
        }
    }


    pub(in crate::shell) fn fetch_whisper_thread(
        &mut self,
        talker_id: u64,
        session_type: i32,
        context: &ComponentContext<Self>,
    ) {
        if !matches!(self.auth, AuthState::LoggedIn(_)) {
            return;
        }
        let entry = self
            .whisper_threads
            .entry((talker_id, session_type))
            .or_default();
        if entry.loading {
            return;
        }
        entry.loading = true;
        entry.error = None;
        let seq = self.notify_gen;
        let client = self.auth_service.notify_client();
        context.spawn_background(move |_| Message::WhisperThreadLoaded {
            seq,
            talker_id,
            session_type,
            res: block_on(client.fetch_session_messages(talker_id, session_type))
                .map_err(|e| e.to_string()),
        });
    }


}

