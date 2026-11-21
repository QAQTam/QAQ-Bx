//! 图片管线域(封面+头像,首页/列表/相关推荐/账号多域共用):CoverReady 回填、
//! FlushCovers 80ms 批合并、离屏驱逐(sync_offscreen_covers,±4 屏窗口)、头像批量下载
//! (spawn_avatars 系)与 AvatarLoaded/FlushPlaybackAvatars 分发。
//!
//! 本文件由 P3 拆解生成:match 臂与配套方法自原 update()/impl AppShell 逐字搬运,
//! 消息路由见 handlers/mod.rs(穷尽 match)。单元测试锚点见 shell/tests.rs。

use super::super::*;

impl AppShell {
    /// cover_pipeline 域消息臂(4 变体,与 message.rs 分组注释对应);未命中变体静默忽略。
    pub(super) fn handle_cover_pipeline(&mut self, message: Message, context: &ComponentContext<Self>) -> bool {
        match message {
            Message::CoverReady { seq, bvid, image } => {
                if seq != self.feed_gen {
                    return true;
                }
                self.cover_inflight.remove(&bvid);
                let Some(image) = image else { return true };
                self.pending_covers.insert(bvid, image);
                self.schedule_cover_flush(context);
            }
            Message::FlushCovers => {
                self.cover_flush_timer = None;
                if self.pending_covers.is_empty() {
                    return true;
                }
                let keep = feed_pager::visible_cover_window_by_row(
                    self.home_anchor_row,
                    self.viewport_layout.cards,
                    self.home_viewport_h,
                    self.home_slots.len(),
                    COVER_WINDOW_BUFFER_SCREENS,
                );
                let batch = std::mem::take(&mut self.pending_covers);
                let mut changed = false;
                for (bvid, image) in batch {
                    let mut applied = false;
                    if let Some(&idx) = self.home_slot_by_bvid.get(&bvid)
                        && keep.contains(&idx)
                        && let Some(HomeSlot::Video { item, .. }) = self.home_slots.get_mut(idx)
                        && item.cover.is_none()
                    {
                        Rc::make_mut(item).cover = Some(image.clone());
                        changed = true;
                        applied = true;
                    }
                    // 搜索结果封面兜底回填(首页未消费的封面落到搜索卡)。
                    if !applied
                        && let Some(item) =
                            self.search_state.videos.iter_mut().find(|v| v.bvid == bvid)
                        && item.cover.is_none()
                    {
                        item.cover = Some(image.clone());
                        applied = true;
                    }
                    // 热门/相关推荐封面兜底回填（同一有界 LRU，8GB 内存约束内共享字节）。
                    if !applied
                        && let Some(item) = self.popular.items.iter_mut().find(|v| v.bvid == bvid)
                        && item.cover.is_none()
                    {
                        item.cover = Some(image.clone());
                        applied = true;
                    }
                    if !applied
                        && let Some(item) = self.related.items.iter_mut().find(|v| v.bvid == bvid)
                        && item.cover.is_none()
                    {
                        item.cover = Some(image);
                        // F3:封面回填是原位变更,必须换代触发相关虚拟源重建。
                        self.related.rev = self.related.rev.wrapping_add(1);
                    }
                }
                if changed {
                    self.rebuild_home_source();
                }
            }
            Message::AvatarLoaded {
                avatar_gen,
                inflight_epoch,
                url,
                has_image,
            } => {
                if avatar_gen != self.avatar_gen {
                    return true;
                }
                if self.avatar_inflight.get(&url) == Some(&inflight_epoch) {
                    self.avatar_inflight.remove(&url);
                }
                if has_image
                    && self.now_playing.is_some()
                    && self.comments_session.references_avatar(&url)
                {
                    self.playback_avatar_dirty = true;
                    self.schedule_playback_avatar_flush(context);
                }
            }
            Message::FlushPlaybackAvatars => {
                self.playback_avatar_timer = None;
                if self.playback_avatar_dirty && self.now_playing.is_some() {
                    self.playback_avatar_dirty = false;
                    self.refresh_comments_snapshot();
                }
            }
            _ => {}
        }
        false
    }

    pub(in crate::shell) fn spawn_cover_fetch(
        &mut self,
        bvid: String,
        url: String,
        seq: u64,
        context: &ComponentContext<Self>,
    ) {
        if !self.cover_inflight.insert(bvid.clone()) {
            return;
        }
        let auth = self.auth_service.clone();
        let cache = self.image_cache.clone();
        let epoch = self.feed_epoch.clone();
        context.spawn_background(move |_| {
            if epoch.load(Ordering::SeqCst) != seq {
                return Message::CoverReady {
                    seq,
                    bvid,
                    image: None,
                };
            }
            let image = block_on(async {
                let _permit = cover_semaphore().acquire().await.ok()?;
                if epoch.load(Ordering::SeqCst) != seq {
                    return None;
                }
                let img = auth.fetch_image_async(&url).await;
                if let Some(ref img) = img
                    && epoch.load(Ordering::SeqCst) == seq
                {
                    cache.put(url.clone(), img.clone());
                }
                img
            });
            Message::CoverReady { seq, bvid, image }
        });
    }


    /// 离屏封面去引用 & 缓存热恢复：距当前挂载行 ±(4+1) 屏之外置空 `item.cover` 释放 WinUI 解码纹理，
    /// 滚回时先查有界 480 张 LRU `ImageCache` 零网络秒级恢复，命中失败则自动兜底补拉。
    pub(in crate::shell) fn sync_offscreen_covers(&mut self, context: &ComponentContext<Self>) {
        let keep = feed_pager::visible_cover_window_by_row(
            self.home_anchor_row,
            self.viewport_layout.cards,
            self.home_viewport_h,
            self.home_slots.len(),
            COVER_WINDOW_BUFFER_SCREENS,
        );
        self.home_last_evict_row = self.home_anchor_row;
        let mut changed = false;
        let mut refetch = Vec::new();
        for (idx, slot) in self.home_slots.iter_mut().enumerate() {
            let HomeSlot::Video { item, .. } = slot else {
                continue;
            };
            if keep.contains(&idx) {
                if item.cover.is_none()
                    && let Some(url) = &item.pic_url
                {
                    if let Some(cached) = self.image_cache.get(url) {
                        // #6:copy-on-write,仅变更条目发生一次真实拷贝。
                        Rc::make_mut(item).cover = Some(cached);
                        changed = true;
                    } else {
                        refetch.push((item.bvid.clone(), url.clone()));
                    }
                }
            } else if item.cover.is_some() {
                Rc::make_mut(item).cover = None;
                changed = true;
            }
        }
        if changed {
            self.rebuild_home_source();
        }
        let seq = self.feed_gen;
        for (bvid, url) in refetch {
            self.spawn_cover_fetch(bvid, url, seq, context);
        }
    }


    pub(in crate::shell) fn schedule_playback_avatar_flush(&mut self, context: &ComponentContext<Self>) {
        if self.playback_avatar_timer.is_none() {
            self.playback_avatar_timer = Some(
                context.set_local_timeout(COVER_FLUSH_INTERVAL, || Message::FlushPlaybackAvatars),
            );
        }
    }


    /// U4:直播间封面补拉(url 键 LRU;与视频封面同一信号量节流)。
    pub(in crate::shell) fn spawn_live_cover(&mut self, url: String, context: &ComponentContext<Self>) {
        if !self.live_cover_inflight.insert(url.clone()) {
            return;
        }
        let auth = self.auth_service.clone();
        let cache = self.image_cache.clone();
        context.spawn_background(move |_| {
            let image = block_on(async {
                let _permit = cover_semaphore().acquire().await.ok()?;
                auth.fetch_image_async(&url).await
            });
            if image.is_some() {
                cache.put(url.clone(), image.clone().unwrap());
            }
            Message::LiveCoverReady { url, image }
        });
    }

    // ---- U5:登录态列表页(历史/稍后再看/收藏) ----
    //
    // 门控纪律:未登录不发请求(与 U1 评论发布同款);登录态切换由
    // `reset_account_data` 清态,翻页页号随之复位。


    /// U5:用户列表封面补拉(url 键 LRU;与视频封面同一信号量节流)。
    pub(in crate::shell) fn spawn_user_cover(&mut self, url: String, context: &ComponentContext<Self>) {
        if !self.user_cover_inflight.insert(url.clone()) {
            return;
        }
        let auth = self.auth_service.clone();
        let cache = self.image_cache.clone();
        context.spawn_background(move |_| {
            let image = block_on(async {
                let _permit = cover_semaphore().acquire().await.ok()?;
                auth.fetch_image_async(&url).await
            });
            if image.is_some() {
                cache.put(url.clone(), image.clone().unwrap());
            }
            Message::UserCoverReady { url, image }
        });
    }



    pub(in crate::shell) fn schedule_cover_flush(&mut self, context: &ComponentContext<Self>) {
        if self.cover_flush_timer.is_none() {
            self.cover_flush_timer =
                Some(context.set_local_timeout(COVER_FLUSH_INTERVAL, || Message::FlushCovers));
        }
    }


    pub(in crate::shell) fn spawn_notify_avatars<'a>(
        &mut self,
        urls: impl IntoIterator<Item = &'a str>,
        context: &ComponentContext<Self>,
    ) {
        self.spawn_avatars(urls, 0, context);
    }


    pub(in crate::shell) fn spawn_playback_avatars<'a>(
        &mut self,
        urls: impl IntoIterator<Item = &'a str>,
        playback_seq: u64,
        context: &ComponentContext<Self>,
    ) {
        self.spawn_avatars(urls, playback_seq, context);
    }


    pub(in crate::shell) fn spawn_avatars<'a>(
        &mut self,
        urls: impl IntoIterator<Item = &'a str>,
        inflight_epoch: u64,
        context: &ComponentContext<Self>,
    ) {
        let avatar_gen = self.avatar_gen;
        for raw in urls {
            if raw.is_empty() {
                continue;
            }
            let thumb = avatar_thumbnail_url(raw);
            if self.avatar_cache.get(&thumb).is_some() {
                continue;
            }
            if let Some(&existing_epoch) = self.avatar_inflight.get(&thumb) {
                // 0 表示持久有效的通知/关系头像；同代次也不重复发起；仅在旧播放视频被切走时允许接管。
                if existing_epoch == 0 || existing_epoch == inflight_epoch || inflight_epoch == 0 {
                    continue;
                }
            }
            self.avatar_inflight.insert(thumb.clone(), inflight_epoch);
            let auth = self.auth_service.clone();
            let cache = self.avatar_cache.clone();
            let playback_epoch = self.playback_epoch.clone();
            let url = thumb;
            context.spawn_background(move |_| {
                let has_image = block_on(async {
                    if inflight_epoch != 0
                        && playback_epoch.load(Ordering::SeqCst) != inflight_epoch
                    {
                        return false;
                    }
                    let Ok(_permit) = cover_semaphore().acquire().await else {
                        return false;
                    };
                    if inflight_epoch != 0
                        && playback_epoch.load(Ordering::SeqCst) != inflight_epoch
                    {
                        return false;
                    }
                    let Some(img) = auth.fetch_image_async(&url).await else {
                        return false;
                    };
                    cache.put(url.clone(), img);
                    true
                });
                Message::AvatarLoaded {
                    avatar_gen,
                    inflight_epoch,
                    url,
                    has_image,
                }
            });
        }
    }

}

