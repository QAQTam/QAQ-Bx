//! 登录态 / 数据源未接入页面的通用空态。

use windows_reactor::*;
use crate::theme;

pub fn stub_page(title: &str, subtitle: &str, notice_title: &str, notice: &str) -> View {
    ScrollViewer::new()
        .content(
            Border::new()
                .padding(Thickness::new(36.0, 24.0, 36.0, 36.0))
                .content(
                    StackPanel::new().spacing(24.0).children((
                        theme::page_header(title, subtitle),
                        InfoBar::new()
                            .title(notice_title)
                            .message(notice)
                            .severity(InfoBarSeverity::Informational)
                            .is_open(true)
                            .is_closable(false),
                        Border::new()
                            .corner_radius(8.0)
                            .background(ThemeBrush::CardBackground)
                            .border_brush(ThemeBrush::CardStroke)
                            .border_thickness(1.0)
                            .padding(24.0)
                            .content(
                                StackPanel::new().spacing(8.0).children((
                                    TextBlock::new()
                                        .text("本页面在骨架阶段保留空态")
                                        .font_size(14.0)
                                        .font_weight(FontWeight::SEMI_BOLD),
                                    TextBlock::new()
                                        .text(
                                            "对应 feed API 与 bili-core 数据通道接入后自动启用；\
                                             布局与卡片组件已按最终形态就位。",
                                        )
                                        .font_size(12.0)
                                        .opacity(0.6)
                                        .text_wrapping(TextWrapping::Wrap),
                                )),
                            ),
                    )),
                ),
        )
        .into()
}
