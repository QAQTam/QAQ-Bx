//! 底栏「悬浮液态玻璃」(iOS 26 风格)——路线 0 阶段 1:纯 stock composition。
//!
//! 渲染栈:`compact_bottom_bar` 的悬浮胶囊 Border → 内容网格(本组件根,
//! `ElementRef<Grid>`)→ fork imperative `observe_composition_host` 拿
//! Compositor 与尺寸 → 圆角 ShapeVisual(半透明白 tint 霜面)→
//! `request_set_child_visual` 垫到 Tab 行之下。尺寸变化由 Metrics 事件
//! 驱动 geometry/visual 同步更新。
//!
//! **实测发现(2026-11-19)**:`CreateBackdropBrush()` 直接做形状填充被
//! 运行时拒绝(E_INVALIDARG)——backdrop brush 仅可作为
//! `CompositionEffectBrush.SetSourceParameter` 的效果图源(社区实现一致),
//! 所以「采样 + 模糊」必须走阶段 2 的效果图,阶段 1 只有 tint 半透明层。
//!
//! 组件范式照抄 `danmaku_surface.rs`(根组件 AppShell 的 use_effect 不
//! 触发——实测;子组件自持元素 + 自注册观察是生产验证过的路径)。
//!
//! 阶段 2(模糊):`Microsoft.UI.Composition.Effects` 效果结构体不在 fork
//! curated winmd 中(build.rs 有注),需手工实现 IGraphicsEffect /
//! IGraphicsEffectD2DInterop 后经 `CreateEffectFactory` 接入 GaussianBlur
//! + Saturation;当前为「清玻璃 + 霜面」形态。
//!
//! spike 第一问(composition backdrop 是否覆盖 SwapChainPanel / 弹幕这类
//! 独立 composition 子树):播放页真机截图验证,结论记
//! docs/dev-notes/liquid-glass-research.md。

use std::{cell::RefCell, rc::Rc};

use windows_core::{IUnknown, Interface as _};
use windows_numerics::Vector2;
use windows_reactor::{
    Component, ComponentContext, CompositionHostEvent, ElementRef, ElementObservation, Grid, View,
    ViewContext,
};

include!(concat!(env!("OUT_DIR"), "/winrt_bindings.rs"));

/// 悬浮胶囊外观常量(`chrome.rs` 的胶囊 Border 与本模块的玻璃几何共用)。
pub mod capsule {
    /// 胶囊高度(与既有底栏一致,Tab 行距不回退)。
    pub const HEIGHT: f64 = 64.0;
    /// 胶囊外圆角(全胶囊 = HEIGHT / 2)。
    pub const OUTER_RADIUS: f64 = HEIGHT / 2.0;
    /// 玻璃几何圆角:内缩 2 DIP,避免与 1px 描边相切出锯齿。
    pub const GLASS_RADIUS: f64 = OUTER_RADIUS - 2.0;
    /// 悬浮留白(左右 / 底部,DIP):脱边悬浮的视觉锚。
    pub const FLOAT_MARGIN: f64 = 10.0;
}

/// 中性 tint 霜面(白,α≈0.30)。不随主题自适应(诚实清单项:
/// ActualTheme 观察未接,若深色下发灰再引入主题观察重建 tint)。
const TINT_ALPHA: u8 = 0x4D;

/// 玻璃材质参数(iOS 26 霜面参考的 stock 近似)。
mod material {
    /// 高斯模糊标准差(D2D1_GAUSSIANBLUR_PROP_STANDARDDEVIATION)。
    pub const BLUR_STD_DEV: f32 = 9.0;
    /// 饱和度增益(D2D1_SATURATION_PROP_SATURATION;iOS 玻璃参考 ≈1.1-1.2)。
    pub const SATURATION: f32 = 1.15;
}

/// Metrics 尺寸 → 玻璃几何尺寸(非法值忽略;正数钳制)。
fn glass_metrics(width: f64, height: f64) -> Option<(f32, f32)> {
    if !width.is_finite() || !height.is_finite() || width <= 1.0 || height <= 1.0 {
        return None;
    }
    Some((width as f32, height as f32))
}

/// 组件入参:Tab 行内容(chrome.rs 每次渲染重建;structural PartialEq
/// 一致则组件不重挂,玻璃宿主元素保持绑定)。
#[derive(Clone, PartialEq)]
pub struct GlassBarInput {
    pub content: View,
}

/// 底栏玻璃宿主组件:根网格绑 `ElementRef<Grid>`,观察 composition host
/// 事件驱动玻璃资产的生命周期(Ready 建,Metrics 随尺寸)。
pub struct GlassBar {
    element: ElementRef<Grid>,
    state: Rc<RefCell<GlassState>>,
}

/// Ready 可能先于布局到达(零尺寸):compositor 先存,Metrics 有效尺寸补建。
#[derive(Default)]
struct GlassState {
    compositor: Option<Compositor>,
    runtime: Option<GlassRuntime>,
}

struct GlassRuntime {
    geometry: CompositionRoundedRectangleGeometry,
    container: ContainerVisual,
    children: Vec<Visual>,
}

impl Component for GlassBar {
    type Input = GlassBarInput;
    type Message = ();

    fn create(_input: &Self::Input, _context: &ComponentContext<Self>) -> Self {
        Self {
            element: ElementRef::new(),
            state: Rc::new(RefCell::new(GlassState::default())),
        }
    }

    fn update(&mut self, _message: Self::Message, _context: &ComponentContext<Self>) {}

    fn view(&self, input: &Self::Input, context: &mut ViewContext<Self>) -> View {
        let element = self.element.clone();
        let state = Rc::clone(&self.state);
        context.use_effect_guard("glass-host", (), move || {
            eprintln!("[glass] observe registered");
            GlassObserver { element, state }.observe()
        });

        Grid::new()
            .element_ref(&self.element)
            .children((input.content.clone(),))
            .into()
    }
}

/// 观察回调的借用拆分(回调持 Rc,不持组件引用)。
struct GlassObserver {
    element: ElementRef<Grid>,
    state: Rc<RefCell<GlassState>>,
}

impl GlassObserver {
    fn observe(&self) -> ElementObservation {
        let observer = GlassObserver {
            element: self.element.clone(),
            state: Rc::clone(&self.state),
        };
        self.element.observe_composition_host(move |event| match event {
            CompositionHostEvent::Ready {
                compositor,
                width,
                height,
                ..
            } => observer.on_ready(compositor, width, height),
            CompositionHostEvent::Metrics { width, height, .. } => {
                observer.on_metrics(width, height)
            }
        })
    }

    /// Ready:存 compositor;尺寸已就绪则立即建玻璃(否则等 Metrics)。
    fn on_ready(&self, compositor_unknown: IUnknown, width: f64, height: f64) {
        eprintln!("[glass] composition host ready w={width} h={height}");
        let Ok(compositor) = compositor_unknown.cast::<Compositor>() else {
            eprintln!("[glass] compositor cast failed");
            return;
        };
        let compositor = {
            let mut state = self.state.borrow_mut();
            state.compositor = Some(compositor);
            state.compositor.clone().expect("just stored")
        };
        if let Some((w, h)) = glass_metrics(width, height) {
            self.build(&compositor, w, h);
        }
    }

    /// Metrics:有效尺寸 → 无资产则补建,有资产则随尺寸同步。
    fn on_metrics(&self, width: f64, height: f64) {
        let Some((w, h)) = glass_metrics(width, height) else {
            return;
        };
        let needs_build = {
            let state = self.state.borrow();
            state.runtime.is_none() && state.compositor.is_some()
        };
        if needs_build {
            let compositor = self.state.borrow().compositor.clone().expect("checked");
            eprintln!("[glass] building glass on late metrics {w}x{h}");
            self.build(&compositor, w, h);
            return;
        }
        let state = self.state.borrow();
        let Some(runtime) = state.runtime.as_ref() else {
            return;
        };
        let v2 = Vector2 { x: w, y: h };
        let _ = runtime.geometry.SetSize(v2);
        for child in &runtime.children {
            if let Ok(sized) = child.cast::<IVisual>() {
                let _ = sized.SetSize(v2);
            }
        }
        if let Ok(visual) = runtime.container.cast::<IVisual>() {
            let _ = visual.SetSize(v2);
        }
    }

    /// 建玻璃资产并垫到宿主之下。任一步失败只记日志(下一拍 Metrics /
    /// 重绑重试),不影响 Tab 行交互。
    ///
    /// 结构:容器视觉(圆角几何裁剪)→ [效果刷 Sprite(模糊背板) +
    /// tint 色 Sprite]。实测约束:效果刷不能做形状填充(形状 SetFillBrush
    /// 对 effect brush 返回 E_INVALIDARG),必须走 SpriteVisual.Brush;
    /// 圆角经 CompositionGeometricClip(rounded rect geometry)裁剪。
    fn build(&self, compositor: &Compositor, w: f32, h: f32) {
        let Ok(shape_factory) = compositor.cast::<ICompositor5>() else {
            eprintln!("[glass] build failed: ICompositor5 cast");
            return;
        };
        let Ok(geometry) = shape_factory.CreateRoundedRectangleGeometry() else {
            eprintln!("[glass] build failed: CreateRoundedRectangleGeometry");
            return;
        };
        let radius = capsule::GLASS_RADIUS as f32;
        let _ = geometry.SetCornerRadius(Vector2 {
            x: radius,
            y: radius,
        });
        let _ = geometry.SetSize(Vector2 { x: w, y: h });
        // 圆角裁剪(容器与子视觉共用同一几何)。
        let clip = match self.build_clip(compositor, &geometry) {
            Some(clip) => clip,
            None => return,
        };
        let Ok(container) = compositor.CreateContainerVisual() else {
            eprintln!("[glass] build failed: CreateContainerVisual");
            return;
        };
        let Ok(children_coll) = container.Children() else {
            eprintln!("[glass] build failed: ContainerVisual.Children");
            return;
        };

        let mut children: Vec<Visual> = Vec::new();
        // 层 1:效果刷(模糊背板);失败时回退 tint-only 单层。
        match self.build_effect_brush(compositor) {
            Ok(effect_brush) => {
                if let Some(visual) =
                    self.filled_sprite(compositor, &effect_brush, w, h, &clip)
                {
                    let _ = children_coll.InsertAtBottom(&visual);
                    children.push(visual);
                }
            }
            Err(step) => {
                eprintln!("[glass] effect graph failed at {step}; tint-only fallback");
            }
        }
        // 层 2:tint 霜面色层。
        let Ok(tint) = compositor.CreateColorBrush() else {
            eprintln!("[glass] build failed: CreateColorBrush");
            return;
        };
        let _ = tint.SetColor(Color {
            a: TINT_ALPHA,
            r: 0xFF,
            g: 0xFF,
            b: 0xFF,
        });
        if let Ok(tint_brush) = tint.cast::<CompositionBrush>()
            && let Some(visual) = self.filled_sprite(compositor, &tint_brush, w, h, &clip)
        {
            let _ = children_coll.InsertAtBottom(&visual);
            children.push(visual);
        }

        let Ok(container_sized) = container.cast::<IVisual>() else {
            eprintln!("[glass] build failed: container->IVisual");
            return;
        };
        let _ = container_sized.SetSize(Vector2 { x: w, y: h });

        let container_iunknown = match container.cast::<IUnknown>() {
            Ok(unknown) => unknown,
            Err(_) => {
                eprintln!("[glass] build failed: container->IUnknown");
                return;
            }
        };
        let accepted = self.element.request_set_child_visual(Some(container_iunknown), |result| {
            match result {
                Ok(()) => eprintln!("[glass] child visual attached"),
                Err(error) => eprintln!("[glass] set child visual failed: {error:?}"),
            }
        });
        if accepted {
            eprintln!("[glass] child visual request enqueued");
            self.state.borrow_mut().runtime = Some(GlassRuntime {
                geometry,
                container,
                children,
            });
        } else {
            eprintln!("[glass] set child visual request not accepted (宿主未实例化)");
        }
    }

    /// 圆角几何裁剪(CompositionGeometricClip + rounded rect)。
    fn build_clip(
        &self,
        compositor: &Compositor,
        geometry: &CompositionRoundedRectangleGeometry,
    ) -> Option<CompositionGeometricClip> {
        let Ok(factory6) = compositor.cast::<ICompositor6>() else {
            eprintln!("[glass] build failed: ICompositor6 cast");
            return None;
        };
        let Ok(clip) = factory6.CreateGeometricClip() else {
            eprintln!("[glass] build failed: CreateGeometricClip");
            return None;
        };
        if clip.SetGeometry(geometry).is_err() {
            eprintln!("[glass] build failed: clip.SetGeometry");
            return None;
        }
        Some(clip)
    }

    /// 建一个填充 `brush`、圆角裁剪、尺寸 (w,h) 的 Sprite 视觉。
    fn filled_sprite(
        &self,
        compositor: &Compositor,
        brush: &CompositionBrush,
        w: f32,
        h: f32,
        clip: &CompositionGeometricClip,
    ) -> Option<Visual> {
        let Ok(sprite) = compositor.CreateSpriteVisual() else {
            eprintln!("[glass] build failed: CreateSpriteVisual");
            return None;
        };
        if sprite.SetBrush(brush).is_err() {
            eprintln!("[glass] build failed: sprite.SetBrush");
            return None;
        }
        let Ok(sized) = sprite.cast::<IVisual>() else {
            eprintln!("[glass] build failed: sprite->IVisual");
            return None;
        };
        let _ = sized.SetSize(Vector2 { x: w, y: h });
        if sized.SetClip(clip).is_err() {
            eprintln!("[glass] build failed: sprite.SetClip");
            return None;
        }
        sprite.cast::<Visual>().ok()
    }

    /// 效果刷:blur(saturation(backdrop))。
    /// 任何一步失败返回步骤名(用于日志与回退判定)。
    fn build_effect_brush(
        &self,
        compositor: &Compositor,
    ) -> Result<CompositionBrush, &'static str> {
        use crate::widgets::glass_fx::{glass_graph, IGraphicsEffectHand};

        let Ok(legacy) = compositor.cast::<ICompositor2>() else {
            return Err("ICompositor2 cast");
        };
        let Ok(backdrop) = legacy.CreateBackdropBrush() else {
            return Err("CreateBackdropBrush");
        };
        let Ok(backdrop_brush) = backdrop.cast::<CompositionBrush>() else {
            return Err("backdrop->CompositionBrush");
        };
        // 效果图源参数(运行时类);SetSourceParameter 按名字匹配,必须先
        // 经 IGraphicsEffect.put_Name 写入 "backdrop"。
        let Ok(source_param) = CompositionEffectSourceParameter::Create("backdrop") else {
            return Err("CompositionEffectSourceParameter::Create");
        };
        if let Ok(hand) = source_param.cast::<IGraphicsEffectHand>() {
            let name: windows_core::HSTRING = "backdrop".into();
            unsafe {
                let hr = (windows_core::Interface::vtable(&hand).put_name)(
                    windows_core::Interface::as_raw(&hand),
                    core::mem::transmute_copy(&name),
                );
                if hr.is_err() {
                    eprintln!("[glass] put_Name failed hr={:#x}", hr.0);
                }
            }
            drop(name);
        }
        let backdrop_param: windows_core::IUnknown = source_param.into();
        let top = glass_graph(material::BLUR_STD_DEV, material::SATURATION, backdrop_param);
        let Ok(effect) = top.as_graphics_effect() else {
            return Err("node->IGraphicsEffect");
        };
        let Ok(factory) = compositor.CreateEffectFactory(&effect) else {
            return Err("CreateEffectFactory");
        };
        let Ok(brush) = factory.CreateBrush() else {
            return Err("factory.CreateBrush");
        };
        if brush.SetSourceParameter("backdrop", &backdrop_brush).is_err() {
            return Err("SetSourceParameter");
        }
        brush.cast::<CompositionBrush>().map_err(|_| "effect->CompositionBrush")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Metrics 尺寸归一:非法(非有限 / 非正)忽略,合法值转 f32。
    #[test]
    fn glass_metrics_rejects_invalid_and_clamps() {
        assert_eq!(glass_metrics(400.0, 62.0), Some((400.0, 62.0)));
        assert_eq!(glass_metrics(f64::NAN, 62.0), None);
        assert_eq!(glass_metrics(f64::INFINITY, 62.0), None);
        assert_eq!(glass_metrics(400.0, 0.0), None);
        assert_eq!(glass_metrics(400.0, -5.0), None);
        assert_eq!(glass_metrics(0.5, 0.5), None, "亚像素宿主不建玻璃");
    }

    /// 胶囊常量自洽:玻璃几何圆角小于外圆角(不与描边相切)。
    #[test]
    fn capsule_constants_are_consistent() {
        assert!(capsule::GLASS_RADIUS < capsule::OUTER_RADIUS);
        assert_eq!(capsule::OUTER_RADIUS, 32.0);
        assert!(capsule::FLOAT_MARGIN > 0.0);
    }

    /// tint 霜面必须半透明白(backdrop 采样可见)。
    #[test]
    fn tint_alpha_keeps_backdrop_visible() {
        assert!(TINT_ALPHA > 0 && TINT_ALPHA < 0xFF);
    }
}
