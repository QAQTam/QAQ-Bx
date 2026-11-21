//! SMTC 系统媒体控制域:SmtcAction 命令回环(播放/暂停/上一条/下一条)、sync_smtc
//! 元数据/缩略图/进度同步、apply_playback_want 播放意愿仲裁、reset_smtc_writer 引擎切换时重挂。
//!
//! 本文件由 P3 拆解生成:match 臂与配套方法自原 update()/impl AppShell 逐字搬运,
//! 消息路由见 handlers/mod.rs(穷尽 match)。单元测试锚点见 shell/tests.rs。

use super::super::*;

impl AppShell {
    /// smtc 域消息臂(2 变体,与 message.rs 分组注释对应);未命中变体静默忽略。
    pub(super) fn handle_smtc(&mut self, message: Message, context: &ComponentContext<Self>) -> bool {
        match message {
            Message::SmtcAction(Some(cmd)) => {
                // 诊断锚:系统媒体键 / 控制栏按钮确实打到了本进程。
                eprintln!("[qaq-bx-ui] smtc action: {cmd:?}");
                // 系统媒体控件/媒体键的播放暂停请求统一走同一入口:
                // native → MediaPlayer,mpv → 视频引擎(两条输入路径不两套语义)。
                let want_play = matches!(cmd, SmtcCommand::Play);
                self.apply_playback_want(want_play, context);
                self.listen_smtc(context);
            }
            Message::SmtcAction(None) => {}
            _ => {}
        }
        false
    }

    /// M7:SMTC 写权交接:切走的一方必须停止手写状态并收尾(否则锁屏/控制栏留残卡
    /// 或不再更新)。native→mpv 交回窗口级桥;mpv→native 交回 player 自持控件
    /// (窗口级桥继续只当输入通道);直播按 mpv 一侧接管。
    pub(in crate::shell) fn reset_smtc_writer(&mut self, to_native: bool) {
        if let Some(bound) = self.smtc_native.take() {
            bound.set_stopped();
        }
        if to_native
            && let Some(smtc) = &self.smtc
        {
            smtc.set_stopped();
        }
        self.smtc_bvid = None;
        self.smtc_playing = None;
    }


    pub(in crate::shell) fn listen_smtc(&self, context: &ComponentContext<Self>) {
        let Some(rx) = &self.smtc_rx else {
            return;
        };
        let rx = Arc::clone(rx);
        context.spawn_background(move |_| {
            let cmd = rx.lock().ok().and_then(|guard| guard.recv().ok());
            Message::SmtcAction(cmd)
        });
    }


    /// 播放意愿落地的唯一入口(UI 按钮/快捷键、SMTC 指令、系统媒体键同一条路径):
    /// native → 系统媒体引擎,mpv → 视频引擎;两条路径共享「末帧重播回零」语义。
    pub(in crate::shell) fn apply_playback_want(&mut self, want_play: bool, context: &ComponentContext<Self>) {
        // 先克隆句柄:下面要写 self.playing/self.position,不能一路借用 self。
        if let Some(player) = self.native_sink().cloned() {
            let status = player.status();
            if want_play {
                // 播放结束(锁屏点播放)→ 回零重播。
                if status.finished {
                    let _ = player.seek(0.0);
                    self.position = 0;
                }
                let _ = player.play();
                self.playing = true;
            } else {
                let _ = player.pause();
                self.playing = false;
            }
            self.sync_smtc(context);
            return;
        }
        let Some(engine) = &self.engine else {
            return;
        };
        let status = engine.status();
        if !status.has_media() {
            return; // 媒体未加载(解析中/失败):按钮无操作对象。
        }
        if want_play {
            if status.eof {
                // 末帧重播:回零续播。
                engine.seek(0.0);
            }
            engine.play();
            self.playing = true;
        } else {
            engine.pause();
            self.playing = false;
        }
        self.sync_smtc(context);
    }


    /// 窗口级 SMTC 桥懒建(按钮输入通道)。native 模式下也建它:系统媒体会话按
    /// 窗口绑定,媒体键/控制栏按钮的 ButtonPressed 只经这条路进来,而展示状态与
    /// 元数据交给 MediaPlayer 自持控件写 —— 一个管输入、一个管展示,不互相覆盖。
    pub(in crate::shell) fn ensure_window_smtc(&mut self, context: &ComponentContext<Self>) {
        if self.smtc.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        match SmtcController::try_init(tx) {
            Some(ctrl) => {
                self.smtc = Some(ctrl);
                self.smtc_rx = Some(Arc::new(Mutex::new(rx)));
                self.listen_smtc(context);
            }
            // 诊断锚:窗口级桥建不起来 = 媒体键/控制栏按钮的输入通道缺失。
            None => eprintln!("[qaq-bx-ui] smtc window bridge 不可用(按钮输入通道缺失)"),
        }
    }


    pub(in crate::shell) fn sync_smtc(&mut self, context: &ComponentContext<Self>) {
        // item 先取快照:后面要改 self 的多个字段,不能一路借用 now_playing。
        let item = self.now_playing.clone();
        if let Some(item) = item.as_ref() {
            // v0.3 native:展示状态/时间轴交给 MediaPlayer 自持 SMTC(系统管线驱动),
            // 我们只补元数据;输入通道(媒体键/控制栏按钮)始终走窗口级桥。
            let player = self
                .native_sink()
                .and_then(|p| p.current_player());
            if let Some(player) = player.as_ref() {
                self.ensure_window_smtc(context);
                if self
                    .smtc_native
                    .as_ref()
                    .is_none_or(|bound| !bound.is_bound_to(player))
                {
                    self.smtc_native = NativeSmtc::from_player(player);
                    // 面板重建后 player 变更:元数据与状态锚都得重来一次。
                    self.smtc_bvid = None;
                    self.smtc_playing = None;
                    if let Some(bound) = &self.smtc_native {
                        eprintln!(
                            "[qaq-bx-ui] smtc native: 控件已绑定(status={:?})",
                            bound.playback_status()
                        );
                    }
                }
                if self.smtc_bvid.as_deref() != Some(item.bvid.as_str()) {
                    if let Some(bound) = &self.smtc_native {
                        let cover_bytes = item.cover.as_ref().map(|img| img.as_bytes());
                        bound.update_metadata(&item.title, &item.author, cover_bytes);
                    }
                    self.smtc_bvid = Some(item.bvid.clone());
                }
                // 只读不写:锁屏/控制栏显示的就是系统侧状态,变化时留诊断锚。
                let system_playing = self
                    .smtc_native
                    .as_ref()
                    .and_then(|bound| bound.playback_status())
                    == Some(MediaPlaybackStatus::Playing);
                if self.smtc_playing != Some(system_playing) {
                    self.smtc_playing = Some(system_playing);
                    eprintln!(
                        "[qaq-bx-ui] smtc native: status={}",
                        if system_playing { "Playing" } else { "NotPlaying" }
                    );
                }
                return;
            }
            // 系统媒体引擎在驱动但尚未 attach(装载在途/面板重建中):不写状态,
            // 但仍要把输入通道备好(否则重建期间的媒体键会丢)。
            if self.native_playback {
                self.ensure_window_smtc(context);
                return;
            }
            self.ensure_window_smtc(context);
            if self.smtc_bvid.as_deref() != Some(item.bvid.as_str()) {
                if let Some(smtc) = &self.smtc {
                    let cover_bytes = item.cover.as_ref().map(|img| img.as_bytes());
                    smtc.update_metadata(&item.title, &item.author, cover_bytes);
                }
                self.smtc_bvid = Some(item.bvid.clone());
            }
            if self.smtc_playing != Some(self.playing) {
                if let Some(smtc) = &self.smtc {
                    smtc.set_playing(self.playing);
                }
                self.smtc_playing = Some(self.playing);
            }
        } else if self.smtc_bvid.is_some() || self.smtc_playing.is_some() {
            if let Some(smtc) = &self.smtc {
                smtc.set_stopped();
            }
            if let Some(bound) = &self.smtc_native {
                bound.set_stopped();
            }
            self.smtc_native = None;
            self.smtc_bvid = None;
            self.smtc_playing = None;
        }
    }


}

