//! 播放页三态视图构建:主窗/全屏 HUD 的 `PlayerPage` 内容与半窗层。
//!
//! 拆解约定(见 docs/dev-notes/shell-decomposition-plan.md §3):渲染面始终由同一个
//! keyed `player:{bvid}` 组件持有(E24/E25/E26 零重挂的前提),本文件只做属性驱动的
//! 视图装配,不增删组件层级。

use super::super::*;

impl AppShell {
    /// S2 常驻播放层:播放页视图构造单源(park 模式下路由离开也要渲染同一层,
    /// 组件与 MPE 才不会过渡卸载)。
    pub(in crate::shell) fn player_page_content(
        &self,
        author_avatar: &Option<EncodedImage>,
        my_avatar: &Option<EncodedImage>,
        comment_cb: &CommentCallbacks,
        on_open: &Callback<String>,
        layout: ViewportLayout,
        fullscreen: bool,
        pane: Option<(f64, f64, f64)>,
        context: &mut ViewContext<Self>,
    ) -> Option<View> {
            Some(            match (self.now_playing.as_ref(), self.engine.as_ref()) {
                (Some(item), Some(engine)) => {
                    View::component::<pages::player::PlayerPage>(pages::player::PlayerInput {
                        item: item.clone(),
                        author_avatar: author_avatar.clone(),
                        my_avatar: my_avatar.clone(),
                        desc: self.now_playing_desc.clone(),
                        playing: self.playing,
                        position: self.position,
                        volume: self.volume,
                        danmaku_enabled: self.settings.danmaku_enabled,
                        quality_menu: self.quality_menu_view(),
                        current_qn: self.current_qn,
                        pages_menu: self.pages_menu_view(),
                        current_cid: self.current_cid,
                        layout,
                        mount_tick: self.mount_tick,
                        duration_secs: self.display_duration(item),
                        notice: self.player_notice.clone(),
                        loading: self.video_loading,
                        engine: engine.clone(),
                        native: self.native_sink().cloned(),
                        stage: self.stage_sink().cloned(),
                        comments: self.comments_snapshot.clone(),
                        comment_cb: comment_cb.clone(),
                        related: self.related.clone(),
                        on_open_video: on_open.clone(),
                        on_related_retry: context.message(Message::RelatedRetry),
                        on_toggle: context.message(Message::TogglePlay),
                        on_seek: context.callback(|value: f64| Message::Seek(value)),
                        on_volume: context.callback(|value: f64| Message::SetVolume(value)),
                        on_volume_wheel: context.callback(|delta: i32| {
                            Message::PlayerHotkey(pages::fullscreen::PlayerHotkey::VolumeDelta(
                                delta,
                            ))
                        }),
                        on_toggle_mute: context.message(Message::ToggleMute),
                        on_toggle_danmaku: context.message(Message::ToggleDanmaku),
                        on_select_quality: context.callback(Message::SelectQuality),
                        on_select_page: context.callback(Message::SelectPage),
                        on_enter_fullscreen: context.message(Message::EnterFullscreen),
                        demo_override: self.demo.as_ref().and_then(|d| d.player_override),
                        fullscreen,
                        fullscreen_window_height: self.fullscreen_window_height,
                        pane_mode: pane.is_some(),
                        pane_width: pane.map(|p| p.0).unwrap_or(0.0),
                        pane_max_width: pane.map(|p| p.1).unwrap_or(0.0),
                        pane_max_height: pane.map(|p| p.2).unwrap_or(0.0),
                        on_pane_expand: context.message(Message::ExpandHalfPane),
                        on_pane_close: context.message(Message::CloseHalfPane),
                        on_pane_resize: context.callback(Message::ResizeHalfPane),
                    })
                }
                _ => self.home_view(context),
            })
    }

    pub(in crate::shell) fn half_pane_layer(
        &self,
        author_avatar: Option<EncodedImage>,
        my_avatar: Option<EncodedImage>,
        comment_cb: CommentCallbacks,
        on_open: &Callback<String>,
        context: &mut ViewContext<Self>,
    ) -> Option<KeyedView> {
        let (Some(item), Some(engine)) = (self.now_playing.as_ref(), self.engine.as_ref()) else {
            return None;
        };
        let max_width =
            (self.viewport_layout.content_width() - 48.0).max(pages::player::HALF_PANE_MIN_WIDTH);
        // 与主窗/全屏共用同一 key 与同一组件:surface 的 keyed 路径完全一致,
        // 半窗↔主窗只是 input 属性切换,native MPE 不销毁、不换源重载。
        let pane_geom = (
            self.settings.half_pane_width,
            max_width,
            self.viewport_layout.content_height(),
        );
        let content = self.player_page_content(
            &author_avatar,
            &my_avatar,
            &comment_cb,
            on_open,
            self.viewport_layout,
            false,
            Some(pane_geom),
            context,
        )?;
        Some(keyed(
            format!("player:{}", item.bvid),
            Border::new()
                .width(pane_geom.0)
                .horizontal_alignment(windows_reactor::HorizontalAlignment::Right)
                .vertical_alignment(windows_reactor::VerticalAlignment::Stretch)
                .background(ThemeBrush::SolidBackground)
                .content(content),
        ))
    }
}
