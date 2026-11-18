//! 系统媒体播放面板(MediaPlayerElement ↔ NativePlayer 接管桥,v0.3 spike)。
//!
//! 协议:WinUI3 MPE 的内部 `MediaPlayer` 只读——渲染必须由元素自持的 player
//! 驱动。本组件在首个 `input_changed`(shell 250ms mount 节拍)经
//! `request_get_media_player`(UI 线程 imperative)把该 player 交给引擎
//! `attach_player`;此后引擎的 load/play/pause/seek 全部作用于这个 player,
//! MPE 自动出画。attach 一次成功即止;失败(面板未实例化)下一拍重试。

use std::sync::Arc;

use windows_reactor::{
    Component, ComponentContext, ElementRef, MediaPlayerElement, View, ViewContext,
};

use qaq_bx_video::native::NativePlayer;

#[derive(Clone)]
pub struct NativeSurfaceInput {
    pub player: Arc<NativePlayer>,
    /// 挂载尝试节拍(250ms mount 脉冲;与 VideoSurfaceInput 同语义)。
    pub tick: u32,
}

impl PartialEq for NativeSurfaceInput {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.player, &other.player) && self.tick == other.tick
    }
}

/// 本组件无页面语义消息(接管由 input_changed 驱动)。
#[derive(Clone, PartialEq)]
pub enum Message {}

pub struct NativeSurface {
    player: Arc<NativePlayer>,
    element: ElementRef<MediaPlayerElement>,
    /// attach 在途/已成功标志(completion 回写;input_changed 据此重试)。
    attaching: Arc<std::sync::atomic::AtomicBool>,
    attached: Arc<std::sync::atomic::AtomicBool>,
}

impl Component for NativeSurface {
    type Input = NativeSurfaceInput;
    type Message = Message;

    fn create(input: &Self::Input, _context: &ComponentContext<Self>) -> Self {
        Self {
            player: input.player.clone(),
            element: ElementRef::new(),
            attaching: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            attached: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    fn input_changed(&mut self, input: &Self::Input, _context: &ComponentContext<Self>) {
        self.player = input.player.clone();
        self.try_attach();
    }

    fn update(&mut self, _message: Message, _context: &ComponentContext<Self>) {
        match _message {}
    }

    fn view(&self, _input: &Self::Input, _context: &mut ViewContext<Self>) -> View {
        // AutoPlay 由引擎 attach 时关闭,起播时机 shell 统一决定。
        MediaPlayerElement::new().element_ref(&self.element).into()
    }
}

impl NativeSurface {
    fn try_attach(&mut self) {
        let atomic = std::sync::atomic::Ordering::SeqCst;
        if self.attached.load(atomic) || self.attaching.swap(true, atomic) {
            return;
        }
        // 引擎尚未 load(build_source 未产出 MediaSource):等下一拍再试。
        let Some(source) = self.player.take_pending_source_raw() else {
            self.attaching.store(false, atomic);
            return;
        };
        eprintln!("[qaq-bx-ui] native surface: attach attempt");
        let engine = self.player.clone();
        let attached = self.attached.clone();
        let attaching = self.attaching.clone();
        let accepted = self
            .element
            .request_set_media_source_get_player(source, move |result| {
                eprintln!(
                    "[qaq-bx-ui] native surface: set_source_get_player completion ok={}",
                    result.is_ok()
                );
                attaching.store(false, atomic);
                match result {
                    Ok(unknown) => {
                        if engine.attach_player(unknown).is_ok() {
                            attached.store(true, atomic);
                        } else {
                            eprintln!("[qaq-bx-ui] native engine attach failed");
                        }
                    }
                    Err(_) => {} // 面板未就绪:下一拍(mount_tick)再试
                }
            });
        if !accepted {
            eprintln!("[qaq-bx-ui] native surface: request not accepted (panel 未实例化)");
            self.attaching.store(false, atomic); // 入队失败:下一拍重试
        }
    }
}

