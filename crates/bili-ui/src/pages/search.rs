//! 搜索页：标题栏搜索框的数据面（骨架阶段仅布局与说明）。

use windows_reactor::*;
use crate::theme;

pub fn search_page() -> View {
    ScrollViewer::new()
        .content(
            Border::new()
                .padding(Thickness::new(36.0, 24.0, 36.0, 36.0))
                .content(
                    StackPanel::new().spacing(24.0).children((
                        theme::page_header("搜索", "关键词搜索与建议词 —— WBI 签名通道待接入"),
                        TextBox::new().placeholder_text("输入关键词，回车搜索（待接入）").width(520.0),
                        TextBlock::new()
                            .text("热门搜索：动画 · 游戏 · 科技 · 音乐 · 知识 · 影视 · 美食")
                            .font_size(12.0)
                            .opacity(0.6),
                        InfoBar::new()
                            .title("搜索结果")
                            .message(
                                "all/v2 搜索 API 已在 research 阶段验证（复刻 WBI 签名可用，code=0），\
                                 UI 数据源待接入。",
                            )
                            .severity(InfoBarSeverity::Informational)
                            .is_open(true)
                            .is_closable(false),
                    )),
                ),
        )
        .into()
}
