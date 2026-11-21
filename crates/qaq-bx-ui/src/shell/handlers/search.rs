//! 搜索域:草稿与提交(SearchDraftChanged/SearchSubmitted)、结果吸收与失败
//! (SearchResults/SearchFailed)、热词默认填充(SearchDefaultReady/SearchPickDefault)。
//!
//! 本文件由 P3 拆解生成:match 臂与配套方法自原 update()/impl AppShell 逐字搬运,
//! 消息路由见 handlers/mod.rs(穷尽 match)。单元测试锚点见 shell/tests.rs。

use super::super::*;

impl AppShell {
    /// search 域消息臂(7 变体,与 message.rs 分组注释对应);未命中变体静默忽略。
    pub(super) fn handle_search(&mut self, message: Message, context: &ComponentContext<Self>) -> bool {
        match message {
            Message::SearchDraftChanged(text) => {
                self.search_state.draft = text;
            }
            Message::SearchSubmitted => {
                self.submit_search(context);
            }
            Message::SearchResults {
                seq,
                query,
                items,
                total,
            } => {
                if seq != self.search_seq {
                    return true;
                }
                let mut owned = Vec::with_capacity(items.len());
                for item in items {
                    if item.cover.is_none()
                        && let Some(url) = &item.pic_url
                    {
                        // 复用首页封面补拉(缓存命中由后台线程已挂;feed_epoch 未变即生效)。
                        self.spawn_cover_fetch(
                            item.bvid.clone(),
                            url.clone(),
                            self.feed_gen,
                            context,
                        );
                    }
                    self.search_index.insert(item.bvid.clone(), item.clone());
                    owned.push(item);
                }
                self.search_state.videos = owned;
                self.search_state.total = total;
                self.search_state.query = query;
                self.search_state.loading = false;
            }
            Message::SearchFailed { seq, error } => {
                if seq != self.search_seq {
                    return true;
                }
                self.search_state.loading = false;
                self.search_state.error = Some(error);
            }
            Message::SearchDefaultReady(word) => {
                self.search_default = word.clone();
                self.search_state.default_word = word;
            }
            Message::SearchPickDefault => {
                // U2:默认词填充草稿并立即提交(草稿先入单源,提交复用同一路径)。
                if let Some(word) = self.search_default.clone() {
                    self.search_state.draft = word;
                    self.update(Message::SearchSubmitted, context);
                }
            }
            _ => {}
        }
        false
    }

    pub(in crate::shell) fn submit_search(&mut self, context: &ComponentContext<Self>) {
        let query = self.search_state.draft.trim().to_string();
        if query.is_empty() {
            return;
        }
        self.exit_fullscreen(context);
        self.search_overlay_open = false;
        self.more_menu_open = false;
        self.search_seq += 1;
        let seq = self.search_seq;
        if self.route != Route::Search {
            self.history.push(self.route);
            self.route = Route::Search;
            crate::trace::note("route->search (搜索提交)");
        }
        self.search_state.query = query.clone();
        self.search_state.loading = true;
        self.search_state.error = None;
        let client = self.search_client.clone();
        let cache = self.image_cache.clone();
        context.spawn_background(move |_| match block_on(client.all_v2(&query, 1, 20)) {
            Ok(page) => {
                let items: Vec<VideoItem> = page
                    .videos
                    .iter()
                    .map(|video| {
                        let mut item = search_video_item(video);
                        if item.cover.is_none()
                            && let Some(url) = &item.pic_url
                        {
                            item.cover = cache.get(url);
                        }
                        item
                    })
                    .collect();
                Message::SearchResults {
                    seq,
                    query: page.query,
                    items,
                    total: page.total,
                }
            }
            Err(err) => Message::SearchFailed {
                seq,
                error: err.to_string(),
            },
        });
    }


}

