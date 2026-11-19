//! 视频面板:SwapChainPanel ↔ VideoEngine 挂载桥(`docs/video-channel.md`)。
//!
//! 职责:持有 panel ElementRef;Metrics 上报引擎(驱动 resize);把引擎产出
//! 的 swapchain 经 `request_set_swap_chain`(UI 线程)挂上 panel。
//! 挂载驱动:**input_changed**(播放页 250ms 状态节拍 → input.tick 递增)+
//! metrics 回调即时尝试;resize 后引擎重放 pending,由下一次 tick 重挂。

use std::sync::Arc;

use windows_core::{IUnknown, Interface as _};
use windows_reactor::{
    Component, ComponentContext, ElementRef, IntegrationError, SwapChainPanel,
    SwapChainPanelEvent, View, ViewContext,
};

use qaq_bx_video::engine::VideoEngine;

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
        Self {
            engine: input.engine.clone(),
            panel: ElementRef::new(),
        }
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
                handle_metrics_event(&engine_in_callback, &panel_in_callback, event);
            })
        });

        // 卸载清理:解绑引擎 attached。**不主动 clear 面板**(黑屏排查 2026-11):
        // 组件卸载时 panel 元素随视图树一起销毁,SwapChain 引用由 XAML 释放,
        // request_clear_swap_chain 只会把视频区主动清黑拉长空窗;引擎侧
        // `detach_panel` 已把 swapchain 放回待挂载槽,新面板首个 metrics
        // 事件即重挂。
        let engine = self.engine.clone();
        context.use_effect("lifetime", (), move || {
            Some(Box::new(move || {
                engine.detach_panel();
            }))
        });

        SwapChainPanel::new().element_ref(&self.panel).into()
    }
}

fn handle_metrics_event(
    engine: &Arc<VideoEngine>,
    panel: &ElementRef<SwapChainPanel>,
    event: SwapChainPanelEvent,
) {
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
    apply_surface_metrics(engine, panel, width, height, scale_x, scale_y);
}

fn apply_surface_metrics(
    engine: &Arc<VideoEngine>,
    panel: &ElementRef<SwapChainPanel>,
    width: f64,
    height: f64,
    scale_x: f32,
    scale_y: f32,
) {
    engine.set_metrics(width, height, scale_x, scale_y);
    attach_pending(engine, panel);
}

/// 尝试把引擎的待挂载 swapchain 挂上 panel(UI 线程;无 pending 则空操作)。
fn attach_pending(engine: &Arc<VideoEngine>, panel: &ElementRef<SwapChainPanel>) {
    let Some((gen_id, raw)) = engine.take_pending() else {
        return;
    };
    let Some(chain) = adopt_swap_chain(raw) else {
        return;
    };
    // request 接管克隆引用;入队失败或面板瞬态 Unavailable 时原件放回当前 gen 槽,
    // 由 250ms 后的下一拍重试(input.tick 持续递增)。
    let engine_on_attach = engine.clone();
    let chain_on_err = chain.clone();
    let accepted = panel.request_set_swap_chain(chain.clone(), move |result| match result {
        Ok(()) => engine_on_attach.mark_attached(gen_id, true),
        Err(IntegrationError::Unavailable) => {
            engine_on_attach.put_back_pending(gen_id, chain_on_err.clone().into_raw());
        }
        Err(e) => {
            eprintln!("[qaq-bx-ui] VideoSurface SetSwapChain 失败(gen={gen_id}):{e:?}");
            engine_on_attach.mark_attached(gen_id, false);
        }
    });
    if !accepted {
        engine.put_back_pending(gen_id, chain.into_raw());
    }
}

impl VideoSurface {
    fn try_mount(&mut self) {
        attach_pending(&self.engine, &self.panel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_surface_metrics_forwards_high_dpi_without_panicking() {
        let engine = VideoEngine::new();
        let panel = ElementRef::<SwapChainPanel>::new();
        apply_surface_metrics(&engine, &panel, 844.0, 420.0, 1.5, 1.5);
        apply_surface_metrics(&engine, &panel, 366.0, 206.0, 2.0, 2.0);
        handle_metrics_event(&engine, &panel, SwapChainPanelEvent::Rendering);
    }
}
