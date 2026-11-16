//! biliplay 前端入口：WinUI3 / Fluent 应用壳。
//! 设计与布局对照 `docs/ui-design.md`（Windows 媒体播放器母版）。

mod auth;
mod data;
mod feed_pager;
mod image_cache;
mod pages;
mod router;
mod runtime;
mod shell;
mod smtc;
mod theme;
mod widgets;

fn main() {
    windows_reactor::App::run_component::<shell::AppShell>(()).unwrap();
}
