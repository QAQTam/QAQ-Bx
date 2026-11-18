//! 系统媒体播放面板(MediaPlayerElement ↔ NativePlayer 注入桥,v0.3 正式版)。
//!
//! 协议:WinUI3 MPE 的内部 `MediaPlayer` 只读——渲染必须由元素自持的 player
//! 驱动。本组件在 `input_changed`(shell 250ms mount 节拍)按「装载世代」驱动
//! 注入:引擎工作线程产出的 MediaSource(`player.pending_source()`)经
//! `request_set_media_source_get_player`(UI 线程 imperative)设进 MPE 并读回
//! player,交回 `attach_player(raw, seq)` 接管。**首次装载与换画质/换分P 重载
//! 走同一条路径**(是否需要再注入一次由引擎侧世代账本决定);一次注入请求在途
//! 时不重复发,失败(面板未实例化)由下一拍按同世代重试。
//!
//! 本组件不做任何网络/解析工作:装载耗时全在引擎的装载线程(UI 线程只发 imperative)。

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

/// 本组件无页面语义消息(注入由 input_changed 驱动)。
#[derive(Clone, PartialEq)]
pub enum Message {}

pub struct NativeSurface {
    player: Arc<NativePlayer>,
    element: ElementRef<MediaPlayerElement>,
    /// 注入请求在途(completion / 入队失败回写;input_changed 据此避免重复发)。
    attaching: Arc<std::sync::atomic::AtomicBool>,
}

impl Component for NativeSurface {
    type Input = NativeSurfaceInput;
    type Message = Message;

    fn create(input: &Self::Input, _context: &ComponentContext<Self>) -> Self {
        Self {
            player: input.player.clone(),
            element: ElementRef::new(),
            attaching: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    fn input_changed(&mut self, input: &Self::Input, _context: &ComponentContext<Self>) {
        self.player = input.player.clone();
        self.try_attach();
    }

    fn update(&mut self, _message: Message, _context: &ComponentContext<Self>) {
        match _message {}
    }

    fn view(&self, _input: &Self::Input, context: &mut ViewContext<Self>) -> View {
        // 面板卸载:元素自持的 MediaPlayer 随视图树拆走,把断点交给引擎并解绑。
        // 新面板挂载(view 首次创建)时引擎用断点原位重载(`remount_pending`)。
        let engine = self.player.clone();
        context.use_effect("native-lifetime", (), move || {
            let engine_mount = engine.clone();
            // 挂载即消费续播断点(有断点才会重新提交装载,不阻塞 UI)。
            let _ = engine_mount.remount_pending();
            Some(Box::new(move || {
                engine.note_surface_detached();
            }))
        });

        // AutoPlay 由引擎 attach 时关闭,起播时机 shell 统一决定。
        MediaPlayerElement::new().element_ref(&self.element).into()
    }
}

impl NativeSurface {
    /// 取引擎待注入的产物并交 MPE;无新产物(未装载 / 该世代已注入 / 别的实例
    /// 正在注入)则空操作。
    fn try_attach(&mut self) {
        let atomic = std::sync::atomic::Ordering::SeqCst;
        if self.attaching.swap(true, atomic) {
            return; // 本实例上一次注入请求仍在途
        }
        // 领取全局注入权:同一份 MediaSource 只允许绑定一个 player。
        let Some((seq, source)) = self.player.claim_pending_source() else {
            self.attaching.store(false, atomic);
            return;
        };
        eprintln!("[qaq-bx-ui] native surface: attach attempt(seq={seq})");
        let engine = self.player.clone();
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
                        if let Err(e) = engine.attach_player(unknown, seq) {
                            eprintln!("[qaq-bx-ui] native engine attach failed: {e}");
                        }
                    }
                    // 面板未就绪:放回注入权,下一拍(mount_tick)按同世代重试。
                    Err(_) => engine.release_injection(seq),
                }
            });
        if !accepted {
            eprintln!("[qaq-bx-ui] native surface: request not accepted (panel 未实例化)");
            self.attaching.store(false, atomic);
            self.player.release_injection(seq); // 入队失败:放回注入权
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 引擎无产物时 try_attach 空操作且不卡在途标志(节拍可继续推进)。
    #[test]
    fn try_attach_without_pending_source_is_a_noop() {
        let mut surface = NativeSurface {
            player: Arc::new(NativePlayer::new()),
            element: ElementRef::new(),
            attaching: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        surface.try_attach();
        surface.try_attach();
        assert!(
            !surface
                .attaching
                .load(std::sync::atomic::Ordering::SeqCst)
        );
    }

    /// input 等价判定只看引擎身份 + 节拍(引用计数变化不该触发重挂)。
    #[test]
    fn input_equality_is_engine_identity_and_tick() {
        let player = Arc::new(NativePlayer::new());
        let a = NativeSurfaceInput {
            player: player.clone(),
            tick: 3,
        };
        let b = NativeSurfaceInput {
            player: player.clone(),
            tick: 3,
        };
        let c = NativeSurfaceInput { player, tick: 4 };
        assert!(a == b, "同引擎同节拍应等价(不触发重挂)");
        assert!(a != c, "节拍变化必须触发一次注入尝试");
    }
}
