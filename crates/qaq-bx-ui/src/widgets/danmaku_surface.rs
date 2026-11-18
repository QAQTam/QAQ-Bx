//! 弹幕透明覆盖面板(`DanmakuStage` ↔ SwapChainPanel 挂载桥,v0.3 native 模式)。
//!
//! 与 `video_surface.rs` 同一套挂载协议(引擎侧产出 swapchain → UI 线程
//! `request_set_swap_chain` → completion 回写 attached),区别只在两点:
//! - 这一层的画布是**预乘 alpha 的透明层**,叠在 MediaPlayerElement 之上
//!   (Grid 同 cell),不参与视频解码;
//! - resize 只需 ResizeBuffers,**不需要重挂**(合成链对象身份不变),
//!   所以卸载时同样不主动 clear 面板(见 `docs/dev-notes/resize-black-screen.md`)。
//!
//! 挂载驱动同样是 **input_changed**(播放页 250ms 状态节拍 → input.tick)+
//! metrics 回调即时尝试;面板未实例化时把指针放回引擎槽,下一拍重试。

use std::sync::Arc;

use windows_core::{IUnknown, Interface as _};
use windows_reactor::{
    Component, ComponentContext, ElementRef, SwapChainPanel, SwapChainPanelEvent, View, ViewContext,
};

use qaq_bx_video::stage::DanmakuStage;

/// 引擎转移的原始 COM 指针 → 本侧 IUnknown(接管所有权,Drop 释放;
/// 与 `DanmakuStage::take_pending` 的 `into_raw` 引用计数闭合)。
fn adopt_swap_chain(raw: *mut std::ffi::c_void) -> Option<IUnknown> {
    (!raw.is_null()).then(|| unsafe { IUnknown::from_raw(raw) })
}

#[derive(Clone)]
pub struct DanmakuSurfaceInput {
    pub stage: Arc<DanmakuStage>,
    /// 状态节拍计数:每次变化触发一次挂载尝试(与 VideoSurfaceInput 同语义)。
    pub tick: u32,
}

impl PartialEq for DanmakuSurfaceInput {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.stage, &other.stage) && self.tick == other.tick
    }
}

/// 本组件无页面语义消息(挂载由 input_changed / metrics 驱动)。
#[derive(Clone, PartialEq)]
pub enum Message {}

pub struct DanmakuSurface {
    stage: Arc<DanmakuStage>,
    panel: ElementRef<SwapChainPanel>,
}

impl Component for DanmakuSurface {
    type Input = DanmakuSurfaceInput;
    type Message = Message;

    fn create(input: &Self::Input, _context: &ComponentContext<Self>) -> Self {
        Self {
            stage: input.stage.clone(),
            panel: ElementRef::new(),
        }
    }

    fn input_changed(&mut self, input: &Self::Input, _context: &ComponentContext<Self>) {
        self.stage = input.stage.clone();
        attach_pending(&self.stage, &self.panel);
    }

    fn update(&mut self, _message: Message, _context: &ComponentContext<Self>) {
        match _message {}
    }

    fn view(&self, _input: &Self::Input, context: &mut ViewContext<Self>) -> View {
        // metrics 观察:尺寸/DPI → 层线程 ResizeBuffers;顺带即时尝试挂载
        // (覆盖「面板先就绪、swapchain 后产出」的时序)。
        let stage = self.stage.clone();
        let panel_for_metrics = self.panel.clone();
        let panel_in_callback = self.panel.clone();
        context.use_effect_guard("danmaku-metrics", (), move || {
            let stage_in_callback = stage.clone();
            panel_for_metrics.observe_surface_metrics(move |event| {
                let SwapChainPanelEvent::Metrics {
                    width,
                    height,
                    scale_x,
                    scale_y,
                    ..
                } = event
                else {
                    return;
                };
                stage_in_callback.set_metrics(width, height, scale_x, scale_y);
                attach_pending(&stage_in_callback, &panel_in_callback);
            })
        });

        // 卸载:仅解绑 attached(swapchain 放回待挂载槽);不清面板、不销毁层线程。
        let stage = self.stage.clone();
        context.use_effect("danmaku-lifetime", (), move || {
            Some(Box::new(move || {
                stage.detach_panel();
            }))
        });

        // 透明层不吃命中:Panel 背景默认为 null,点击仍由上层 double-click Border 收。
        SwapChainPanel::new().element_ref(&self.panel).into()
    }
}

/// 尝试把层的待挂载 swapchain 挂上 panel(UI 线程;无 pending 则空操作)。
fn attach_pending(stage: &Arc<DanmakuStage>, panel: &ElementRef<SwapChainPanel>) {
    let Some(raw) = stage.take_pending() else {
        return;
    };
    let Some(chain) = adopt_swap_chain(raw) else {
        return;
    };
    let stage_on_attach = stage.clone();
    let accepted = panel.request_set_swap_chain(chain.clone(), move |result| {
        stage_on_attach.mark_attached(result.is_ok())
    });
    if !accepted {
        // 入队失败(面板未实例化):原件放回,由下一拍(tick)重试。
        stage.put_back_pending(chain.into_raw());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// metrics 转发对非法尺寸静默忽略,且不影响层的挂载状态判定。
    ///
    /// 注:这里**不**调用 `attach_pending` —— imperative 需要 ElementRef 绑定到
    /// UI 线程的活动元素,无头单测里那是 phantom 引用(与 `video_surface.rs`
    /// 同一约束:它只在引擎尚未产出 swapchain 时才走到空槽早退)。
    #[test]
    fn metrics_forwarding_ignores_invalid_sizes_without_ui() {
        let stage = DanmakuStage::new();
        let panel = ElementRef::<SwapChainPanel>::new();
        stage.set_metrics(366.0, 206.0, 2.0, 2.0);
        stage.set_metrics(0.0, 0.0, 1.0, 1.0);
        stage.set_metrics(f64::NAN, 206.0, 1.0, 1.0);
        assert!(!stage.attached());
        assert!(panel.clone() == panel, "phantom 引用自比一致");
    }

    /// input 等价只看层身份与节拍:重建 Arc 不该触发重挂。
    #[test]
    fn input_equality_is_stage_identity_and_tick() {
        let stage = DanmakuStage::new();
        let a = DanmakuSurfaceInput {
            stage: stage.clone(),
            tick: 1,
        };
        let b = DanmakuSurfaceInput {
            stage: stage.clone(),
            tick: 1,
        };
        let c = DanmakuSurfaceInput { stage, tick: 2 };
        assert!(a == b);
        assert!(a != c);
    }
}
