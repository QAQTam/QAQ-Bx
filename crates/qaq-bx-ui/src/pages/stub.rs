//! 空态页面：简洁标题 + 克制提示。

use windows_reactor::*;

use crate::layout::ViewportLayout;

pub fn stub_page(title: &str, empty_text: &str, layout: ViewportLayout) -> View {
    let margin_x = layout.page_margin_x();
    let top = if layout.is_mobile() { 12.0 } else { 24.0 };
    Grid::new()
        .rows([GridLength::Auto, GridLength::Star(1.0)])
        .children((
            Border::new()
                .padding(Thickness::new(margin_x, top, margin_x, 12.0))
                .grid_row(0)
                .content(
                    TextBlock::new()
                        .text(title)
                        .font_size(layout.page_title_size())
                        .font_weight(FontWeight::BOLD),
                ),
            TextBlock::new()
                .text(empty_text)
                .font_size(13.0)
                .opacity(0.55)
                .horizontal_alignment(HorizontalAlignment::Center)
                .vertical_alignment(VerticalAlignment::Center)
                .grid_row(1),
        ))
        .into()
}
