//! Windows 控制中心媒体控件（SMTC: `SystemMediaTransportControls`）桥接。
//!
//! 两条通道(v0.3 起并存,由 shell 按播放引擎分流):
//! - **mpv 通道**(`SmtcController`):自研 libmpv 播放器没有系统媒体管线,必须经
//!   `ISystemMediaTransportControlsInterop::GetForWindow(hwnd)` 绑定主窗口句柄,
//!   自己写状态/元数据,并把按钮事件回传到 `AppShell`。
//! - **native 通道**(`NativeSmtc`):`MediaPlayer` 自带 SMTC —— 播放状态、时间轴、
//!   播放/暂停按钮指令(`CommandManager` 自动生成)全部由系统媒体管线驱动,我们
//!   只补 B 站侧元数据(标题/UP 主/封面)。再手写状态会与系统抢同一份控件,
//!   所以这条通道**不写** PlaybackStatus,只在状态变化时读一次做诊断锚点。

use std::sync::mpsc::Sender;

use windows::Media::{
    MediaPlaybackStatus, MediaPlaybackType, SystemMediaTransportControls,
    SystemMediaTransportControlsButton,
};
use windows::Storage::Streams::{
    DataWriter, InMemoryRandomAccessStream, RandomAccessStreamReference,
};
use windows::Win32::minwindef::LPARAM;
use windows::Win32::processthreadsapi::GetCurrentThreadId;
use windows::Win32::systemmediatransportcontrolsinterop::ISystemMediaTransportControlsInterop;
use windows::Win32::windef::HWND;
use windows::Win32::winuser::{
    EnumThreadWindows, GW_OWNER, GetActiveWindow, GetWindow, IsWindowVisible,
};
use windows_core::{BOOL, EventRevoker, HSTRING};
use windows::Media::Playback::MediaPlayer;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SmtcCommand {
    Play,
    Pause,
}

pub struct SmtcController {
    controls: SystemMediaTransportControls,
    _button_revoker: EventRevoker,
}

impl SmtcController {
    /// 在当前 UI 线程查找主窗口 `HWND` 并绑定 `SystemMediaTransportControls`。
    pub fn try_init(tx: Sender<SmtcCommand>) -> Option<Self> {
        let hwnd = find_app_hwnd()?;
        let interop: ISystemMediaTransportControlsInterop = windows_core::factory::<
            SystemMediaTransportControls,
            ISystemMediaTransportControlsInterop,
        >()
        .ok()?;
        let controls: SystemMediaTransportControls = unsafe { interop.GetForWindow(hwnd) }.ok()?;

        let _ = controls.SetIsEnabled(true);
        let _ = controls.SetIsPlayEnabled(true);
        let _ = controls.SetIsPauseEnabled(true);

        let revoker = controls
            .ButtonPressed(move |_, args| {
                if let Ok(args) = args.ok()
                    && let Ok(btn) = args.Button()
                {
                    match btn {
                        SystemMediaTransportControlsButton::Play => {
                            let _ = tx.send(SmtcCommand::Play);
                        }
                        SystemMediaTransportControlsButton::Pause => {
                            let _ = tx.send(SmtcCommand::Pause);
                        }
                        _ => {}
                    }
                }
            })
            .ok()?;

        Some(Self {
            controls,
            _button_revoker: revoker,
        })
    }

    pub fn set_playing(&self, playing: bool) {
        let status = if playing {
            MediaPlaybackStatus::Playing
        } else {
            MediaPlaybackStatus::Paused
        };
        let _ = self.controls.SetPlaybackStatus(status);
    }

    pub fn set_stopped(&self) {
        let _ = self
            .controls
            .SetPlaybackStatus(MediaPlaybackStatus::Stopped);
        if let Ok(updater) = self.controls.DisplayUpdater() {
            let _ = updater.ClearAll();
            let _ = updater.Update();
        }
    }

    pub fn update_metadata(&self, title: &str, author: &str, cover_bytes: Option<&[u8]>) {
        write_video_metadata(&self.controls, title, author, cover_bytes);
    }
}

/// SMTC 展示字段的纯逻辑部分(空/空白标题与 UP 主走占位;单测覆盖)。
pub(crate) fn display_fields(title: &str, author: &str) -> (String, String) {
    let clean = |value: &str, fallback: &str| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            fallback.to_string()
        } else {
            trimmed.to_string()
        }
    };
    (
        clean(title, "未知标题"),
        clean(author, "bilibili"),
    )
}

/// 把视频元数据写进任意 SMTC 句柄(手写桥与 MediaPlayer 自持控件共用)。
fn write_video_metadata(
    controls: &SystemMediaTransportControls,
    title: &str,
    author: &str,
    cover_bytes: Option<&[u8]>,
) {
    let Ok(updater) = controls.DisplayUpdater() else {
        return;
    };
    let _ = updater.SetType(MediaPlaybackType::Video);
    if let Ok(props) = updater.VideoProperties() {
        let (title, author) = display_fields(title, author);
        let _ = props.SetTitle(&HSTRING::from(title.as_str()));
        let _ = props.SetSubtitle(&HSTRING::from(author.as_str()));
    }
    if let Some(bytes) = cover_bytes
        && let Some(thumb) = thumbnail_from_bytes(bytes)
    {
        let _ = updater.SetThumbnail(&thumb);
    }
    let _ = updater.Update();
}

/// native 通道:`MediaPlayer` 自持的 SMTC(状态/时间轴/按钮由系统管线驱动)。
pub struct NativeSmtc {
    controls: SystemMediaTransportControls,
    /// 绑定时读到的 player 原始指针,用于识别「面板重建换了 player」并重新绑定。
    player_raw: *mut core::ffi::c_void,
}

impl NativeSmtc {
    /// 从引擎接管到的 player 建立绑定;player 为空 = 尚未 attach。
    pub fn from_player(player: &MediaPlayer) -> Option<Self> {
        let controls = player.SystemMediaTransportControls().ok()?;
        // 指令通道开启即可:播放/暂停按钮由 `CommandManager` 自动生成并直接作用到
        // player(AutoGenerateInstructions / IsPlayPauseButtonEnabled 默认开)。
        let _ = controls.SetIsEnabled(true);
        if let Ok(manager) = player.CommandManager() {
            let _ = manager.SetIsEnabled(true);
        }
        Some(Self {
            controls,
            player_raw: windows_core::Interface::as_raw(player) as *mut _,
        })
    }

    /// 绑定的 player 是否已更换(面板重建后必须重新 `from_player`)。
    pub fn is_bound_to(&self, player: &MediaPlayer) -> bool {
        (windows_core::Interface::as_raw(player) as *mut core::ffi::c_void) == self.player_raw
    }

    pub fn update_metadata(&self, title: &str, author: &str, cover_bytes: Option<&[u8]>) {
        write_video_metadata(&self.controls, title, author, cover_bytes);
    }

    /// 关播:状态交回 Stopped 并清空展示(系统侧不留残卡)。
    pub fn set_stopped(&self) {
        let _ = self
            .controls
            .SetPlaybackStatus(MediaPlaybackStatus::Stopped);
        if let Ok(updater) = self.controls.DisplayUpdater() {
            let _ = updater.ClearAll();
            let _ = updater.Update();
        }
    }

    /// 当前系统侧看到的播放状态(锁屏/控制栏显示的就是它;诊断锚用)。
    pub fn playback_status(&self) -> Option<MediaPlaybackStatus> {
        self.controls.PlaybackStatus().ok()
    }
}

fn thumbnail_from_bytes(bytes: &[u8]) -> Option<RandomAccessStreamReference> {
    // 空载荷直接判定为「无封面」:否则会往锁屏挂一张必坏的缩略图。
    if bytes.is_empty() {
        return None;
    }
    let stream = InMemoryRandomAccessStream::new().ok()?;
    let writer = DataWriter::CreateDataWriter(&stream).ok()?;
    writer.WriteBytes(bytes).ok()?;
    writer.StoreAsync().ok()?.join().ok()?;
    let _ = writer.DetachStream();
    stream.Seek(0).ok()?;
    RandomAccessStreamReference::CreateFromStream(&stream).ok()
}

fn find_app_hwnd() -> Option<HWND> {
    let active = unsafe { GetActiveWindow() };
    if !active.is_null() {
        return Some(active);
    }
    let mut found: HWND = std::ptr::null_mut();
    let tid = unsafe { GetCurrentThreadId() };
    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        unsafe {
            let is_visible = IsWindowVisible(hwnd).as_bool();
            let owner = GetWindow(hwnd, GW_OWNER as u32);
            if is_visible && owner.is_null() {
                let target = &mut *(lparam as *mut HWND);
                *target = hwnd;
                return BOOL(0);
            }
            BOOL(1)
        }
    }
    let _ = unsafe { EnumThreadWindows(tid, Some(enum_proc), &mut found as *mut HWND as LPARAM) };
    if found.is_null() { None } else { Some(found) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 展示字段清洗:空白标题/UP 主走占位,两侧(native/手写桥)共用同一口径。
    #[test]
    fn display_fields_trim_and_fallback() {
        assert_eq!(
            display_fields("  测试视频  ", " 某UP "),
            ("测试视频".to_string(), "某UP".to_string())
        );
        let (title, author) = display_fields("", "   ");
        assert_eq!(title, "未知标题");
        assert_eq!(author, "bilibili");
    }

    /// 封面载荷闸:空载荷直接判定「无封面」,不会往锁屏挂一张必坏的图。
    /// (内存流管线本身需要 COM 套间,与并发测试线程的套间模型冲突,故其端到端
    /// 行为由 E12 真机场景覆盖——那里 SMTC 元数据/缩略图走的就是这条路径。)
    #[test]
    fn thumbnail_reference_requires_non_empty_payload() {
        assert!(thumbnail_from_bytes(&[]).is_none());
    }
}
