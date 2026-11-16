//! 视频面板:SwapChainPanel ↔ VideoEngine 挂载桥(`docs/video-channel.md`)。
//!
//! 职责:持有 panel ElementRef;Metrics 上报引擎(驱动 resize);把引擎产出
//! 的 swapchain 经 `request_set_swap_chain`(UI 线程)挂上 panel。
//! 挂载驱动:**input_changed**(播放页 250ms 状态节拍 → input.tick 递增)+
//! metrics 回调即时尝试;resize 后引擎重放 pending,由下一次 tick 重挂。

use std::sync::Arc;

use windows_core::{Interface as _, IUnknown};
use windows_reactor::{
    Component, ComponentContext, ElementRef, SwapChainPanel, SwapChainPanelEvent, View, ViewContext,
};

use bili_video::engine::VideoEngine;

/// 引擎转移的原始 COM 指针 → 本侧 IUnknown(接管所有权,Drop Release;
/// 引擎侧 `into_raw` 与本侧 `from_raw` 引用计数闭合;两侧均为
/// fork 的 windows-core 0.101)。
fn adopt_swap_chain(raw: *mut std::ffi::c_void) -> Option<IUnknown> {
    (!raw.is_null()).then(|| unsafe { IUnknown::from_raw(raw) })
}

#[derive(Clone)]
pub struct VideoSurfaceInput {
    pub engine: Arc<VideoEngine>,
    /// 状态节拍计数:每次变化触发一次挂载尝试(input_changed 驱动)。
    pub tick: u32,
}

impl PartialEq for VideoSurfaceInput {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.engine, &other.engine) && self.tick == other.tick
    }
}

/// 本组件无页面语义消息(挂载由 input_changed 驱动)。
#[derive(Clone, PartialEq)]
pub enum Message {}

pub struct VideoSurface {
    engine: Arc<VideoEngine>,
    panel: ElementRef<SwapChainPanel>,
}

impl Component for VideoSurface {
    type Input = VideoSurfaceInput;
    type Message = Message;

    fn create(input: &Self::Input, _context: &ComponentContext<Self>) -> Self {
        Self { engine: input.engine.clone(), panel: ElementRef::new() }
    }

    fn input_changed(&mut self, input: &Self::Input, _context: &ComponentContext<Self>) {
        self.engine = input.engine.clone();
        self.try_mount();
    }

    fn update(&mut self, _message: Message, _context: &ComponentContext<Self>) {
        match _message {}
    }

    fn view(&self, _input: &Self::Input, context: &mut ViewContext<Self>) -> View {
        // metrics 观察:尺寸/DPI 变化 → 引擎 resize;顺带即时尝试挂载
        // (覆盖"面板先就绪、swapchain 后产出"的时序)。
        let engine = self.engine.clone();
        let panel_for_metrics = self.panel.clone();
        let panel_in_callback = self.panel.clone();
        context.use_effect_guard("surface-metrics", (), move || {
            let engine_in_callback = engine.clone();
            panel_for_metrics.observe_surface_metrics(move |event| {
                let SwapChainPanelEvent::Metrics { width, height, scale_x, .. } = event else {
                    return;
                };
                engine_in_callback.set_metrics(width, height, scale_x);
                let Some(raw) = engine_in_callback.take_pending() else { return };
                let Some(chain) = adopt_swap_chain(raw) else { return };
                let engine_on_attach = engine_in_callback.clone();
                let accepted = panel_in_callback.request_set_swap_chain(
                    chain.clone(),
                    move |result| engine_on_attach.mark_attached(result.is_ok()),
                );
                if !accepted {
                    engine_in_callback.put_back_pending(chain.into_raw());
                }
            })
        });

        // 卸载清理:解绑引擎 attached + 清 panel 的 swapchain(重进播放页再挂)。
        let engine = self.engine.clone();
        let panel_for_cleanup = self.panel.clone();
        context.use_effect("lifetime", (), move || {
            Some(Box::new(move || {
                engine.detach_panel();
                let _ = panel_for_cleanup.request_clear_swap_chain(|_| {});
            }))
        });

        SwapChainPanel::new().element_ref(&self.panel).into()
    }
}

impl VideoSurface {
    /// 尝试把引擎的待挂载 swapchain 挂上 panel(UI 线程;无 pending 则空操作)。
    fn try_mount(&mut self) {
        let Some(raw) = self.engine.take_pending() else { return };
        let Some(chain) = adopt_swap_chain(raw) else { return };
        // request 接管克隆引用;入队失败(面板未实例化)时原件放回引擎槽,
        // 由 300ms 后的下一拍重试(input.tick 持续递增)。
        let engine = self.engine.clone();
        let accepted = self.panel.request_set_swap_chain(
            chain.clone(),
            move |result| engine.mark_attached(result.is_ok()),
        );
        if !accepted {
            self.engine.put_back_pending(chain.into_raw());
        }
    }
}
