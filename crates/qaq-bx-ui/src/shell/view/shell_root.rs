//! 根树装配视图:shell-root Grid(keyed 稳定键,MPE 不销毁)与首页网格视图。

use super::super::*;

impl AppShell {
    /// 根层编排单源。全屏 / compact / desktop 三条返回路径统一从这里出根 View:
    /// 根 key 与覆盖层顺序集中在一处 —— keyed diff 按 key + kind 匹配,根 key
    /// 变化即整树销毁重建(全屏换源重载的根因)。后续要让全屏沿用播放层同键,
    /// 只需改这一个函数,不必再翻 570 行的 `view()`。
    pub(in crate::shell) fn shell_root_view(
        &self,
        root_key: &str,
        root_content: impl Into<View>,
        overlays: Vec<KeyedView>,
    ) -> View {
        let mut layers = Vec::with_capacity(1 + overlays.len());
        layers.push(keyed(root_key, root_content.into()));
        layers.extend(overlays);
        Border::new()
            .on_preview_key_down(self.global_key_handler.clone())
            .content(Grid::new().keyed_children(layers))
            .into()
    }

    /// 首页视图（Home 路由与播放页无播放项时的兜底共用）。
    pub(in crate::shell) fn home_view(&self, context: &ViewContext<Self>) -> View {
        pages::home::home_page(
            &self.home_source,
            &self.home_state,
            self.viewport_layout,
            &context.message(Message::HomeRefresh),
            &context.message(Message::HomeRetry),
        )
    }
}
