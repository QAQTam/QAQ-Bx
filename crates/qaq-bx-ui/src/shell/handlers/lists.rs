//! 列表页域:热门/直播/历史/稍后再看/收藏五页 ensure/fetch/absorb 全链、ListAutoMore
//! 滚动自动续页门控(见 docs/Update-0.6.0.md A 项)、直播开播(start_live_playback)、
//! 热门拉取批(spawn_popular_fetch)。
//!
//! 本文件由 P3 拆解生成:match 臂与配套方法自原 update()/impl AppShell 逐字搬运,
//! 消息路由见 handlers/mod.rs(穷尽 match)。单元测试锚点见 shell/tests.rs。

use super::super::*;

impl AppShell {
    /// lists 域消息臂(17 变体,与 message.rs 分组注释对应);未命中变体静默忽略。
    pub(super) fn handle_lists(&mut self, message: Message, context: &ComponentContext<Self>) -> bool {
        match message {
            Message::PopularMore => {
                self.fetch_popular(context);
            }
            Message::PopularLoaded(res) => {
                self.absorb_popular(res, context);
            }
            Message::ListAutoMore(target) => {
                use crate::paging::{should_auto_page, AutoPage};
                // 门控:首页已成功 + 无在飞 + 未终态 + 无错误才自动续页;
                // 错误态留给手动「重试」胶囊,避免滚动事件反复打失败请求。
                let ok = match target {
                    AutoPage::History => {
                        let s = &self.user_history;
                        self.lists_logged_in()
                            && should_auto_page(s.loaded_once, s.loading, s.reached_end, s.error.is_some())
                    }
                    AutoPage::WatchLater => {
                        let s = &self.watch_later;
                        self.lists_logged_in()
                            && should_auto_page(s.loaded_once, s.loading, s.reached_end, s.error.is_some())
                    }
                    AutoPage::Favorites => {
                        let s = &self.favorites.resources;
                        self.lists_logged_in()
                            && self.favorites.selected != 0
                            && should_auto_page(s.loaded_once, s.loading, s.reached_end, s.error.is_some())
                    }
                    AutoPage::Popular => {
                        let s = &self.popular;
                        should_auto_page(s.loaded_once, s.loading, s.reached_end, s.error.is_some())
                    }
                    AutoPage::Live => {
                        let s = &self.live;
                        should_auto_page(s.loaded_once, s.loading, s.reached_end, s.error.is_some())
                    }
                };
                if !ok {
                    return true;
                }
                match target {
                    AutoPage::History => self.fetch_history(context),
                    AutoPage::WatchLater => self.fetch_watch_later(context),
                    AutoPage::Favorites => self.fetch_fav_resources(context),
                    AutoPage::Popular => self.fetch_popular(context),
                    AutoPage::Live => self.fetch_live_rooms(context),
                }
            }
            Message::LiveRoomsLoaded { page, res } => {
                self.absorb_live_rooms(page, res, context);
            }
            Message::LiveCoverReady { url, image } => {
                self.live_cover_inflight.remove(&url);
                if let Some(img) = image {
                    for card in &mut self.live.cards {
                        if card.room.cover == url && card.cover.is_none() {
                            card.cover = Some(img.clone());
                        }
                    }
                }
            }
            Message::LiveRoomSelected(roomid, title, uname) => {
                self.start_live_playback(roomid, title, uname, context);
                self.sync_smtc(context);
            }
            Message::LiveReady { gen_id, result } => {
                if gen_id != self.playback_gen {
                    return true;
                }
                match result {
                    Ok(url) => {
                        self.mpv_load_streams(&url, None, 0.0, self.settings.autoplay, context);
                        self.playing = self.settings.autoplay;
                        self.current_video_url = url;
                        self.media_probe_at = Some(Instant::now());
                    }
                    Err(reason) => {
                        self.player_notice = Some(format!("直播间打开失败:{reason}"));
                        self.video_loading = false;
                        self.media_load_pending = false;
                        self.mpv_load_epoch = None;
                    }
                }
            }
            Message::LiveMore => {
                self.fetch_live_rooms(context);
            }
            Message::HistoryLoaded { page, res } => {
                self.absorb_history(page, res, context);
            }
            Message::HistoryMore => {
                self.fetch_history(context);
            }
            Message::WatchLaterLoaded { page, res } => {
                self.absorb_watch_later(page, res, context);
            }
            Message::WatchLaterMore => {
                self.fetch_watch_later(context);
            }
            Message::FavFoldersLoaded { res } => {
                self.absorb_fav_folders(res, context);
            }
            Message::FavSelectFolder(folder) => {
                self.select_fav_folder(folder, context);
            }
            Message::FavResourcesLoaded { folder, page, res } => {
                self.absorb_fav_resources(folder, page, res, context);
            }
            Message::FavMore => {
                self.fetch_fav_resources(context);
            }
            Message::UserCoverReady { url, image } => {
                self.user_cover_inflight.remove(&url);
                if let Some(img) = image {
                    for card in &mut self.user_history.cards {
                        if card.cover_url == url && card.cover.is_none() {
                            card.cover = Some(img.clone());
                        }
                    }
                    for card in &mut self.watch_later.cards {
                        if card.cover_url == url && card.cover.is_none() {
                            card.cover = Some(img.clone());
                        }
                    }
                    for card in &mut self.favorites.resources.cards {
                        if card.cover_url == url && card.cover.is_none() {
                            card.cover = Some(img.clone());
                        }
                    }
                    // user_index 以 bvid 键入,不按 url 回填;open_video 时经
                    // pic_url → image_cache.get 命中位图(已 put 进缓存)。
                }
            }
            _ => {}
        }
        false
    }

    /// 热门页首拉守卫：导航进入/重试时仅在从未成功加载且不在飞时开拉。
    pub(in crate::shell) fn ensure_popular(&mut self, context: &ComponentContext<Self>) {
        if self.popular.loaded_once || self.popular.loading {
            return;
        }
        self.fetch_popular(context);
    }


    /// U4:直播页首屏/加载更多(页号推进与热门同语义)。
    pub(in crate::shell) fn ensure_live(&mut self, context: &ComponentContext<Self>) {
        if self.live.loaded_once || self.live.loading {
            return;
        }
        self.fetch_live_rooms(context);
    }


    pub(in crate::shell) fn fetch_live_rooms(&mut self, context: &ComponentContext<Self>) {
        if self.live.loading {
            return;
        }
        self.live.loading = true;
        self.live.error = None;
        let client = self.live_client.clone();
        let cache = self.image_cache.clone();
        let page = self.live.next_page.max(1);
        context.spawn_background(move |_| {
            let res = block_on(client.rooms(page)).map_err(|err| err.to_string());
            Message::LiveRoomsLoaded {
                page,
                res: res.map(|rooms| {
                    rooms
                        .into_iter()
                        .map(|room| LiveCard {
                            cover: cache.get(&room.cover),
                            room,
                        })
                        .collect()
                }),
            }
        });
    }


    pub(in crate::shell) fn absorb_live_rooms(
        &mut self,
        page: u32,
        res: Result<Vec<LiveCard>, String>,
        context: &ComponentContext<Self>,
    ) {
        if page != self.live.next_page {
            return;
        }
        self.live.loading = false;
        let cards = match res {
            Ok(cards) => cards,
            Err(err) => {
                self.live.error = Some(err);
                return;
            }
        };
        self.live.next_page += 1;
        self.live.reached_end = cards.is_empty();
        let existing: std::collections::HashSet<u64> =
            self.live.cards.iter().map(|c| c.room.roomid).collect();
        for card in cards {
            if existing.contains(&card.room.roomid) {
                continue;
            }
            if card.cover.is_none() && !card.room.cover.is_empty() {
                self.spawn_live_cover(card.room.cover.clone(), context);
            }
            self.live.cards.push(card);
        }
        self.live.loaded_once = true;
    }


    pub(in crate::shell) fn lists_logged_in(&self) -> bool {
        matches!(self.auth, AuthState::LoggedIn(_))
    }


    /// U5:历史页首屏/加载更多。
    pub(in crate::shell) fn ensure_history(&mut self, context: &ComponentContext<Self>) {
        if !self.lists_logged_in() || self.user_history.loaded_once || self.user_history.loading {
            return;
        }
        self.fetch_history(context);
    }


    pub(in crate::shell) fn fetch_history(&mut self, context: &ComponentContext<Self>) {
        if !self.lists_logged_in() || self.user_history.loading {
            return;
        }
        self.user_history.loading = true;
        self.user_history.error = None;
        let client = self.history_client.clone();
        let cache = self.image_cache.clone();
        let page = self.user_history.next_page.max(1);
        context.spawn_background(move |_| {
            let res = block_on(client.history(page))
                .map(|items| {
                    items
                        .iter()
                        .map(|item| {
                            pages::user_lists::card_from_watch(
                                item,
                                &crate::theme::fmt_time(item.viewed_at),
                                cache.get(&item.cover),
                            )
                        })
                        .collect()
                })
                .map_err(|err| err.to_string());
            Message::HistoryLoaded { page, res }
        });
    }


    pub(in crate::shell) fn absorb_history(
        &mut self,
        page: u32,
        res: Result<Vec<UserVideoCard>, String>,
        context: &ComponentContext<Self>,
    ) {
        if page != self.user_history.next_page {
            return;
        }
        self.user_history.loading = false;
        let cards = match res {
            Ok(cards) => cards,
            Err(err) => {
                self.user_history.error = Some(err);
                return;
            }
        };
        self.user_history.next_page += 1;
        self.user_history.reached_end = cards.is_empty();
        for card in cards {
            if card.cover.is_none() && !card.cover_url.is_empty() {
                self.spawn_user_cover(card.cover_url.clone(), context);
            }
            self.user_index
                .insert(card.bvid.clone(), user_card_item(&card));
            self.user_history.cards.push(card);
        }
        self.user_history.loaded_once = true;
    }


    /// U5:稍后再看页首屏/加载更多(服务端 count 作页头总数)。
    pub(in crate::shell) fn ensure_watch_later(&mut self, context: &ComponentContext<Self>) {
        if !self.lists_logged_in() || self.watch_later.loaded_once || self.watch_later.loading {
            return;
        }
        self.fetch_watch_later(context);
    }


    pub(in crate::shell) fn fetch_watch_later(&mut self, context: &ComponentContext<Self>) {
        if !self.lists_logged_in() || self.watch_later.loading {
            return;
        }
        self.watch_later.loading = true;
        self.watch_later.error = None;
        let client = self.history_client.clone();
        let cache = self.image_cache.clone();
        let page = self.watch_later.next_page.max(1);
        context.spawn_background(move |_| {
            let res = block_on(async {
                let page = client.toview(page).await;
                page.map(|data| {
                    let cards = data
                        .items
                        .iter()
                        .map(|item| {
                            pages::user_lists::card_from_watch(
                                item,
                                &item
                                    .added_at
                                    .map(crate::theme::fmt_time)
                                    .unwrap_or_default(),
                                cache.get(&item.cover),
                            )
                        })
                        .collect();
                    (data.total, cards)
                })
            });
            let res = res.map_err(|err| err.to_string());
            Message::WatchLaterLoaded { page, res }
        });
    }


    pub(in crate::shell) fn absorb_watch_later(
        &mut self,
        page: u32,
        res: Result<(u64, Vec<UserVideoCard>), String>,
        context: &ComponentContext<Self>,
    ) {
        if page != self.watch_later.next_page {
            return;
        }
        self.watch_later.loading = false;
        let (total, cards) = match res {
            Ok(pair) => pair,
            Err(err) => {
                self.watch_later.error = Some(err);
                return;
            }
        };
        self.watch_later.next_page += 1;
        self.watch_later.reached_end = cards.is_empty();
        self.watch_later.total = Some(total);
        for card in cards {
            if card.cover.is_none() && !card.cover_url.is_empty() {
                self.spawn_user_cover(card.cover_url.clone(), context);
            }
            self.user_index
                .insert(card.bvid.clone(), user_card_item(&card));
            self.watch_later.cards.push(card);
        }
        self.watch_later.loaded_once = true;
    }


    /// U5:收藏页首屏(夹列表;成功后默认选首夹并拉资源首页)。
    pub(in crate::shell) fn ensure_favorites(&mut self, context: &ComponentContext<Self>) {
        if !self.lists_logged_in() || self.favorites.folders_loading || self.favorites.folders_loaded
        {
            return;
        }
        self.fetch_fav_folders(context);
    }


    pub(in crate::shell) fn fetch_fav_folders(&mut self, context: &ComponentContext<Self>) {
        if !self.lists_logged_in() || self.favorites.folders_loading {
            return;
        }
        self.favorites.folders_loading = true;
        self.favorites.folders_error = None;
        let client = self.fav_client.clone();
        // mid 从登录态资料直取(lists_logged_in 已保证 LoggedIn 分支)。
        let mid = match &self.auth {
            AuthState::LoggedIn(user) => user.mid,
            _ => 0,
        };
        if mid == 0 {
            self.favorites.folders_loading = false;
            self.favorites.folders_error = Some("未获取到用户 mid,请稍后重试".into());
            return;
        }
        context.spawn_background(move |_| {
            let res = block_on(client.created_folders(mid))
                .map(|folders| {
                    folders
                        .into_iter()
                        .map(|f| FavFolderChip {
                            id: f.id,
                            title: f.title,
                            media_count: f.media_count,
                        })
                        .collect()
                })
                .map_err(|err| err.to_string());
            Message::FavFoldersLoaded { res }
        });
    }


    pub(in crate::shell) fn absorb_fav_folders(
        &mut self,
        res: Result<Vec<FavFolderChip>, String>,
        context: &ComponentContext<Self>,
    ) {
        self.favorites.folders_loading = false;
        let folders = match res {
            Ok(folders) => folders,
            Err(err) => {
                self.favorites.folders_error = Some(err);
                return;
            }
        };
        let first = folders.first().map(|f| f.id).unwrap_or(0);
        self.favorites.folders = folders;
        self.favorites.folders_loaded = true;
        if first != 0 && self.favorites.selected == 0 {
            self.select_fav_folder(first, context);
        }
    }


    pub(in crate::shell) fn select_fav_folder(&mut self, folder: u64, context: &ComponentContext<Self>) {
        if folder == 0 {
            return;
        }
        self.favorites.selected = folder;
        self.favorites.resources = UserListState::default();
        self.fetch_fav_resources(context);
    }


    pub(in crate::shell) fn fetch_fav_resources(&mut self, context: &ComponentContext<Self>) {
        let folder = self.favorites.selected;
        if !self.lists_logged_in() || folder == 0 || self.favorites.resources.loading {
            return;
        }
        self.favorites.resources.loading = true;
        self.favorites.resources.error = None;
        let client = self.fav_client.clone();
        let cache = self.image_cache.clone();
        let page = self.favorites.resources.next_page.max(1);
        context.spawn_background(move |_| {
            let res = block_on(client.resources(folder, page))
                .map(|data| {
                    let cards = data
                        .items
                        .iter()
                        .map(|media| {
                            pages::user_lists::card_from_fav(
                                media,
                                &crate::theme::fmt_time(media.fav_at),
                                cache.get(&media.cover),
                            )
                        })
                        .collect();
                    (data.folder_media_count, cards, data.has_more)
                })
                .map_err(|err| err.to_string());
            Message::FavResourcesLoaded { folder, page, res }
        });
    }


    pub(in crate::shell) fn absorb_fav_resources(
        &mut self,
        folder: u64,
        page: u32,
        res: Result<(u64, Vec<UserVideoCard>, bool), String>,
        context: &ComponentContext<Self>,
    ) {
        if folder != self.favorites.selected || page != self.favorites.resources.next_page {
            return;
        }
        self.favorites.resources.loading = false;
        let (total, cards, has_more) = match res {
            Ok(triple) => triple,
            Err(err) => {
                self.favorites.resources.error = Some(err);
                return;
            }
        };
        self.favorites.resources.next_page += 1;
        self.favorites.resources.reached_end = !has_more || cards.is_empty();
        self.favorites.resources.total = Some(total);
        for card in cards {
            if card.cover.is_none() && !card.cover_url.is_empty() {
                self.spawn_user_cover(card.cover_url.clone(), context);
            }
            self.user_index
                .insert(card.bvid.clone(), user_card_item(&card));
            self.favorites.resources.cards.push(card);
        }
        self.favorites.resources.loaded_once = true;
    }


    /// U4:直播间 → Player 页(mpv 直播 FLV/HLS;评论区/画质/分P/弹幕不适用)。
    pub(in crate::shell) fn start_live_playback(
        &mut self,
        roomid: u64,
        title: String,
        uname: String,
        context: &ComponentContext<Self>,
    ) {
        self.more_menu_open = false;
        self.search_overlay_open = false;
        self.home_half_pane = false;
        if self.route != Route::Player {
            self.history.push(self.route);
        }
        self.route = Route::Player;
        crate::trace::note("route->player (换流/重开)");
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
        self.pages_menu.clear();
        self.current_cid = 0;
        self.now_playing_desc = None;
        self.meta_fetch_inflight_seq = None;
        self.playback_avatar_dirty = false;
        self.playback_avatar_timer = None;
        // 直播恒走 mpv(FLV,MF 不支持):先彻底停掉上一视频的双引擎残余。
        self.teardown_native_side(context);
        let engine = self.ensure_engine();
        engine.stop();
        engine.set_danmaku(Vec::new());
        self.comment_draft.clear();
        self.composer_notice = None;
        let mut item = minimal_video_item(&format!("{LIVE_BVID_PREFIX}{roomid}"), &title);
        item.author = uname;
        self.now_playing = Some(item);
        self.playing = self.settings.autoplay;
        self.position = 0;
        self.engine_duration = None;
        self.media_load_pending = true;
        self.native_load_epoch = None;
        self.mpv_load_epoch = None;
        self.native_video_probe_at = None;
        self.native_stall_probe_at = None;
        self.release_stream_proxy_session();
        self.player_notice = None;
        self.video_loading = true;
        self.comments_session.reset(0, 0);
        self.refresh_comments_snapshot();
        self.schedule_tick(context);
        let client = self.live_client.clone();
        let gen_id = self.playback_gen;
        context.spawn_background(move |_| {
            let result = block_on(client.play_url(roomid)).map_err(|err| err.to_string());
            Message::LiveReady { gen_id, result }
        });
    }


    pub(in crate::shell) fn fetch_popular(&mut self, context: &ComponentContext<Self>) {
        if self.popular.loading {
            return;
        }
        self.popular.loading = true;
        self.popular.error = None;
        spawn_popular_fetch(
            &self.feed_client,
            &self.image_cache,
            self.popular.next_pn,
            context,
        );
    }


    pub(in crate::shell) fn absorb_popular(
        &mut self,
        res: Result<Vec<VideoItem>, String>,
        context: &ComponentContext<Self>,
    ) {
        self.popular.loading = false;
        let items = match res {
            Ok(items) => items,
            Err(err) => {
                // next_pn 不推进：「加载更多」即自动重取同页（兼作重试）。
                self.popular.error = Some(err);
                return;
            }
        };
        self.popular.next_pn += 1;
        self.popular.reached_end = items.is_empty();
        let fresh = merge_popular_items(&mut self.popular.items, items);
        for mut item in fresh {
            if item.cover.is_none()
                && let Some(url) = &item.pic_url
            {
                if let Some(cached) = self.image_cache.get(url) {
                    item.cover = Some(cached);
                } else {
                    self.spawn_cover_fetch(item.bvid.clone(), url.clone(), self.feed_gen, context);
                }
            }
            self.popular_index.insert(item.bvid.clone(), item);
        }
        self.popular.loaded_once = true;
    }


}

/// 单次 popular 分页抓取（后台线程转展示模型 + LRU 缓存命中预填封面）。
pub(in crate::shell) fn spawn_popular_fetch(
    feed_client: &Arc<FeedClient>,
    image_cache: &Arc<ImageCache>,
    pn: u32,
    context: &ComponentContext<AppShell>,
) {
    let client = feed_client.clone();
    let cache = image_cache.clone();
    context.spawn_background(move |_| {
        let res = block_on(client.popular(pn, POPULAR_PAGE_SIZE)).map_err(|e| e.to_string());
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
        Message::PopularLoaded(match res {
            Ok(_) => Ok(items),
            Err(err) => Err(err),
        })
    });
}


