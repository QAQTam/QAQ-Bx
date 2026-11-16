//! 登录页与个人中心入口分发：未登录态展示简洁扫码流程，登录态交由 `pages::account`。

use windows_reactor::*;

use crate::auth::{AuthState, LoginFlowState};
use crate::pages::account::{account_page, AccountCallbacks, AccountViewData};

pub fn login_page(
    auth: &AuthState,
    flow: &LoginFlowState,
    account_view: Option<AccountViewData<'_>>,
    account_cb: Option<AccountCallbacks>,
    on_start: Callback<()>,
    on_cancel: Callback<()>,
) -> View {
    match auth {
        AuthState::Unknown => centered(StackPanel::new().spacing(12.0).children((
            ProgressRing::new()
                .is_indeterminate(true)
                .is_active(true)
                .width(32.0)
                .height(32.0)
                .horizontal_alignment(HorizontalAlignment::Center),
            TextBlock::new()
                .text("验证中…")
                .font_size(12.0)
                .opacity(0.6)
                .horizontal_alignment(HorizontalAlignment::Center),
        ))),

        AuthState::LoggedIn(_) => match (account_view, account_cb) {
            (Some(view), Some(cb)) => account_page(view, cb),
            _ => centered(TextBlock::new().text("加载中…").opacity(0.6)),
        },

        AuthState::Guest => match flow {
            LoginFlowState::Starting => centered(StackPanel::new().spacing(12.0).children((
                ProgressRing::new()
                    .is_indeterminate(true)
                    .is_active(true)
                    .width(32.0)
                    .height(32.0)
                    .horizontal_alignment(HorizontalAlignment::Center),
                TextBlock::new()
                    .text("正在生成二维码…")
                    .font_size(12.0)
                    .opacity(0.6)
                    .horizontal_alignment(HorizontalAlignment::Center),
            ))),

            LoginFlowState::Idle => ScrollViewer::new()
                .content(
                    Border::new()
                        .padding(Thickness::new(36.0, 24.0, 36.0, 36.0))
                        .content(
                            StackPanel::new().spacing(20.0).children((
                                TextBlock::new()
                                    .text("登录")
                                    .font_size(28.0)
                                    .font_weight(FontWeight::BOLD),
                                Border::new()
                                    .corner_radius(8.0)
                                    .background(ThemeBrush::CardBackground)
                                    .border_brush(ThemeBrush::CardStroke)
                                    .border_thickness(1.0)
                                    .padding(32.0)
                                    .max_width(420.0)
                                    .horizontal_alignment(HorizontalAlignment::Left)
                                    .content(
                                        StackPanel::new().spacing(16.0).children((
                                            TextBlock::new()
                                                .text("哔哩哔哩 App 扫码登录")
                                                .font_size(16.0)
                                                .font_weight(FontWeight::SEMI_BOLD)
                                                .horizontal_alignment(HorizontalAlignment::Center),
                                            Button::new()
                                                .style(ButtonStyle::Accent)
                                                .content("生成二维码")
                                                .on_click(on_start)
                                                .horizontal_alignment(HorizontalAlignment::Center),
                                        )),
                                    ),
                            )),
                        ),
                )
                .into(),

            other => {
                let (qr, status) = match other {
                    LoginFlowState::AwaitingScan { qr } => {
                        (Some(qr.clone()), "打开哔哩哔哩 App 扫一扫")
                    }
                    LoginFlowState::Scanned { qr } => {
                        (Some(qr.clone()), "已扫码，请在手机确认")
                    }
                    LoginFlowState::Expired { qr } => {
                        (Some(qr.clone()), "二维码已过期")
                    }
                    LoginFlowState::Failed(reason) => (None, reason.as_str()),
                    _ => (None, ""),
                };
                let is_failed = matches!(other, LoginFlowState::Failed(_));

                let mut card_children: Vec<View> = Vec::new();
                if let Some(img) = qr {
                    card_children.push(
                        Border::new()
                            .corner_radius(8.0)
                            .background(Brush::Solid(Color::rgb(255, 255, 255)))
                            .padding(10.0)
                            .horizontal_alignment(HorizontalAlignment::Center)
                            .content(
                                Image::new()
                                    .source_data(img)
                                    .width(216.0)
                                    .height(216.0)
                                    .stretch(Stretch::Uniform),
                            )
                            .into(),
                    );
                }
                if is_failed {
                    card_children.push(
                        InfoBar::new()
                            .title("登录失败")
                            .message(status)
                            .severity(InfoBarSeverity::Error)
                            .is_open(true)
                            .is_closable(false)
                            .into(),
                    );
                } else {
                    card_children.push(
                        TextBlock::new()
                            .text(status)
                            .font_size(13.0)
                            .opacity(0.75)
                            .horizontal_alignment(HorizontalAlignment::Center)
                            .into(),
                    );
                }
                card_children.push(
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(8.0)
                        .horizontal_alignment(HorizontalAlignment::Center)
                        .children((
                            Button::new()
                                .style(ButtonStyle::Accent)
                                .content(if matches!(other, LoginFlowState::Expired { .. }) {
                                    "重新生成"
                                } else {
                                    "刷新"
                                })
                                .on_click(on_start),
                            Button::new()
                                .style(ButtonStyle::Subtle)
                                .content("取消")
                                .on_click(on_cancel),
                        ))
                        .into(),
                );

                ScrollViewer::new()
                    .content(
                        Border::new()
                            .padding(Thickness::new(36.0, 24.0, 36.0, 36.0))
                            .content(
                                StackPanel::new().spacing(20.0).children((
                                    TextBlock::new()
                                        .text("登录")
                                        .font_size(28.0)
                                        .font_weight(FontWeight::BOLD),
                                    Border::new()
                                        .corner_radius(8.0)
                                        .background(ThemeBrush::CardBackground)
                                        .border_brush(ThemeBrush::CardStroke)
                                        .border_thickness(1.0)
                                        .padding(28.0)
                                        .max_width(420.0)
                                        .horizontal_alignment(HorizontalAlignment::Left)
                                        .content(StackPanel::new().spacing(14.0).children(card_children)),
                                )),
                            ),
                    )
                    .into()
            }
        },
    }
}

fn centered(content: impl Into<View>) -> View {
    Border::new()
        .horizontal_alignment(HorizontalAlignment::Center)
        .vertical_alignment(VerticalAlignment::Center)
        .content(content)
        .into()
}
