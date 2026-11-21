//! 悬停预览卡覆盖层(#7):全局唯一 PeekCard 的视图装配,z 序在玻璃宿主之后、
//! shell-root 之前;无背景不拦截命中。

use super::super::*;

impl AppShell {
    /// #7 根布局覆盖层:全局唯一 PeekCard,根 Grid 末子元素(z 序在液态玻璃宿主
    /// child visual 之后,v0.4 G4/G7 教训);Canvas 无背景,空白区不拦截命中。
    pub(in crate::shell) fn peek_overlay_layer(&self, context: &mut ViewContext<Self>) -> Option<KeyedView> {
        if !self.peek_allowed() {
            return None;
        }
        let (bvid, anchor) = self.peek.mounted_content()?;
        // 槽位现值优先(封面回填/驱逐都发生在 home_slots 的 Rc 上,home_index
        // 是加载时副本);越界时回落索引。
        let slot_item = self
            .home_slot_by_bvid
            .get(&bvid)
            .and_then(|idx| self.home_slots.get(*idx))
            .and_then(|slot| match slot {
                HomeSlot::Video { item, .. } => Some(Rc::clone(item)),
                HomeSlot::Skeleton { .. } => None,
            });
        // 预览是低频弹出路径:直接拷贝槽位现值(封面随驱逐/回填的最新状态),
        // 槽位越界时回落加载副本。
        let item = match slot_item
            .map(|item| (*item).clone())
            .or_else(|| self.home_index.get(&bvid).cloned())
        {
            Some(item) => item,
            None => return None,
        };
        let desc = self.peek_desc.get(&bvid).cloned().flatten();
        let notice = self.peek_notice.clone();
        let reduced = self.settings.reduce_animations;
        let scale = if reduced { 1.0 } else { peek::PEEK_POP_SCALE };
        let placement = peek::peek_placement(
            anchor,
            (
                self.viewport_layout.window_width,
                self.viewport_layout.window_height,
            ),
            (peek::PEEK_WIDTH, peek::PEEK_HEIGHT),
            scale,
            peek::PEEK_EDGE_MARGIN,
            peek::PEEK_GAP,
        );
        let render = peek::peek_render(!self.peek.hiding(), !self.peek_anim_pending, reduced);
        let on_enter = context.callback(|_: PointerEventInfo| Message::PeekPointerEntered);
        let on_exit = context.callback(|_: PointerEventInfo| Message::PeekPointerExited);
        let on_play = context.callback(Message::PeekPlay);
        let on_later = context.callback(Message::PeekWatchLater);
        let on_close = context.message(Message::PeekClose);
        let card = peek::peek_card_view(
            &item,
            desc.as_deref(),
            notice.as_deref(),
            placement,
            render,
            &bvid,
            on_enter,
            on_exit,
            &on_play,
            &on_later,
            &on_close,
        );
        Some(keyed(
            "peek-overlay",
            Canvas::new().children([card]),
        ))
    }
}
