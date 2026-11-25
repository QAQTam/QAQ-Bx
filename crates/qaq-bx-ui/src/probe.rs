//! xamlroot 崩溃探针(`--probe-xamlroot`,feature `probe`,诊断专用)。
//!
//! 背景:`Microsoft.UI.Xaml.Controls.dll +0x359595` 崩溃点位于框架内部
//! `consume_IUIElement<D>::XamlRoot()`——QI 结果未查 HRESULT 即解引用。
//! 本探针经 reactor `debug_raw`(test/诊断专用 imperative)拿到真实
//! TitleBar 的裸 COM 指针,分阶段实测什么状态让该 consume 的 QI 失败:
//!
//! 1. RAW     经 debug_raw 拿到 TitleBar 的 IUnknown;
//! 2. OK      连接态安全探测 XamlRoot()(基线,预期 Ok 非空);
//! 3. DETACH  把 TitleBar 从可视树移除(绕过 reactor,诊断构建专用);
//! 4. DISC    断开态安全探测 XamlRoot()(关键数据:QI 是否失败);
//! 5. TRIG    对断开态 TitleBar 设置 Title 属性 → 框架内部
//!            OnPropertyChanged → UpdateDragRegion → GetAppWindowId
//!            → 无检查 consume —— 若 QI 失败则 AV(即 demo 复现点,
//!            WER LocalDumps 会自动留 dump);
//! 6. THREAD  跨线程调用 XamlRoot()(验证 context station 行为)。
//!
//! 全程写 `data/xamlroot-probe.log`。探针状态机全部在 UI 线程
//! (thread_local),仅 THREAD 阶段显式跨线程。只在 probe feature 下编译。

#![allow(
    non_snake_case,
    non_camel_case_types,
    non_upper_case_globals,
    dead_code,
    clippy::all
)]

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};

use windows_core::{Interface, IUnknown};
use windows_reactor::{Button, ElementRef, TitleBar as ReactorTitleBar};

include!(concat!(env!("OUT_DIR"), "/winrt_bindings.rs"));

pub(crate) static PROBE_ENABLED: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// 焦点锚点 + 元素引用:0 尺寸 Button,随 TitleBar left_header 进入可视树。
    pub(crate) static PROBE_FOCUS: ElementRef<Button> = ElementRef::default();
}

const STAGE_INSTALL: u32 = 0;
const STAGE_WAIT_RAW: u32 = 1;
const STAGE_CONNECTED: u32 = 2;
const STAGE_DETACH: u32 = 3;
const STAGE_DISCONNECTED: u32 = 4;
const STAGE_TRIGGER: u32 = 5;
const STAGE_THREAD: u32 = 6;
const STAGE_DONE: u32 = 7;

struct ProbeState {
    stage: u32,
    tick: u32,
    raw: Option<usize>,
    title_bar: Option<TitleBar>,
    title_bar_ui: Option<UIElement>,
    timer: Option<DispatcherQueueTimer>,
    timer_revoker: Option<windows_core::EventRevoker>,
}

thread_local! {
    static STATE: RefCell<ProbeState> = RefCell::new(ProbeState {
        stage: STAGE_INSTALL,
        tick: 0,
        raw: None,
        title_bar: None,
        title_bar_ui: None,
        timer: None,
        timer_revoker: None,
    });
}

fn log(message: impl AsRef<str>) {
    let line = format!("[probe] {}\n", message.as_ref());
    let path = qaq_bx_core::resolve_data_dir().join("xamlroot-probe.log");
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        use std::io::Write;
        let _ = file.write_all(line.as_bytes());
    }
    print!("{line}");
}

/// 把探针锚点 Button 挂进 TitleBar left_header(0 尺寸,不可见,仅供诊断)。
pub(crate) fn attach_focus_button(bar: ReactorTitleBar) -> ReactorTitleBar {
    let focus = PROBE_FOCUS.with(|cell| cell.clone());
    bar.left_header(Button::new().width(0.0).height(0.0).element_ref(&focus))
}

/// 由 shell 的 update 循环驱动(首个 message 时安装一次 timer)。
pub(crate) fn pump() {
    if !PROBE_ENABLED.load(Ordering::Relaxed) {
        return;
    }
    STATE.with_borrow_mut(|state| {
        if state.timer.is_some() {
            return;
        }
        log("probe enabled, installing stage timer");
        let installed = (|| {
            let queue = DispatcherQueue::GetForCurrentThread().ok()?;
            let timer = queue.CreateTimer().ok()?;
            let interval =
                windows_time::TimeSpan::try_from(std::time::Duration::from_millis(500)).ok()?;
            timer.SetInterval(interval).ok()?;
            let revoker = timer
                .Tick(move |_, _| {
                    tick();
                })
                .ok()?;
            Some((timer, revoker))
        })();
        match installed {
            Some((timer, revoker)) => {
                let _ = timer.Start();
                state.timer = Some(timer);
                state.timer_revoker = Some(revoker);
            }
            None => log("安装 stage timer 失败"),
        }
    });
}

fn tick() {
    STATE.with_borrow_mut(|state| {
        state.tick += 1;
        let tick_no = state.tick;
        match state.stage {
            STAGE_INSTALL => {
                // 经 debug_raw 拿 TitleBar 原生 IUnknown(命令队列在 UI 线程投递)。
                let requested = PROBE_FOCUS.with(|cell| cell.debug_raw(|raw| {
                    STATE.with_borrow_mut(|state| {
                        if let Some(raw) = raw {
                            log("debug_raw 交付 TitleBar 原生指针");
                            state.raw = Some(raw);
                        } else {
                            log("debug_raw 返回 None(元素未发布)");
                        }
                        if state.stage == STAGE_WAIT_RAW {
                            state.stage = STAGE_CONNECTED;
                        }
                    });
                }));
                state.stage = STAGE_WAIT_RAW;
                let _ = requested;
            }
            STAGE_WAIT_RAW => {
                if tick_no > 20 {
                    log("等待 debug_raw 交付超时(10s),终止探针");
                    state.stage = STAGE_DONE;
                }
            }
            STAGE_CONNECTED => {
                let Some(raw) = state.raw else {
                    state.stage = STAGE_DONE;
                    return;
                };
                // 锚点 Button 的借用指针 → 引用计数安全的 IUIElement。
                let abi = raw as *mut core::ffi::c_void;
                let Some(anchor) = (unsafe { UIElement::from_raw_borrowed(&abi) })
                    .and_then(|object| object.cast::<UIElement>().ok())
                else {
                    log("raw → IUIElement 转换失败,终止探针");
                    state.stage = STAGE_DONE;
                    return;
                };
                // 自锚点向上找 TitleBar 祖先(COM cast 即 QI)。
                let mut title_bar: Option<TitleBar> = None;
                let mut title_bar_ui: Option<UIElement> = None;
                let mut current = anchor.clone();
                for _ in 0..16 {
                    if let Ok(found) = current.cast::<TitleBar>() {
                        title_bar = Some(found);
                        title_bar_ui = Some(current.clone());
                        break;
                    }
                    let next = current
                        .cast::<DependencyObject>()
                        .ok()
                        .and_then(|object| VisualTreeHelper::GetParent(&object).ok())
                        .and_then(|parent| parent.cast::<UIElement>().ok());
                    match next {
                        Some(parent) => current = parent,
                        None => break,
                    }
                }
                let Some(found_ui) = title_bar_ui else {
                    log("锚点向上未找到 TitleBar 祖先,终止探针");
                    state.stage = STAGE_DONE;
                    return;
                };
                log("定位到 TitleBar 祖先");
                state.title_bar = title_bar;
                state.title_bar_ui = Some(found_ui);
                let Some(ui) = state.title_bar_ui.as_ref() else {
                    state.stage = STAGE_DONE;
                    return;
                };
                match ui.XamlRoot() {
                    Ok(_) => log("连接态 XamlRoot()=Ok(非空)"),
                    Err(error) => log(format!("连接态 XamlRoot()=Err({error:?})")),
                }
                state.stage = STAGE_DETACH;
            }
            STAGE_DETACH => {
                let Some(ui) = state.title_bar_ui.as_ref() else {
                    state.stage = STAGE_DONE;
                    return;
                };
                let parent = ui
                    .cast::<DependencyObject>()
                    .ok()
                    .and_then(|object| VisualTreeHelper::GetParent(&object).ok())
                    .and_then(|parent| parent.cast::<Panel>().ok());
                let Some(panel) = parent else {
                    log("TitleBar 父节点不是 Panel,终止探针");
                    state.stage = STAGE_DONE;
                    return;
                };
                let removed = (panel.Children())
                    .ok()
                    .and_then(|children| {
                        let size = children.Size().ok()?;
                        let self_id = ui.cast::<IUnknown>().ok()?.as_raw() as usize;
                        for index in 0..size {
                            let child = children.GetAt(index).ok()?;
                            let child_id = child.cast::<IUnknown>().ok()?.as_raw() as usize;
                            if child_id == self_id {
                                children.RemoveAt(index).ok()?;
                                return Some(());
                            }
                        }
                        None
                    })
                    .is_some();
                log(if removed {
                    "已把 TitleBar 从可视树移除(绕过 reactor,诊断构建专用)"
                } else {
                    "未在父集合中找到 TitleBar(移除失败)"
                });
                state.stage = STAGE_DISCONNECTED;
            }
            STAGE_DISCONNECTED => {
                let Some(ui) = state.title_bar_ui.as_ref() else {
                    state.stage = STAGE_DONE;
                    return;
                };
                // 关键数据:断开态下安全 consume 的 QI 行为。
                match unsafe { ui.XamlRoot() } {
                    Ok(_) => log(
                        "断开态 XamlRoot()=Ok(非空) ← QI 未失败,该状态不构成触发条件",
                    ),
                    Err(error) => log(format!(
                        "断开态 XamlRoot()=Err({error:?}) ← QI 失败!下一步 TRIG 应触发崩溃"
                    )),
                }
                state.stage = STAGE_TRIGGER;
            }
            STAGE_TRIGGER => {
                let Some(title_bar) = state.title_bar.as_ref() else {
                    state.stage = STAGE_DONE;
                    return;
                };
                // 框架内部无检查路径:OnPropertyChanged → UpdateDragRegion
                // → GetAppWindowId → consume_IUIElement::XamlRoot()。
                log("对断开态 TitleBar 设置 Title(触发框架内部 UpdateDragRegion)…");
                let _ = title_bar.SetTitle("xamlroot-probe crash attempt");
                log("属性设置完成且未崩溃(该状态下 QI 未失败)");
                state.stage = STAGE_THREAD;
            }
            STAGE_THREAD => {
                let Some(ui) = state.title_bar_ui.as_ref() else {
                    state.stage = STAGE_DONE;
                    return;
                };
                let cross = ui.clone();
                log("跨线程调用 XamlRoot()(验证 context station)…");
                let handle = std::thread::Builder::new()
                    .name("xamlroot-probe-thread".into())
                    .spawn(move || match cross.XamlRoot() {
                        Ok(_) => log("跨线程 XamlRoot()=Ok(意外!context station 未拦截)"),
                        Err(error) => log(format!("跨线程 XamlRoot()=Err({error:?})")),
                    })
                    .ok();
                if let Some(handle) = handle {
                    let _ = handle.join();
                }
                state.stage = STAGE_DONE;
            }
            _ => {
                log("探针完成;若中途崩溃,对应的 dump 已由 WER LocalDumps 捕获");
                state.stage = STAGE_DONE;
                // 无人值守:跑完即退(诊断构建专用)。
                std::process::exit(0);
            }
        }
    });
}
