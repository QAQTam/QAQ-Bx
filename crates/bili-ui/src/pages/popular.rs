//! 热门页：复用卡片网格，按播放量排序的占位数据。

use windows_reactor::*;
use crate::data;
use crate::theme;
use crate::widgets::video_card;

pub fn popular_page(on_open: Callback<String>) -> View {
    let videos = data::popular();
    ScrollViewer::new()
        .content(
            Border::new()
                .padding(Thickness::new(36.0, 24.0, 36.0, 36.0))
                .content(
                    StackPanel::new().spacing(24.0).children((
                        theme::page_header(
                            "热门",
                            &format!("{} 个视频 · 按播放量排序（占位数据）", videos.len()),
                        ),
                        video_card::video_grid(&videos, &on_open),
                    )),
                ),
        )
        .into()
}
