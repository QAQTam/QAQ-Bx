//! 登录页 / 个人中心：扫码二维码 / 登录态展示 / 登出（二次确认）。
//! 状态与回调由 AppShell 单源持有（`docs/ui-design.md` §1 页脚账号入口）。
//! 登录态（`AuthState::LoggedIn`）下本页即"个人中心"：账号卡片 + 消息中心空位
//! （骨架期占位；消息中心不设独立导航项，详见 `docs/ui-design.md` §1）。

use windows_reactor::*;
use crate::auth::{AuthState, LoginFlowState};

pub fn login_page(
    auth: &AuthState,
    flow: &LoginFlowState,
    logout_confirm: bool,
    notice: Option<String>,
    on_start: Callback<()>,
    on_cancel: Callback<()>,
    on_logout: Callback<()>,
) -> View {
    match auth {
        AuthState::Unknown => centered(StackPanel::new().spacing(12.0).children((
            ProgressRing::new().is_indeterminate(true).is_active(true).width(36.0).height(36.0)
                .horizontal_alignment(HorizontalAlignment::Center),
            TextBlock::new().text("正在检查登录态…").opacity(0.7),
        ))),

        AuthState::LoggedIn(user) => {
            let avatar: View = user.avatar.clone().map_or_else(
                || View::from(PersonPicture::new().display_name(user.uname.as_str()).width(64.0).height(64.0)),
                |img| {
                    View::from(Image::new().source_data(img).width(64.0).height(64.0).stretch(Stretch::Uniform))
                },
            );
            let (label, style) = if logout_confirm {
                ("再次点击确认登出", ButtonStyle::Default)
            } else {
                ("登出", ButtonStyle::Default)
            };
            // 账号资料行（投影自 bili_core::account::UserProfile）。
            let vip_label = format!("会员状态：{}", user.vip.label());
            let vip_opacity = if user.vip.is_vip() { 0.9 } else { 0.55 };
            // 空间主页：HyperlinkButton 依赖 WinUI 原生 NavigateUri 打开浏览器；
            // URL 异常时降级为纯文本（space_url 由 mid 派生，正常不触发）。
            let space_link: View = match HyperlinkButton::new()
                .content(user.space_url.as_str())
                .navigate_uri(user.space_url.as_str())
            {
                Ok(btn) => btn.into(),
                Err(_) => TextBlock::new().text(user.space_url.clone()).into(),
            };
            let profile_rows: Vec<View> = vec![
                TextBlock::new()
                    .text(user.uname.clone())
                    .font_size(18.0)
                    .font_weight(FontWeight::SEMI_BOLD)
                    .into(),
                TextBlock::new()
                    .text(format!("mid={} · Lv{}", user.mid, user.level))
                    .font_size(12.0)
                    .opacity(0.6)
                    .into(),
                TextBlock::new()
                    .text(vip_label)
                    .font_size(12.0)
                    .font_weight(FontWeight::SEMI_BOLD)
                    .opacity(vip_opacity)
                    .into(),
                space_link,
                TextBlock::new()
                    .text("登录态来自本地凭据（data/credentials.json），与服务端 nav 校验一致")
                    .font_size(11.0)
                    .opacity(0.5)
                    .text_wrapping(TextWrapping::Wrap)
                    .into(),
            ];
            // 卡片内容动态组装：主体 + 可选错误提示条。
            let card_items: Vec<View> = std::iter::once(
                StackPanel::new()
                    .spacing(12.0)
                    .children((
                        StackPanel::new()
                            .orientation(Orientation::Horizontal)
                            .spacing(16.0)
                            .children((
                                avatar,
                                StackPanel::new()
                                    .spacing(4.0)
                                    .vertical_alignment(VerticalAlignment::Center)
                                    .children(profile_rows),
                            )),
                        StackPanel::new()
                            .orientation(Orientation::Horizontal)
                            .spacing(8.0)
                            .children((
                                Button::new().style(style).content(label).on_click(on_logout),
                                TextBlock::new()
                                    .text("登出会作废服务端会话并清除本地凭据")
                                    .font_size(11.0)
                                    .opacity(0.55)
                                    .vertical_alignment(VerticalAlignment::Center),
                            )),
                    ))
                    .into(),
            )
            .chain(notice.map(|reason| {
                InfoBar::new()
                    .title("账号操作失败")
                    .message(reason)
                    .severity(InfoBarSeverity::Error)
                    .is_open(true)
                    .is_closable(false)
                    .into()
            }))
            .collect();

            ScrollViewer::new()
                .content(
                    Border::new()
                        .padding(Thickness::new(36.0, 24.0, 36.0, 36.0))
                        .content(
                            StackPanel::new().spacing(20.0).children((
                                TextBlock::new().text("个人中心").font_size(28.0).font_weight(FontWeight::BOLD),
                                Border::new()
                                    .corner_radius(8.0)
                                    .background(ThemeBrush::CardBackground)
                                    .border_brush(ThemeBrush::CardStroke)
                                    .border_thickness(1.0)
                                    .padding(24.0)
                                    .content(StackPanel::new().spacing(12.0).children(card_items)),
                                message_center_stub(),
                            )),
                        ),
                )
                .into()
        }

        AuthState::Guest => match flow {
            LoginFlowState::Starting => centered(StackPanel::new().spacing(12.0).children((
                ProgressRing::new().is_indeterminate(true).is_active(true).width(36.0).height(36.0)
                    .horizontal_alignment(HorizontalAlignment::Center),
                TextBlock::new().text("正在生成二维码…").opacity(0.7),
            ))),

            LoginFlowState::Idle => ScrollViewer::new()
                .content(
                    Border::new()
                        .padding(Thickness::new(36.0, 24.0, 36.0, 36.0))
                        .content(
                            StackPanel::new().spacing(20.0).children((
                                TextBlock::new().text("登录").font_size(28.0).font_weight(FontWeight::BOLD),
                                Border::new()
                                    .corner_radius(8.0)
                                    .background(ThemeBrush::CardBackground)
                                    .border_brush(ThemeBrush::CardStroke)
                                    .border_thickness(1.0)
                                    .padding(32.0)
                                    .content(
                                        StackPanel::new().spacing(16.0).children((
                                            TextBlock::new()
                                                .text("使用哔哩哔哩 App 扫码登录")
                                                .font_size(16.0)
                                                .font_weight(FontWeight::SEMI_BOLD)
                                                .horizontal_alignment(HorizontalAlignment::Center),
                                            TextBlock::new()
                                                .text("凭据经扫码流程写入本地文件，后续请求自动携带；Cookie 约 180 天有效，服务端标记后可续期。")
                                                .font_size(12.0)
                                                .opacity(0.6)
                                                .text_wrapping(TextWrapping::Wrap)
                                                .horizontal_alignment(HorizontalAlignment::Center),
                                            Button::new()
                                                .style(ButtonStyle::Accent)
                                                .content("生成登录二维码")
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
                        (Some(qr.clone()), "等待扫码：打开哔哩哔哩 App → 扫一扫".to_string())
                    }
                    LoginFlowState::Scanned { qr } => {
                        (Some(qr.clone()), "已扫码：请在手机上点击确认登录".to_string())
                    }
                    LoginFlowState::Expired { qr } => {
                        (Some(qr.clone()), "二维码已失效，请重新生成".to_string())
                    }
                    LoginFlowState::Failed(reason) => (None, reason.clone()),
                    _ => (None, String::new()),
                };
                let severity = match other {
                    LoginFlowState::Scanned { .. } => InfoBarSeverity::Success,
                    LoginFlowState::Expired { .. } => InfoBarSeverity::Warning,
                    LoginFlowState::Failed(_) => InfoBarSeverity::Error,
                    _ => InfoBarSeverity::Informational,
                };
                ScrollViewer::new()
                    .content(
                        Border::new()
                            .padding(Thickness::new(36.0, 24.0, 36.0, 36.0))
                            .content(
                                StackPanel::new().spacing(16.0).children((
                                    TextBlock::new().text("扫码登录").font_size(28.0).font_weight(FontWeight::BOLD),
                                    Border::new()
                                        .corner_radius(8.0)
                                        .background(ThemeBrush::CardBackground)
                                        .border_brush(ThemeBrush::CardStroke)
                                        .border_thickness(1.0)
                                        .padding(32.0)
                                        .content(
                                            StackPanel::new().spacing(14.0).children((
                                                qr.map_or_else(
                                                    || View::from(TextBlock::new()
                                                        .text("二维码不可用")
                                                        .opacity(0.6)
                                                        .horizontal_alignment(HorizontalAlignment::Center)),
                                                    |img| View::from(Image::new()
                                                        .source_data(img)
                                                        .width(240.0)
                                                        .height(240.0)
                                                        .stretch(Stretch::Uniform)
                                                        .horizontal_alignment(HorizontalAlignment::Center)),
                                                ),
                                                InfoBar::new()
                                                    .title("扫码流程")
                                                    .message("打开哔哩哔哩 App → 扫一扫，并在手机上确认登录。二维码约 3 分钟有效。")
                                                    .severity(severity)
                                                    .is_open(true)
                                                    .is_closable(false),
                                                TextBlock::new()
                                                    .text(status)
                                                    .font_size(13.0)
                                                    .opacity(0.75)
                                                    .horizontal_alignment(HorizontalAlignment::Center),
                                                StackPanel::new()
                                                    .orientation(Orientation::Horizontal)
                                                    .spacing(8.0)
                                                    .horizontal_alignment(HorizontalAlignment::Center)
                                                    .children((
                                                        Button::new()
                                                            .style(ButtonStyle::Accent)
                                                            .content(if matches!(other, LoginFlowState::Expired { .. }) {
                                                                "重新生成二维码"
                                                            } else {
                                                                "刷新二维码"
                                                            })
                                                            .on_click(on_start),
                                                        Button::new().content("取消").on_click(on_cancel),
                                                    )),
                                            )),
                                        ),
                                )),
                            ),
                    )
                    .into()
            }
        }
    }
}

/// 消息中心空位（骨架期）：在个人中心内部渲染，不设独立导航项。
/// 纯展示、无回调；Guest 态（二维码登录流程）不渲染本区。
///
/// 接入落点：`bili-core` 新增 `MessageClient`（需 Cookie，注入 `CredentialProvider`），
/// 端点线索见 `docs/bilibili-api-inventory.md` §8（`dynamic/entrance` 红点、
/// `api.vc.bilibili.com` 私信未读）。
fn message_center_stub() -> View {
    let rows: Vec<View> = ["回复我的", "@我的", "收到的赞", "系统通知", "私信"]
        .into_iter()
        .map(message_row)
        .collect();
    StackPanel::new()
        .spacing(12.0)
        .children((
            TextBlock::new()
                .text("消息中心")
                .font_size(20.0)
                .font_weight(FontWeight::SEMI_BOLD),
            TextBlock::new()
                .text("互动与通知 · 登录后可用")
                .font_size(12.0)
                .opacity(0.6),
            InfoBar::new()
                .title("消息中心数据源待接入")
                .message("骨架阶段仅占位：消息中心在个人中心内部渲染，不设独立导航项；接入时由 bili-core 提供 MessageClient 并注入登录凭据。")
                .severity(InfoBarSeverity::Informational)
                .is_open(true)
                .is_closable(false),
            Border::new()
                .corner_radius(8.0)
                .background(ThemeBrush::CardBackground)
                .border_brush(ThemeBrush::CardStroke)
                .border_thickness(1.0)
                .padding(24.0)
                .content(StackPanel::new().spacing(14.0).children(rows)),
        ))
        .into()
}

/// 消息中心占位行：名称 + "待接入"灰字。
fn message_row(label: &str) -> View {
    StackPanel::new()
        .orientation(Orientation::Horizontal)
        .spacing(12.0)
        .children((
            TextBlock::new()
                .text(label.to_string())
                .font_size(14.0)
                .font_weight(FontWeight::SEMI_BOLD),
            TextBlock::new()
                .text("暂无数据 · 待接入")
                .font_size(12.0)
                .opacity(0.5)
                .vertical_alignment(VerticalAlignment::Center),
        ))
        .into()
}

/// 页面内容垂直水平居中（在 builder 上施加对齐后再转 View）。
fn centered(content: StackPanel) -> View {
    content
        .horizontal_alignment(HorizontalAlignment::Center)
        .vertical_alignment(VerticalAlignment::Center)
        .into()
}
