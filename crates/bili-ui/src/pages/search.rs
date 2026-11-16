//! 搜索页。

use windows_reactor::*;

pub fn search_page() -> View {
    ScrollViewer::new()
        .content(
            Border::new()
                .padding(Thickness::new(36.0, 24.0, 36.0, 36.0))
                .content(
                    StackPanel::new().spacing(20.0).children((
                        TextBlock::new()
                            .text("搜索")
                            .font_size(28.0)
                            .font_weight(FontWeight::BOLD),
                        TextBox::new()
                            .placeholder_text("搜索视频、UP 主")
                            .width(480.0)
                            .horizontal_alignment(HorizontalAlignment::Left),
                        TextBlock::new()
                            .text("动画 · 游戏 · 科技 · 音乐 · 知识 · 影视 · 美食")
                            .font_size(12.0)
                            .opacity(0.6),
                    )),
                ),
        )
        .into()
}
