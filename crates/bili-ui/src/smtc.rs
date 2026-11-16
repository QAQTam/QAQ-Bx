//! Windows 控制中心媒体控件（SMTC: `SystemMediaTransportControls`）桥接。
//!
//! 桌面 WinUI 3 应用（`runFullTrust` + 自研 `libmpv-2.dll` 播放器）需通过
//! `ISystemMediaTransportControlsInterop::GetForWindow(hwnd)` 绑定主窗口句柄，
//! 并将播放/暂停按钮事件回传到 `AppShell`。

use std::sync::mpsc::Sender;

use windows::Media::{
    MediaPlaybackStatus, MediaPlaybackType, SystemMediaTransportControls,
    SystemMediaTransportControlsButton,
};
use windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream, RandomAccessStreamReference};
use windows::Win32::minwindef::LPARAM;
use windows::Win32::processthreadsapi::GetCurrentThreadId;
use windows::Win32::systemmediatransportcontrolsinterop::ISystemMediaTransportControlsInterop;
use windows::Win32::windef::HWND;
use windows::Win32::winuser::{
    EnumThreadWindows, GW_OWNER, GetActiveWindow, GetWindow, IsWindowVisible,
};
use windows_core::{BOOL, EventRevoker, HSTRING};

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
        let interop: ISystemMediaTransportControlsInterop =
            windows_core::factory::<SystemMediaTransportControls, ISystemMediaTransportControlsInterop>()
                .ok()?;
        let controls: SystemMediaTransportControls =
            unsafe { interop.GetForWindow(hwnd) }.ok()?;

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
        let _ = self.controls.SetPlaybackStatus(MediaPlaybackStatus::Stopped);
        if let Ok(updater) = self.controls.DisplayUpdater() {
            let _ = updater.ClearAll();
            let _ = updater.Update();
        }
    }

    pub fn update_metadata(&self, title: &str, author: &str, cover_bytes: Option<&[u8]>) {
        let Ok(updater) = self.controls.DisplayUpdater() else {
            return;
        };
        let _ = updater.SetType(MediaPlaybackType::Video);
        if let Ok(props) = updater.VideoProperties() {
            let _ = props.SetTitle(&HSTRING::from(title));
            let _ = props.SetSubtitle(&HSTRING::from(author));
        }
        if let Some(bytes) = cover_bytes
            && let Some(thumb) = thumbnail_from_bytes(bytes)
        {
            let _ = updater.SetThumbnail(&thumb);
        }
        let _ = updater.Update();
    }
}

fn thumbnail_from_bytes(bytes: &[u8]) -> Option<RandomAccessStreamReference> {
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
