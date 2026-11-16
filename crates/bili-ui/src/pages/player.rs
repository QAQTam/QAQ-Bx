//! 播放页：双栏布局（左：视频面 + 传输控制 + 详情 + 底部评论区；右：相关推荐）
//! 与评论区下滚触发右下角小窗（YouTube 式页内画中画，保持 `VideoSurface` 挂载不重建）。

use std::sync::Arc;

use bili_video::engine::VideoEngine;
use windows_reactor::*;

use crate::data::VideoItem;
use crate::theme;
use crate::widgets::video_surface::{VideoSurface, VideoSurfaceInput};

const PLAYER_SLOT_HEIGHT: f64 = 420.0;
const RIGHT_RAIL_WIDTH: f64 = 360.0;
const PIP_SCROLL_THRESHOLD: f64 = 300.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommentSort {
    Hot,
    Latest,
}

#[derive(Clone)]
pub struct PlayerInput {
    pub item: VideoItem,
    pub playing: bool,
    pub position: u32,
    pub volume: u32,
    /// SwapChain 挂载尝试节拍（仅在未挂载时按 250ms 递增驱动 VideoSurface::try_mount）。
    pub mount_tick: u32,
    /// 展示总长(引擎 duration 优先,占位数据兜底)。
    pub duration_secs: u32,
    /// 引擎错误/媒体失败提示(None = 无)。
    pub notice: Option<String>,
    pub engine: Arc<VideoEngine>,
    pub on_toggle: Callback<()>,
    pub on_seek: Callback<f64>,
    pub on_volume: Callback<f64>,
}

impl PartialEq for PlayerInput {
    fn eq(&self, other: &Self) -> bool {
        self.item == other.item
            && self.playing == other.playing
            && self.position == other.position
            && self.volume == other.volume
            && self.mount_tick == other.mount_tick
            && self.duration_secs == other.duration_secs
            && self.notice == other.notice
            && Arc::ptr_eq(&self.engine, &other.engine)
            && self.on_toggle == other.on_toggle
            && self.on_seek == other.on_seek
            && self.on_volume == other.on_volume
    }
}

pub struct PlayerPage {
    scroll_y: f64,
    scroll_reset_gen: u64,
    pip_pinned: bool,
    comment_sort: CommentSort,
}

#[derive(Clone, PartialEq)]
pub enum Message {
    Scrolled(f64),
    TogglePip,
    BackToTop,
    SetCommentSort(CommentSort),
}

impl Component for PlayerPage {
    type Input = PlayerInput;
    type Message = Message;

    fn create(_input: &PlayerInput, _context: &ComponentContext<Self>) -> Self {
        Self {
            scroll_y: 0.0,
            scroll_reset_gen: 1,
            pip_pinned: false,
            comment_sort: CommentSort::Hot,
        }
    }

    fn update(&mut self, message: Message, _context: &ComponentContext<Self>) {
        match message {
            Message::Scrolled(y) => self.scroll_y = y.max(0.0),
            Message::TogglePip => self.pip_pinned = !self.pip_pinned,
            Message::BackToTop => {
                self.pip_pinned = false;
                self.scroll_y = 0.0;
                self.scroll_reset_gen = self.scroll_reset_gen.wrapping_add(1);
            }
            Message::SetCommentSort(sort) => self.comment_sort = sort,
        }
    }

    fn view(&self, input: &PlayerInput, context: &mut ViewContext<Self>) -> View {
        let is_pip = self.pip_pinned || self.scroll_y >= PIP_SCROLL_THRESHOLD;

        let surface: View = if let Some(notice) = &input.notice {
            Border::new()
                .corner_radius(8.0)
                .background(Brush::Solid(Color::rgb(16, 16, 16)))
                .content(
                    StackPanel::new()
                        .spacing(10.0)
                        .horizontal_alignment(HorizontalAlignment::Center)
                        .vertical_alignment(VerticalAlignment::Center)
                        .children((
                            TextBlock::new()
                                .text("视频通道异常")
                                .font_size(16.0)
                                .font_weight(FontWeight::SEMI_BOLD),
                            TextBlock::new()
                                .text(notice.clone())
                                .font_size(12.0)
                                .opacity(0.7)
                                .text_wrapping(TextWrapping::Wrap),
                        )),
                )
                .into()
        } else {
            View::component::<VideoSurface>(VideoSurfaceInput {
                engine: input.engine.clone(),
                tick: input.mount_tick,
            })
        };

        // 顶部主播放占位槽：未小窗时由上层稳定悬浮的 VideoSurface 覆盖；进入小窗后展示回到顶部提示。
        let top_player_slot: View = if is_pip {
            Border::new()
                .height(PLAYER_SLOT_HEIGHT)
                .corner_radius(8.0)
                .background(ThemeBrush::CardBackground)
                .border_brush(ThemeBrush::CardStroke)
                .border_thickness(1.0)
                .content(
                    StackPanel::new()
                        .spacing(10.0)
                        .horizontal_alignment(HorizontalAlignment::Center)
                        .vertical_alignment(VerticalAlignment::Center)
                        .children((
                            TextBlock::new()
                                .text("正在右下角小窗播放")
                                .font_size(15.0)
                                .font_weight(FontWeight::SEMI_BOLD),
                            Button::new()
                                .style(ButtonStyle::Accent)
                                .content("回到顶部主画面")
                                .on_click(context.message(Message::BackToTop)),
                        )),
                )
                .into()
        } else {
            Border::new().height(PLAYER_SLOT_HEIGHT).into()
        };

        let transport = StackPanel::new()
            .margin(Thickness::new(0.0, 10.0, 0.0, 4.0))
            .spacing(6.0)
            .children((
                Slider::new()
                    .minimum(0.0)
                    .maximum(f64::from(input.duration_secs.max(1)))
                    .value(input.position as f64)
                    .on_value_changed(input.on_seek.clone()),
                Grid::new()
                    .columns([GridLength::Auto, GridLength::Star(1.0), GridLength::Auto])
                    .children((
                        StackPanel::new()
                            .orientation(Orientation::Horizontal)
                            .spacing(8.0)
                            .vertical_alignment(VerticalAlignment::Center)
                            .children((
                                AppBarButton::new()
                                    .icon(Icon::symbol(if input.playing {
                                        Symbol::Pause
                                    } else {
                                        Symbol::Play
                                    }))
                                    .on_click(input.on_toggle.clone()),
                                TextBlock::new()
                                    .text(format!(
                                        "{} / {}",
                                        theme::fmt_duration(input.position),
                                        theme::fmt_duration(input.duration_secs)
                                    ))
                                    .font_size(12.0)
                                    .opacity(0.7)
                                    .vertical_alignment(VerticalAlignment::Center),
                            )),
                        StackPanel::new()
                            .grid_column(2)
                            .orientation(Orientation::Horizontal)
                            .spacing(8.0)
                            .vertical_alignment(VerticalAlignment::Center)
                            .children((
                                AppBarButton::new().icon(Icon::symbol(if input.volume == 0 {
                                    Symbol::Mute
                                } else {
                                    Symbol::Volume
                                })),
                                Slider::new()
                                    .width(110.0)
                                    .minimum(0.0)
                                    .maximum(100.0)
                                    .value(input.volume as f64)
                                    .on_value_changed(input.on_volume.clone())
                                    .vertical_alignment(VerticalAlignment::Center),
                                TextBlock::new()
                                    .text(format!("{}%", input.volume))
                                    .font_size(12.0)
                                    .opacity(0.6)
                                    .vertical_alignment(VerticalAlignment::Center),
                                Button::new()
                                    .style(if self.pip_pinned {
                                        ButtonStyle::Accent
                                    } else {
                                        ButtonStyle::Subtle
                                    })
                                    .content(if is_pip { "还原主窗" } else { "右下角小窗" })
                                    .on_click(context.message(Message::TogglePip)),
                            )),
                    )),
            ));

        let info_and_actions = StackPanel::new().spacing(12.0).children((
            TextBlock::new()
                .text(input.item.title.clone())
                .font_size(20.0)
                .font_weight(FontWeight::BOLD)
                .text_wrapping(TextWrapping::Wrap),
            Grid::new()
                .columns([GridLength::Star(1.0), GridLength::Auto])
                .children((
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(10.0)
                        .children((
                            PersonPicture::new()
                                .display_name(input.item.author.as_str())
                                .width(36.0)
                                .height(36.0),
                            StackPanel::new()
                                .spacing(2.0)
                                .vertical_alignment(VerticalAlignment::Center)
                                .children((
                                    TextBlock::new()
                                        .text(input.item.author.clone())
                                        .font_size(14.0)
                                        .font_weight(FontWeight::SEMI_BOLD),
                                    TextBlock::new()
                                        .text(format!(
                                            "播放 {} · 弹幕 {} · 时长 {}",
                                            theme::fmt_count(input.item.play_count),
                                            theme::fmt_count(input.item.danmaku_count),
                                            theme::fmt_duration(input.item.duration_secs)
                                        ))
                                        .font_size(12.0)
                                        .opacity(0.6),
                                )),
                        )),
                    StackPanel::new()
                        .grid_column(1)
                        .orientation(Orientation::Horizontal)
                        .spacing(8.0)
                        .vertical_alignment(VerticalAlignment::Center)
                        .children((
                            action_pill("点赞", "1.2万"),
                            action_pill("投币", "3,420"),
                            action_pill("收藏", "5,890"),
                            action_pill("分享", "转发"),
                        )),
                )),
            Expander::new()
                .header("简介")
                .is_expanded(false)
                .content(
                    TextBlock::new()
                        .text(format!(
                            "{}\n\nBV 号：{} · 相关推荐与评论区数据即将接入（当前为布局样式预览）。",
                            input.item.title, input.item.bvid
                        ))
                        .text_wrapping(TextWrapping::Wrap),
                ),
        ));

        let comments_section = comments_panel(self.comment_sort, context);

        let left_scroll_content = StackPanel::new()
            .spacing(14.0)
            .margin(Thickness::new(16.0, 16.0, 12.0, 24.0))
            .children((
                top_player_slot,
                View::from(transport),
                View::from(info_and_actions),
                comments_section,
            ));

        let left_scroll = Grid::new()
            .grid_column(0)
            .keyed_children([keyed(
                format!("player-left-scroll-{}", self.scroll_reset_gen),
                ScrollViewer::new()
                    .on_view_changed(context.callback(|ev: ScrollEventInfo| {
                        Message::Scrolled(ev.offset_y)
                    }))
                    .content(left_scroll_content),
            )]);

        let right_rail = ScrollViewer::new()
            .grid_column(1)
            .margin(Thickness::new(4.0, 16.0, 16.0, 16.0))
            .content(related_panel());

        let two_col_body = Grid::new()
            .columns([GridLength::Star(1.0), GridLength::Pixel(RIGHT_RAIL_WIDTH)])
            .children((View::from(left_scroll), View::from(right_rail)));

        // 稳定挂载 VideoSurface：通过对齐与尺寸切换在“顶部主位”和“右下角小窗”间过渡，不销毁 SwapChainPanel。
        let floating_surface = if is_pip {
            Border::new()
                .width(360.0)
                .height(228.0)
                .margin(Thickness::new(0.0, 0.0, 24.0, 24.0))
                .corner_radius(10.0)
                .background(ThemeBrush::CardBackground)
                .border_brush(ThemeBrush::CardStroke)
                .border_thickness(1.0)
                .horizontal_alignment(HorizontalAlignment::Right)
                .vertical_alignment(VerticalAlignment::Bottom)
                .content(
                    Grid::new()
                        .rows([GridLength::Star(1.0), GridLength::Auto])
                        .children((
                            View::from(Border::new().corner_radius(8.0).content(surface)),
                            Grid::new()
                                .grid_row(1)
                                .columns([GridLength::Auto, GridLength::Star(1.0), GridLength::Auto])
                                .margin(Thickness::xy(8.0, 4.0))
                                .children((
                                    Button::new()
                                        .style(ButtonStyle::Subtle)
                                        .content(if input.playing { "暂停" } else { "播放" })
                                        .on_click(input.on_toggle.clone()),
                                    TextBlock::new()
                                        .grid_column(1)
                                        .text(input.item.title.clone())
                                        .font_size(11.0)
                                        .max_lines(1)
                                        .opacity(0.75)
                                        .margin(Thickness::xy(6.0, 0.0))
                                        .vertical_alignment(VerticalAlignment::Center),
                                    Button::new()
                                        .grid_column(2)
                                        .style(ButtonStyle::Subtle)
                                        .content("回主窗")
                                        .on_click(context.message(Message::BackToTop)),
                                )),
                        )),
                )
        } else {
            Border::new()
                .height(PLAYER_SLOT_HEIGHT)
                .margin(Thickness::new(16.0, 16.0, RIGHT_RAIL_WIDTH + 12.0, 0.0))
                .corner_radius(8.0)
                .horizontal_alignment(HorizontalAlignment::Stretch)
                .vertical_alignment(VerticalAlignment::Top)
                .content(surface)
        };

        Grid::new()
            .keyed_children([
                keyed("player-two-col", two_col_body),
                keyed("player-surface-host", floating_surface),
            ])
            .into()
    }
}

fn action_pill(label: &str, count: &str) -> View {
    Button::new()
        .style(ButtonStyle::Subtle)
        .content(
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(6.0)
                .children((
                    TextBlock::new()
                        .text(label)
                        .font_size(12.0)
                        .font_weight(FontWeight::SEMI_BOLD),
                    TextBlock::new()
                        .text(count)
                        .font_size(11.0)
                        .opacity(0.65),
                )),
        )
        .into()
}

fn comments_panel(sort: CommentSort, context: &mut ViewContext<PlayerPage>) -> View {
    let header = Grid::new()
        .columns([GridLength::Star(1.0), GridLength::Auto])
        .children((
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(10.0)
                .vertical_alignment(VerticalAlignment::Center)
                .children((
                    TextBlock::new()
                        .text("评论")
                        .font_size(17.0)
                        .font_weight(FontWeight::BOLD),
                    TextBlock::new()
                        .text("1,284")
                        .font_size(13.0)
                        .opacity(0.6)
                        .vertical_alignment(VerticalAlignment::Center),
                )),
            StackPanel::new()
                .grid_column(1)
                .orientation(Orientation::Horizontal)
                .spacing(6.0)
                .children((
                    Button::new()
                        .style(if sort == CommentSort::Hot {
                            ButtonStyle::Accent
                        } else {
                            ButtonStyle::Subtle
                        })
                        .content("按热度")
                        .on_click(context.message(Message::SetCommentSort(CommentSort::Hot))),
                    Button::new()
                        .style(if sort == CommentSort::Latest {
                            ButtonStyle::Accent
                        } else {
                            ButtonStyle::Subtle
                        })
                        .content("按时间")
                        .on_click(context.message(Message::SetCommentSort(CommentSort::Latest))),
                )),
        ));

    let composer = Grid::new()
        .columns([GridLength::Auto, GridLength::Star(1.0), GridLength::Auto])
        .children((
            PersonPicture::new()
                .display_name("我")
                .width(34.0)
                .height(34.0)
                .vertical_alignment(VerticalAlignment::Center),
            TextBox::new()
                .grid_column(1)
                .margin(Thickness::xy(10.0, 0.0))
                .placeholder_text("发一条友善的评论…（接口待接入）"),
            Button::new()
                .grid_column(2)
                .style(ButtonStyle::Accent)
                .content("发布"),
        ));

    let sample_comments = [
        (
            "极客阿宅",
            "LV6",
            "2小时前 · 上海",
            "这个 WinUI 3 + Rust + mpv 的播放页质感太舒服了，向下翻评论自动缩到右下角小窗简直完美。",
            "842",
            Some(("UP主 · 回复", "感谢支持！接下来把真实评论和相关推荐接口接上就能直接刷了。")),
        ),
        (
            "林间听风",
            "LV5",
            "5小时前 · 广东",
            "建议楼中楼支持展开，顺便看下分 P 列表能不能也放在右侧推荐上方切换。",
            "216",
            None,
        ),
        (
            "手柄玩家小王",
            "LV4",
            "昨天 · 四川",
            "ROG Ally 刚测完，手柄四色指引胶囊和右摇杆滚动丝滑得很！",
            "129",
            Some(("路人甲", "真的假的，桌面播放器居然原生做 Xbox 手柄支持？")),
        ),
        (
            "Fluent 控",
            "LV6",
            "2天前 · 北京",
            "浅色模式下右下角时长胶囊的反色终于看清了，Mica 背景通透感拉满。",
            "94",
            None,
        ),
    ];

    let mut rows: Vec<View> = vec![View::from(header), View::from(composer)];
    for (author, level, meta, body, likes, reply_preview) in sample_comments {
        rows.push(comment_card(author, level, meta, body, likes, reply_preview));
    }

    Border::new()
        .margin(Thickness::new(0.0, 8.0, 0.0, 0.0))
        .padding(Thickness::xy(16.0, 16.0))
        .corner_radius(8.0)
        .background(ThemeBrush::CardBackground)
        .border_brush(ThemeBrush::CardStroke)
        .border_thickness(1.0)
        .content(StackPanel::new().spacing(14.0).children(rows))
        .into()
}

fn comment_card(
    author: &str,
    level: &str,
    meta: &str,
    body: &str,
    likes: &str,
    reply_preview: Option<(&str, &str)>,
) -> View {
    let mut content_col = vec![
        StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(8.0)
            .children((
                TextBlock::new()
                    .text(author)
                    .font_size(13.0)
                    .font_weight(FontWeight::SEMI_BOLD),
                Border::new()
                    .corner_radius(3.0)
                    .padding(Thickness::xy(4.0, 1.0))
                    .background(ThemeBrush::SolidBackground)
                    .border_brush(ThemeBrush::CardStroke)
                    .border_thickness(1.0)
                    .content(TextBlock::new().text(level).font_size(10.0).opacity(0.8)),
                TextBlock::new()
                    .text(meta)
                    .font_size(11.0)
                    .opacity(0.5)
                    .vertical_alignment(VerticalAlignment::Center),
            ))
            .into(),
        TextBlock::new()
            .text(body)
            .font_size(13.0)
            .text_wrapping(TextWrapping::Wrap)
            .into(),
        StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(16.0)
            .children((
                TextBlock::new()
                    .text(format!("👍 {likes}"))
                    .font_size(11.0)
                    .opacity(0.65),
                TextBlock::new()
                    .text("回复")
                    .font_size(11.0)
                    .opacity(0.65),
            ))
            .into(),
    ];

    if let Some((replier, reply_text)) = reply_preview {
        content_col.push(
            Border::new()
                .margin(Thickness::new(0.0, 4.0, 0.0, 0.0))
                .padding(Thickness::xy(10.0, 8.0))
                .corner_radius(6.0)
                .background(ThemeBrush::SolidBackground)
                .border_brush(ThemeBrush::CardStroke)
                .border_thickness(1.0)
                .content(
                    TextBlock::new()
                        .text(format!("{replier}：{reply_text}"))
                        .font_size(12.0)
                        .opacity(0.85)
                        .text_wrapping(TextWrapping::Wrap),
                )
                .into(),
        );
    }

    Border::new()
        .padding(Thickness::new(0.0, 10.0, 0.0, 6.0))
        .content(
            Grid::new()
                .columns([GridLength::Auto, GridLength::Star(1.0)])
                .children((
                    PersonPicture::new()
                        .display_name(author)
                        .width(34.0)
                        .height(34.0)
                        .vertical_alignment(VerticalAlignment::Top),
                    StackPanel::new()
                        .grid_column(1)
                        .margin(Thickness::new(12.0, 0.0, 0.0, 0.0))
                        .spacing(6.0)
                        .children(content_col),
                )),
        )
        .into()
}

fn related_panel() -> View {
    let samples = [
        ("Rust 写一个原生 WinUI 3 B站客户端有多爽？", "技术宅频道", "14.2万", "08:42"),
        ("Windows App SDK 2.6 + Direct3D11 零拷贝渲染实战", "图形学笔记", "6.8万", "15:19"),
        ("2026 掌机桌面播放器横向对比：手柄体验谁最强", "数码实验室", "23.5万", "11:05"),
        ("mpv 渲染 API + Direct2D 弹幕合批优化全解析", "音视频工坊", "4.1万", "19:30"),
        ("如何用 XInput 给桌面软件做出 Xbox 主机级交互", "独立开发者", "9.3万", "07:14"),
        ("WinUI 3 Gallery 新控件精选：SelectorBar 与画中画", "微软生态观察", "5.6万", "10:28"),
    ];

    let mut cards: Vec<View> = vec![
        TextBlock::new()
            .text("接下来播放 · 相关推荐")
            .font_size(15.0)
            .font_weight(FontWeight::BOLD)
            .into(),
    ];

    for (title, author, views, duration) in samples {
        cards.push(related_card(title, author, views, duration));
    }

    StackPanel::new().spacing(10.0).children(cards).into()
}

fn related_card(title: &str, author: &str, views: &str, duration: &str) -> View {
    Border::new()
        .padding(Thickness::xy(8.0, 8.0))
        .corner_radius(8.0)
        .background(ThemeBrush::CardBackground)
        .border_brush(ThemeBrush::CardStroke)
        .border_thickness(1.0)
        .content(
            Grid::new()
                .columns([GridLength::Pixel(132.0), GridLength::Star(1.0)])
                .children((
                    Grid::new().height(74.0).children((
                        Border::new()
                            .corner_radius(6.0)
                            .background(ThemeBrush::SolidBackground)
                            .content(
                                TextBlock::new()
                                    .text("封面")
                                    .font_size(11.0)
                                    .opacity(0.35)
                                    .horizontal_alignment(HorizontalAlignment::Center)
                                    .vertical_alignment(VerticalAlignment::Center),
                            ),
                        Border::new()
                            .background(ThemeBrush::SolidBackground)
                            .border_brush(ThemeBrush::CardStroke)
                            .border_thickness(1.0)
                            .corner_radius(4.0)
                            .padding(Thickness::xy(5.0, 1.0))
                            .margin(Thickness::new(0.0, 0.0, 5.0, 5.0))
                            .horizontal_alignment(HorizontalAlignment::Right)
                            .vertical_alignment(VerticalAlignment::Bottom)
                            .content(
                                TextBlock::new()
                                    .text(duration)
                                    .foreground(ThemeBrush::PrimaryText)
                                    .font_size(10.0),
                            ),
                    )),
                    StackPanel::new()
                        .grid_column(1)
                        .margin(Thickness::new(10.0, 0.0, 0.0, 0.0))
                        .spacing(4.0)
                        .children((
                            TextBlock::new()
                                .text(title)
                                .font_size(13.0)
                                .font_weight(FontWeight::SEMI_BOLD)
                                .text_wrapping(TextWrapping::Wrap)
                                .max_lines(2),
                            TextBlock::new()
                                .text(author)
                                .font_size(11.0)
                                .opacity(0.6),
                            TextBlock::new()
                                .text(format!("▶ {views}"))
                                .font_size(11.0)
                                .opacity(0.55),
                        )),
                )),
        )
        .into()
}
