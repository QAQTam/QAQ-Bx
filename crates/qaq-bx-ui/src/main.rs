//! QAQ-Bx 前端入口：WinUI3 / Fluent 应用壳。
//! 设计与布局对照 `docs/ui-design.md`（Windows 媒体播放器母版）。
// selftest 模式保留控制台输出报告;生产构建仍为无窗口子系统。
#![cfg_attr(not(feature = "selftest"), windows_subsystem = "windows")]

mod auth;
mod capture_gate;
mod codec;
mod data;
mod demo;
mod feed_pager;
mod gamepad;
mod image_cache;
mod layout;
mod notifications;
mod pages;
#[cfg(feature = "probe")]
mod probe;
mod router;
mod runtime;
#[cfg(feature = "selftest")]
mod selftest;
mod shell;
mod smtc;
mod theme;
mod trace;
mod widgets;
mod winrt_ext;

fn install_panic_hook() {
    use std::io::Write;
    use std::sync::atomic::{AtomicBool, Ordering};
    static FIRST_PANIC_RECORDED: AtomicBool = AtomicBool::new(false);

    std::panic::set_hook(Box::new(|info| {
        let bt = std::backtrace::Backtrace::force_capture();
        let detail = format!("QAQ-Bx 发生未捕获异常：\n{info}\n\nBacktrace:\n{bt}");
        let log_path = qaq_bx_core::resolve_data_dir().join("crash.log");
        if let Some(parent) = log_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let is_first = !FIRST_PANIC_RECORDED.swap(true, Ordering::SeqCst);
        if is_first {
            let _ = std::fs::write(&log_path, &detail);
            let summary = format!("{info}\n\n完整堆栈已写入：\n{}", log_path.display());
            show_error_box("QAQ-Bx 崩溃提示", &summary);
        } else if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
        {
            let _ = write!(f, "\n\n=== 级联异常（保留上方首次堆栈） ===\n{detail}");
        }
    }));
}

fn show_error_box(title: &str, body: &str) {
    use windows::Win32::winuser::{MB_ICONERROR, MB_OK, MessageBoxW};
    use windows_core::HSTRING;

    let text = HSTRING::from(body);
    let caption = HSTRING::from(title);
    unsafe {
        let _ = MessageBoxW(None, &text, &caption, (MB_ICONERROR | MB_OK) as u32);
    }
}

fn main() {
    #[cfg(feature = "selftest")]
    if let Some(scenario) = selftest::scenario_from_args() {
        selftest::run(&scenario);
        return;
    }
    install_panic_hook();
    #[cfg(feature = "probe")]
    if std::env::args().any(|arg| arg == "--probe-xamlroot") {
        probe::PROBE_ENABLED.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    trace::note(format!(
        "app start pid={} args={:?}",
        std::process::id(),
        std::env::args().collect::<Vec<_>>()
    ));
    // WER LocalDumps 链路自检:数据目录存在哨兵文件时主动 AV(见模块注释)。
    if capture_gate::requested(&qaq_bx_core::resolve_data_dir()) {
        capture_gate::fire();
    }
    let _identity = notifications::init_process_identity();
    if let Err(err) =
        windows_reactor::App::run_component::<shell::AppShell>(shell::ShellInput::default())
    {
        let detail = format!("WinUI 启动失败：\n{err:?}");
        let log_path = qaq_bx_core::resolve_data_dir().join("crash.log");
        if let Some(parent) = log_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&log_path, &detail);
        show_error_box(
            "QAQ-Bx 启动失败",
            &format!("{detail}\n\n日志路径：{}", log_path.display()),
        );
        std::process::exit(1);
    }
}
