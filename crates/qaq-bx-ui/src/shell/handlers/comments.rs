//! 评论与楼中楼域:CommentsSessionState/ReplyBucket 状态机(排序桶、头像引用计数、
//! snapshot 版本)、主楼加载/排序切换/追加(CommentsLoaded/SetCommentSort/LoadMoreComments)、
//! 子回复展开与加载(ToggleSubReplies/SubRepliesLoaded)。
//!
//! 本文件由 P3 拆解生成:match 臂与配套方法自原 update()/impl AppShell 逐字搬运,
//! 消息路由见 handlers/mod.rs(穷尽 match)。单元测试锚点见 shell/tests.rs。

use super::super::*;

impl AppShell {
    /// comments 域消息臂(11 变体,与 message.rs 分组注释对应);未命中变体静默忽略。
    pub(super) fn handle_comments(&mut self, message: Message, context: &ComponentContext<Self>) -> bool {
        match message {
            Message::CommentDraftChanged(text) => {
                self.comment_draft = text;
                if self.composer_notice.is_some() {
                    self.composer_notice = None;
                }
            }
            Message::SubmitComment => {
                // U1 登录门控:未登录直接行内提示,不发请求。
                if !matches!(self.auth, AuthState::LoggedIn(_)) {
                    self.composer_notice = Some("请先登录后再发布评论".to_string());
                    return true;
                }
                let draft = self.comment_draft.trim().to_string();
                if draft.is_empty() {
                    self.composer_notice = Some("评论内容不能为空".to_string());
                    return true;
                }
                let Some(meta) = self.current_meta.clone() else {
                    self.composer_notice = Some("视频信息尚未就绪,稍后再试".to_string());
                    return true;
                };
                self.composer_notice = None;
                let reply_client = self.reply_client.clone();
                let gen_id = self.playback_gen;
                context.spawn_background(move |_| {
                    let result = block_on(reply_client.add(meta.aid, &draft))
                        .map_err(|err| err.to_string());
                    Message::CommentSubmitted { gen_id, result }
                });
            }
            Message::CommentSubmitted { gen_id, result } => {
                if gen_id != self.playback_gen {
                    return true;
                }
                match result {
                    Ok(_) => {
                        // 发布成功:清草稿 + 回到首页重拉最新评论。
                        self.comment_draft.clear();
                        self.composer_notice = None;
                        let sort = self.comments_session.sort;
                        self.comments_session.bucket_mut(sort).cursor = None;
                        self.comments_session.bucket_mut(sort).has_more = true;
                        self.fetch_comments(sort, false, context);
                    }
                    Err(reason) => {
                        self.composer_notice = Some(format!("发布失败:{reason}"));
                    }
                }
            }
            Message::SetCommentSort(sort) => {
                if self.comments_session.sort == sort {
                    return true;
                }
                self.comments_session.sort = sort;
                self.refresh_comments_snapshot();
                let bucket = self.comments_session.bucket(sort);
                if !bucket.loaded_once && !bucket.loading {
                    self.fetch_comments(sort, false, context);
                }
            }
            Message::LoadMoreComments => {
                let sort = self.comments_session.sort;
                if self.comments_session.bucket(sort).has_more {
                    self.fetch_comments(sort, true, context);
                }
            }
            Message::RetryComments => {
                if self.comments_session.aid == 0 {
                    let seq = self.playback_gen;
                    self.meta_fetch_inflight_seq = None;
                    self.kickoff_video_extras(seq, context);
                } else {
                    let sort = self.comments_session.sort;
                    let bucket = self.comments_session.bucket(sort);
                    let append = !bucket.replies.is_empty() || !bucket.top_replies.is_empty();
                    self.fetch_comments(sort, append, context);
                }
            }
            Message::CommentsLoaded {
                seq,
                sort,
                append,
                res,
            } => {
                if seq != self.playback_gen {
                    return true;
                }
                let (loaded_upper, avatar_urls): (u64, Vec<String>) = {
                    let bucket = self.comments_session.bucket_mut(sort);
                    bucket.loading = false;
                    bucket.loaded_once = true;
                    match res {
                        Ok(page) => {
                            let page_has_more = page.has_more();
                            let upper = page.upper_mid;
                            if !append {
                                bucket.top_replies.clear();
                                bucket.replies.clear();
                                bucket.seen_rpids.clear();
                            }
                            let mut urls = Vec::new();
                            if !append {
                                for item in page.top_replies {
                                    if bucket.seen_rpids.insert(item.rpid) {
                                        if !item.avatar.is_empty() {
                                            urls.push(item.avatar.clone());
                                        }
                                        bucket.top_replies.push(item);
                                    }
                                }
                            }
                            let mut added = 0usize;
                            for item in page.replies {
                                if bucket.seen_rpids.insert(item.rpid) {
                                    if !item.avatar.is_empty() {
                                        urls.push(item.avatar.clone());
                                    }
                                    bucket.replies.push(item);
                                    added += 1;
                                }
                            }
                            if page.cursor.all_count > 0 || bucket.total_count.is_none() {
                                bucket.total_count = Some(page.cursor.all_count);
                            }
                            let has_more = if append {
                                page_has_more && added > 0
                            } else {
                                page_has_more
                                    && (!bucket.replies.is_empty()
                                        || !bucket.top_replies.is_empty())
                            };
                            bucket.cursor = Some(page.cursor);
                            bucket.has_more = has_more;
                            bucket.error = None;
                            (upper, urls)
                        }
                        Err(err) => {
                            bucket.error = Some(err);
                            (0, Vec::new())
                        }
                    }
                };
                if self.comments_session.upper_mid == 0 && loaded_upper > 0 {
                    self.comments_session.upper_mid = loaded_upper;
                }
                self.refresh_comments_snapshot();
                self.spawn_playback_avatars(avatar_urls.iter().map(String::as_str), seq, context);
            }
            Message::ToggleSubReplies(rpid) => {
                let need_fetch =
                    if let Some(entry) = self.comments_session.sub_replies.get_mut(&rpid) {
                        entry.expanded = !entry.expanded;
                        entry.expanded && entry.items.is_empty() && !entry.loading
                    } else {
                        true
                    };
                if need_fetch {
                    self.fetch_sub_replies(rpid, false, context);
                } else {
                    self.refresh_comments_snapshot();
                }
            }
            Message::LoadMoreSubReplies(rpid) => {
                self.fetch_sub_replies(rpid, true, context);
            }
            Message::SubRepliesLoaded {
                seq,
                root_rpid,
                append,
                res,
            } => {
                if seq != self.playback_gen {
                    return true;
                }
                let avatar_urls: Vec<String> = {
                    let upper_mid = &mut self.comments_session.upper_mid;
                    let entry = self
                        .comments_session
                        .sub_replies
                        .entry(root_rpid)
                        .or_default();
                    entry.loading = false;
                    match res {
                        Ok(page) => {
                            if *upper_mid == 0 && page.upper_mid > 0 {
                                *upper_mid = page.upper_mid;
                            }
                            if !append {
                                entry.items.clear();
                            }
                            let mut seen: HashSet<u64> =
                                entry.items.iter().map(|i| i.rpid).collect();
                            let mut urls = Vec::new();
                            let mut added = 0usize;
                            for item in page.replies {
                                if seen.insert(item.rpid) {
                                    if !item.avatar.is_empty() {
                                        urls.push(item.avatar.clone());
                                    }
                                    entry.items.push(item);
                                    added += 1;
                                }
                            }
                            entry.total_count = page.total_count;
                            entry.next_pn = page.page_num.saturating_add(1);
                            entry.has_more = page.has_more && (!append || added > 0);
                            entry.error = None;
                            urls
                        }
                        Err(err) => {
                            entry.error = Some(err);
                            Vec::new()
                        }
                    }
                };
                self.refresh_comments_snapshot();
                self.spawn_playback_avatars(avatar_urls.iter().map(String::as_str), seq, context);
            }
            _ => {}
        }
        false
    }

    pub(in crate::shell) fn refresh_comments_snapshot(&mut self) {
        self.comments_rev = self.comments_rev.wrapping_add(1);
        self.comments_snapshot = self
            .comments_session
            .to_snapshot(self.comments_rev, &self.avatar_cache);
    }


    pub(in crate::shell) fn fetch_comments(
        &mut self,
        sort: CommentSort,
        append: bool,
        context: &ComponentContext<Self>,
    ) {
        let aid = self.comments_session.aid;
        if aid == 0 {
            return;
        }
        let cursor = {
            let bucket = self.comments_session.bucket_mut(sort);
            if bucket.loading {
                return;
            }
            bucket.loading = true;
            bucket.error = None;
            if append { bucket.cursor.clone() } else { None }
        };
        self.refresh_comments_snapshot();

        let seq = self.playback_gen;
        let client = self.reply_client.clone();
        context.spawn_background(move |_| Message::CommentsLoaded {
            seq,
            sort,
            append,
            res: block_on(client.fetch_main(aid, ReplySort::from(sort), cursor.as_ref()))
                .map_err(|e| e.to_string()),
        });
    }


    pub(in crate::shell) fn fetch_sub_replies(
        &mut self,
        root_rpid: u64,
        append: bool,
        context: &ComponentContext<Self>,
    ) {
        let aid = self.comments_session.aid;
        if aid == 0 || root_rpid == 0 {
            return;
        }
        let pn = {
            let entry = self
                .comments_session
                .sub_replies
                .entry(root_rpid)
                .or_default();
            if entry.loading {
                return;
            }
            entry.expanded = true;
            entry.loading = true;
            entry.error = None;
            if append && !entry.items.is_empty() {
                entry.next_pn.max(1)
            } else {
                1
            }
        };
        self.refresh_comments_snapshot();

        let seq = self.playback_gen;
        let client = self.reply_client.clone();
        context.spawn_background(move |_| Message::SubRepliesLoaded {
            seq,
            root_rpid,
            append: append && pn > 1,
            res: block_on(client.fetch_sub(aid, root_rpid, pn, SUB_REPLY_PAGE_SIZE))
                .map_err(|e| e.to_string()),
        });
    }


}

#[derive(Default)]
pub(in crate::shell) struct ReplyBucket {
    pub(in crate::shell) loading: bool,
    pub(in crate::shell) loaded_once: bool,
    pub(in crate::shell) has_more: bool,
    pub(in crate::shell) error: Option<String>,
    pub(in crate::shell) cursor: Option<ReplyCursor>,
    pub(in crate::shell) total_count: Option<u64>,
    pub(in crate::shell) top_replies: Vec<ReplyItem>,
    pub(in crate::shell) replies: Vec<ReplyItem>,
    pub(in crate::shell) seen_rpids: HashSet<u64>,
}


#[derive(Default)]
pub(in crate::shell) struct CommentsSessionState {
    pub(in crate::shell) aid: u64,
    pub(in crate::shell) sort: CommentSort,
    pub(in crate::shell) upper_mid: u64,
    pub(in crate::shell) hot: ReplyBucket,
    pub(in crate::shell) latest: ReplyBucket,
    pub(in crate::shell) sub_replies: HashMap<u64, SubReplyState>,
}


impl CommentsSessionState {
    pub(in crate::shell) fn reset(&mut self, aid: u64, upper_mid: u64) {
        self.aid = aid;
        self.sort = CommentSort::Hot;
        self.upper_mid = upper_mid;
        self.hot = ReplyBucket::default();
        self.latest = ReplyBucket::default();
        self.sub_replies.clear();
    }

    pub(in crate::shell) fn bucket(&self, sort: CommentSort) -> &ReplyBucket {
        match sort {
            CommentSort::Hot => &self.hot,
            CommentSort::Latest => &self.latest,
        }
    }

    pub(in crate::shell) fn bucket_mut(&mut self, sort: CommentSort) -> &mut ReplyBucket {
        match sort {
            CommentSort::Hot => &mut self.hot,
            CommentSort::Latest => &mut self.latest,
        }
    }

    pub(in crate::shell) fn references_avatar(&self, thumb_url: &str) -> bool {
        let bucket = self.bucket(self.sort);
        let in_main = bucket
            .top_replies
            .iter()
            .chain(bucket.replies.iter())
            .any(|item| !item.avatar.is_empty() && avatar_thumbnail_url(&item.avatar) == thumb_url);
        if in_main {
            return true;
        }
        self.sub_replies.values().any(|sub| {
            sub.expanded
                && sub.items.iter().any(|item| {
                    !item.avatar.is_empty() && avatar_thumbnail_url(&item.avatar) == thumb_url
                })
        })
    }

    pub(in crate::shell) fn to_snapshot(&self, rev: u64, avatar_cache: &ImageCache) -> CommentsSnapshot {
        let bucket = self.bucket(self.sort);
        let mut avatars = HashMap::new();
        let collect_avatar = |raw: &str, out: &mut HashMap<String, EncodedImage>| {
            if raw.is_empty() {
                return;
            }
            let thumb = avatar_thumbnail_url(raw);
            if !out.contains_key(&thumb)
                && let Some(img) = avatar_cache.get(&thumb)
            {
                out.insert(thumb, img);
            }
        };
        for item in bucket.top_replies.iter().chain(bucket.replies.iter()) {
            collect_avatar(&item.avatar, &mut avatars);
        }
        for sub in self.sub_replies.values() {
            if sub.expanded {
                for item in &sub.items {
                    collect_avatar(&item.avatar, &mut avatars);
                }
            }
        }
        let loading = bucket.loading && bucket.top_replies.is_empty() && bucket.replies.is_empty();
        let loading_more =
            bucket.loading && (!bucket.top_replies.is_empty() || !bucket.replies.is_empty());
        CommentsSnapshot {
            rev,
            data: Arc::new(CommentsData {
                aid: self.aid,
                sort: self.sort,
                upper_mid: self.upper_mid,
                total_count: bucket.total_count,
                loading,
                loading_more,
                loaded_once: bucket.loaded_once,
                has_more: bucket.has_more,
                error: bucket.error.clone(),
                top_replies: bucket.top_replies.clone(),
                replies: bucket.replies.clone(),
                sub_replies: self.sub_replies.clone(),
                avatars,
            }),
        }
    }
}


