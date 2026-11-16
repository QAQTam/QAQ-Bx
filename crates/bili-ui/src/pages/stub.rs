//! 空态页面：简洁标题 + 克制提示。

use windows_reactor::*;

pub fn stub_page(title: &str, empty_text: &str) -> View {
    Grid::new()
        .rows([GridLength::Auto, GridLength::Star(1.0)])
        .children((
            Border::new()
                .padding(Thickness::new(36.0, 24.0, 36.0, 12.0))
                .grid_row(0)
                .content(
                    TextBlock::new()
                        .text(title)
                        .font_size(28.0)
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
