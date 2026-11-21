//! 导航菜单项构建(NAV_ROUTES 逐项 KeyedView;desktop/mobile 两侧共用)。

use super::super::*;

pub(in crate::shell) fn nav_item(tag: &str, label: &str, icon: Option<Symbol>, selected: bool) -> KeyedView {
    let mut item = NavigationViewItem::new()
        .tag(tag)
        .is_selected(selected)
        .selects_on_invoked(true)
        .content(label);
    if let Some(symbol) = icon {
        item = item.icon(symbol);
    }
    KeyedView::new(tag.to_string(), item)
}
