//! 弹幕域:分段加载(DanmakuPart/DanmakuFailed,分段数学见 util.rs danmaku_segment_*)、
//! DanmakuStage 装卸与配置下发(danmaku_config/filter)、进度重播种(reseed_danmaku)。
//!
//! 本文件由 P3 拆解生成:match 臂与配套方法自原 update()/impl AppShell 逐字搬运,
//! 消息路由见 handlers/mod.rs(穷尽 match)。单元测试锚点见 shell/tests.rs。

use super::super::*;

impl AppShell {
    /// danmaku 域消息臂(2 变体,与 message.rs 分组注释对应);未命中变体静默忽略。
    pub(super) fn handle_danmaku(&mut self, message: Message, context: &ComponentContext<Self>) -> bool {
        match message {
            Message::DanmakuPart {
                seq,
                epoch,
                index,
                segment,
            } => {
                // 播放代次或弹幕投递代次(切引擎)过期 → 整段作废,不续链。
                if seq != self.playback_gen || epoch != self.danmaku_epoch {
                    return true;
                }
                // 转引擎镜像类型(避免 qaq-bx-video 依赖 qaq-bx-core),增量追加;
                // 文本零拷贝:#2 条目仅携带分段缓冲的 (off,len),被筛掉的弹幕
                // 全程不物化 String。引擎线程归并回 progress 升序后叠画。
                let data = segment.data().clone();
                let items = segment
                    .elems()
                    .iter()
                    .map(|e| OverlayDanmaku {
                        progress_ms: e.progress_ms,
                        mode: e.mode,
                        fontsize: e.fontsize,
                        color: e.color,
                        content: ContentRef::new(data.clone(), e.content_off, e.content_len),
                        lane: 0,
                        y_off: 0.0,
                        est_width: 0.0,
                        mid_hash: e.mid_hash,
                        weight: e.weight,
                        pool: e.pool,
                    })
                    .collect();
                self.danmaku_append(items);
                // 预存链:驱动下一段(全部段到货后自然收束)。
                if let Some(next) = self.next_danmaku_index(index) {
                    self.fetch_danmaku_segment(seq, epoch, next, context);
                }
            }
            Message::DanmakuFailed {
                seq,
                epoch,
                index,
                reason,
            } => {
                // 弹幕拉取失败不影响播放;静默跳过该段,预存链继续补齐后续段。
                // 切引擎后的旧链分段直接作废(与新链的投递口不匹配)。
                if seq == self.playback_gen && epoch == self.danmaku_epoch {
                    eprintln!("[qaq-bx-ui] 弹幕分段 {index} 拉取失败(不影响播放): {reason}");
                    if let Some(next) = self.next_danmaku_index(index) {
                        self.fetch_danmaku_segment(seq, epoch, next, context);
                    }
                }
            }
            _ => {}
        }
        false
    }

    /// v0.3:native 弹幕透明层懒创建(native 未启用返回 None)。创建时按当前
    /// 设置下发一次样式/规则,之后一切变更走 `danmaku_*` 分流口,杜绝两侧漂移。
    pub(in crate::shell) fn ensure_stage(&mut self) -> Option<Arc<DanmakuStage>> {
        if !self.native_enabled() {
            return None;
        }
        if self.native_stage.is_none() {
            let stage = DanmakuStage::with_config(video_config_from(&self.settings));
            eprintln!("[qaq-bx-ui] native danmaku stage: 透明层已创建");
            self.native_stage = Some(stage.clone());
        }
        self.native_stage.clone()
    }

    // ---- 弹幕投递单源(native_playback → 透明层;否则 → 视频引擎同帧叠画) ----


    /// 全量替换(新视频/换分P 的清层语义)。
    pub(in crate::shell) fn danmaku_replace(&self, items: Vec<OverlayDanmaku>) {
        if let Some(stage) = self.stage_sink() {
            stage.set_danmaku(items);
        } else if let Some(engine) = &self.engine {
            engine.set_danmaku(items);
        }
    }


    /// 增量追加(分段预存链逐段到货)。
    pub(in crate::shell) fn danmaku_append(&self, items: Vec<OverlayDanmaku>) {
        if let Some(stage) = self.stage_sink() {
            stage.append_danmaku(items);
        } else if let Some(engine) = &self.engine {
            engine.append_danmaku(items);
        }
    }


    pub(in crate::shell) fn danmaku_config(&self, style: DanmakuStyle) {
        if let Some(stage) = self.stage_sink() {
            stage.set_danmaku_config(style);
        } else if let Some(engine) = &self.engine {
            engine.set_danmaku_config(style);
        }
    }


    pub(in crate::shell) fn danmaku_filter(&self, filter: DanmakuFilter) {
        if let Some(stage) = self.stage_sink() {
            stage.set_danmaku_filter(filter);
        } else if let Some(engine) = &self.engine {
            engine.set_danmaku_filter(filter);
        }
    }


    /// 透明层投递闸:仅在系统媒体引擎真正驱动播放时返回 Some。
    pub(in crate::shell) fn stage_sink(&self) -> Option<&Arc<DanmakuStage>> {
        if self.native_playback {
            self.native_stage.as_ref()
        } else {
            None
        }
    }


    /// 播放控制分流闸(native 驱动时返回引擎;直播/mpv 路径返回 None,调用方
    /// 走视频引擎)。残留的 native_player 不得劫持直播控制,否则直播的进度/音量
    /// 会打到已卸载的引擎上。
    pub(in crate::shell) fn native_sink(&self) -> Option<&Arc<NativePlayer>> {
        if self.native_playback {
            self.native_player.as_ref()
        } else {
            None
        }
    }


    /// S3:引擎翻转后重新落位弹幕 —— 先换代作废旧链在途分段(避免双投/续链),
    /// 再清目标承接方、补一次当前样式/规则,最后从播放头所在分段起重走预存链。
    pub(in crate::shell) fn reseed_danmaku(&mut self, start_secs: f64, context: &ComponentContext<Self>) {
        self.danmaku_epoch = self.danmaku_epoch.wrapping_add(1);
        self.danmaku_replace(Vec::new());
        let st = self.settings.clone();
        self.danmaku_config(danmaku_style_from(&st));
        self.danmaku_filter(danmaku_filter_from(&st));
        self.fetch_danmaku(self.playback_gen, start_secs, context);
    }

    /// M7:释放弹幕透明层(层线程 + 独立 D3D device 随引擎下线)。
    pub(in crate::shell) fn release_stage(&mut self) {
        if self.native_stage.take().is_some() {
            eprintln!("[qaq-bx-ui] native danmaku stage: 已释放(层线程下线)");
        }
    }


    /// 预存弹幕:按 6 分钟段链式拉取,播放头所在段最先到货即画,其余段
    /// 后台陆续补齐(每段 DanmakuPart → 引擎 AppendDanmaku → 链式取下一段)。
    /// 不再等全片拉完才上屏;分P 切换须先 set_danmaku(Vec::new()) 清层。
    pub(in crate::shell) fn fetch_danmaku(&mut self, seq: u64, start_secs: f64, context: &ComponentContext<Self>) {
        if self.current_meta.is_none() {
            return;
        }
        let first = danmaku_segment_at(start_secs);
        self.danmaku_first_seg = first;
        self.fetch_danmaku_segment(seq, self.danmaku_epoch, first, context);
    }


    /// 拉取单个弹幕分段(预存链的一环;结果经 DanmakuPart/DanmakuFailed 驱动下一段)。
    pub(in crate::shell) fn fetch_danmaku_segment(
        &mut self,
        seq: u64,
        epoch: u64,
        index: u32,
        context: &ComponentContext<Self>,
    ) {
        let Some(meta) = self.current_meta.clone() else {
            return;
        };
        let auth = self.auth_service.clone();
        let nav = auth.nav_client();
        context.spawn_background(move |_| {
            let client = DanmakuClient::with_nav(auth.http(), auth.credential_provider(), nav);
            match block_on(client.segment(meta.cid, meta.aid, index)) {
                Ok(segment) => Message::DanmakuPart {
                    seq,
                    epoch,
                    index,
                    segment,
                },
                Err(error) => Message::DanmakuFailed {
                    seq,
                    epoch,
                    index,
                    reason: error.to_string(),
                },
            }
        });
    }


    /// 预存链的下一段(播放头段 → 其余段升序;全部段到货后返回 None 收束)。
    pub(in crate::shell) fn next_danmaku_index(&self, current: u32) -> Option<u32> {
        let total = self
            .current_meta
            .as_ref()
            .map_or(0, |meta| danmaku_segment_count(meta.duration as f64));
        next_danmaku_segment(current, self.danmaku_first_seg, total)
    }


}

