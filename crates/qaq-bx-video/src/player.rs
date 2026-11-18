//! mpv 播放器封装:实例生命周期、loadfile 分轨、属性观察事件线程、命令 API。
//!
//! 线程模型(render.h):`mpv_render_*` 可任意线程调用;事件轮询在专属线程
//! (`mpv_wait_event` 为线程内独占)。渲染上下文按需创建(SW 或 D3D11)。

use std::ffi::{CString, c_char, c_void};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::error::BiliVideoError;
use crate::ffi::{
    MPV_EVENT_END_FILE, MPV_EVENT_FILE_LOADED, MPV_EVENT_IDLE, MPV_EVENT_PLAYBACK_RESTART,
    MPV_EVENT_PROPERTY_CHANGE, MPV_EVENT_SEEK, MPV_EVENT_SHUTDOWN, MPV_EVENT_START_FILE,
    MPV_FORMAT_DOUBLE, MPV_FORMAT_FLAG, MPV_FORMAT_STRING, MPV_RENDER_API_TYPE_SW,
    MPV_RENDER_PARAM_API_TYPE, MPV_RENDER_PARAM_INVALID, MPV_RENDER_PARAM_SW_FORMAT,
    MPV_RENDER_PARAM_SW_POINTER, MPV_RENDER_PARAM_SW_SIZE, MPV_RENDER_PARAM_SW_STRIDE, MpvLib,
    SW_FORMAT_BGRA, mpv_event, mpv_event_property, mpv_render_param,
};

/// 属性变化事件(observe 的值按 format 分派)。
#[derive(Debug, Clone)]
pub enum PropertyValue {
    Double(f64),
    Flag(bool),
    Text(String),
}

#[derive(Debug, Clone)]
pub enum MpvEvent {
    StartFile,
    FileLoaded,
    PlaybackRestart,
    Seek,
    Idle,
    PropertyChange { name: String, value: PropertyValue },
    EndFile { reason: i32 },
    Shutdown,
    Other(u32),
}

/// mpv 播放器句柄(libmpv 实例 + 可选渲染上下文)。
pub struct MpvPlayer {
    lib: Arc<MpvLib>,
    handle: *mut c_void,
    ctx: Mutex<Option<*mut c_void>>,
    event_thread: Mutex<Option<JoinHandle<()>>>,
    stopping: Arc<AtomicBool>,
}

/// 跨线程裸指针包装(事件线程持有 mpv_handle)。
#[derive(Clone, Copy)]
struct SendPtr(*mut c_void);
unsafe impl Send for SendPtr {}

unsafe impl Send for MpvPlayer {}
unsafe impl Sync for MpvPlayer {}

impl MpvPlayer {
    /// 创建并初始化 mpv 实例(默认选项:vo=libmpv 渲染 API 必需)。
    pub fn new(lib: Arc<MpvLib>) -> Result<Self, BiliVideoError> {
        let handle = unsafe { (lib.create)() };
        if handle.is_null() {
            return Err(BiliVideoError::MpvApi(-1));
        }
        let rc = unsafe { (lib.initialize)(handle) };
        if rc < 0 {
            unsafe { (lib.terminate_destroy)(handle) };
            return Err(BiliVideoError::MpvApi(rc));
        }
        let player = Self {
            lib,
            handle,
            ctx: Mutex::new(None),
            event_thread: Mutex::new(None),
            stopping: Arc::new(AtomicBool::new(false)),
        };
        // 渲染 API 必需 vo=libmpv;8GB 内存友好封顶解复用缓存避免吃光系统内存
        // (P2 基线:8+4MiB 在 2Mbps 流上约 45s 缓冲,远小于 32+16 旧的 48MiB)。
        player.set_options(&[
            ("vo", "libmpv"),
            ("cache", "yes"),
            ("keep-open", "yes"),
            ("demuxer-max-bytes", "8MiB"),
            ("demuxer-max-back-bytes", "4MiB"),
            ("demuxer-readahead-secs", "15"),
        ])?;
        Ok(player)
    }

    /// 批量设置选项(初始化后、loadfile 前)。
    pub fn set_options(&self, options: &[(&str, &str)]) -> Result<(), BiliVideoError> {
        for (name, value) in options {
            let (n, v) = (CString::new(*name)?, CString::new(*value)?);
            let rc = unsafe { (self.lib.set_option_string)(self.handle, n.as_ptr(), v.as_ptr()) };
            if rc < 0 {
                return Err(BiliVideoError::MpvApi(rc));
            }
        }
        Ok(())
    }

    /// loadfile:视频流(必)+ 音频流(可选,外挂音轨)+ 防盗链头。
    ///
    /// 音轨与头经 `mpv_set_option_string` 全局设置(对主轨与 audio-files
    /// 外挂轨同效);字符串命令接口不支持 options 表(mpv_command_node 才可),
    /// 下次 loadfile 前重设即覆盖。
    pub fn loadfile(
        &self,
        video_url: &str,
        audio_url: Option<&str>,
        headers: &[(&str, &str)],
    ) -> Result<(), BiliVideoError> {
        if let Some(audio) = audio_url {
            self.set_options(&[("audio-files", audio)])?;
        }
        if !headers.is_empty() {
            let joined = headers
                .iter()
                .map(|(k, v)| format!("{k}: {v}"))
                .collect::<Vec<_>>()
                .join(",");
            self.set_options(&[("http-header-fields", &joined)])?;
        }
        let args = vec![
            CString::new("loadfile")?,
            CString::new(video_url)?,
            CString::new("replace")?,
        ];
        let mut ptrs: Vec<*const c_char> =
            args.iter().map(|a| a.as_ptr() as *const c_char).collect();
        ptrs.push(std::ptr::null()); // 终止必须是 NULL 指针(空串会被当参数)
        let rc = unsafe { (self.lib.command)(self.handle, ptrs.as_ptr()) };
        if rc < 0 {
            return Err(BiliVideoError::MpvApi(rc));
        }
        Ok(())
    }

    /// 启动事件轮询线程(wait_event 0.25s 轮询;PROPERTY_CHANGE 按 format 分派)。
    /// 观察项:time-pos/duration/pause/eof-reached/speed(调用方按需 observe)。
    pub fn start_event_thread(
        &mut self,
        on_event: Arc<dyn Fn(MpvEvent) + Send + Sync>,
    ) -> Result<(), BiliVideoError> {
        let mut guard = self
            .event_thread
            .lock()
            .map_err(|_| BiliVideoError::Poisoned)?;
        if guard.is_some() {
            return Ok(()); // 已在运行
        }
        self.stopping.store(false, Ordering::SeqCst);
        let handle_ptr = SendPtr(self.handle);
        let lib = self.lib.clone();
        let stopping = self.stopping.clone();
        *guard = Some(std::thread::spawn(move || {
            // 整体绑定 SendPtr(闭包内使用字段会触发 disjoint capture 捕获裸指针)。
            let handle = handle_ptr;
            loop {
                if stopping.load(Ordering::SeqCst) {
                    break;
                }
                // wait_event 阻塞至事件或超时(0.25s);线程内独占该调用。
                let event: &mpv_event = unsafe { &*((lib.wait_event)(handle.0, 0.25)) };
                let id = event.event_id;
                let data = event.data;
                let event = match id {
                    MPV_EVENT_PROPERTY_CHANGE => {
                        let prop = unsafe { &*(data as *const mpv_event_property) };
                        let name = unsafe { crate::ffi::cstr_to_string(prop.name) };
                        let value = unsafe { property_value(prop.format, prop.data) };
                        MpvEvent::PropertyChange { name, value }
                    }
                    MPV_EVENT_START_FILE => MpvEvent::StartFile,
                    MPV_EVENT_FILE_LOADED => MpvEvent::FileLoaded,
                    MPV_EVENT_PLAYBACK_RESTART => MpvEvent::PlaybackRestart,
                    MPV_EVENT_SEEK => MpvEvent::Seek,
                    MPV_EVENT_IDLE => MpvEvent::Idle,
                    MPV_EVENT_END_FILE => {
                        // mpv_event_end_file { reply_userdata, reason, ... }:reason
                        // 在 u64 之后(offset 8),不能直接按 i32 读首字段。
                        let reason = if data.is_null() {
                            0
                        } else {
                            unsafe { (*(data as *const crate::ffi::mpv_event_end_file)).reason }
                        };
                        MpvEvent::EndFile { reason }
                    }
                    MPV_EVENT_SHUTDOWN => MpvEvent::Shutdown,
                    other => MpvEvent::Other(other),
                };
                on_event(event);
            }
        }));
        Ok(())
    }

    /// 创建 SW 渲染上下文(API_TYPE=software,输出 bgra)。
    pub fn create_render_context_sw(&mut self) -> Result<(), BiliVideoError> {
        let api = MPV_RENDER_API_TYPE_SW.as_ptr() as *mut c_void;
        let params = [
            mpv_render_param {
                p_type: MPV_RENDER_PARAM_API_TYPE,
                data: api,
            },
            mpv_render_param {
                p_type: MPV_RENDER_PARAM_INVALID,
                data: std::ptr::null_mut(),
            },
        ];
        let mut ctx: *mut c_void = std::ptr::null_mut();
        let rc = unsafe {
            (self.lib.render_context_create)(
                &mut ctx,
                self.handle,
                params.as_ptr() as *mut mpv_render_param,
            )
        };
        if rc < 0 {
            return Err(BiliVideoError::MpvApi(rc));
        }
        *self.ctx.lock().map_err(|_| BiliVideoError::Poisoned)? = Some(ctx);
        Ok(())
    }

    /// 创建 D3D11 渲染上下文(设备由调用方创建;须支持 BGRA 且非 SINGLETHREADED)。
    pub fn create_render_context_d3d11(
        &mut self,
        init: &crate::ffi::mpv_d3d11_init_params,
    ) -> Result<(), BiliVideoError> {
        let api = crate::ffi::MPV_RENDER_API_TYPE_D3D11.as_ptr() as *mut c_void;
        let params = [
            mpv_render_param {
                p_type: MPV_RENDER_PARAM_API_TYPE,
                data: api,
            },
            mpv_render_param {
                p_type: crate::ffi::MPV_RENDER_PARAM_D3D11_INIT_PARAMS,
                data: init as *const crate::ffi::mpv_d3d11_init_params as *mut c_void,
            },
            mpv_render_param {
                p_type: MPV_RENDER_PARAM_INVALID,
                data: std::ptr::null_mut(),
            },
        ];
        let mut ctx: *mut c_void = std::ptr::null_mut();
        let rc = unsafe {
            (self.lib.render_context_create)(
                &mut ctx,
                self.handle,
                params.as_ptr() as *mut mpv_render_param,
            )
        };
        if rc < 0 {
            return Err(BiliVideoError::MpvApi(rc));
        }
        *self.ctx.lock().map_err(|_| BiliVideoError::Poisoned)? = Some(ctx);
        Ok(())
    }

    /// D3D11 渲染一帧到指定纹理(RENDER_TARGET + USAGE_DEFAULT 的 backbuffer)。
    pub fn render_d3d11(&self, fbo: &crate::ffi::mpv_d3d11_fbo) -> Result<(), BiliVideoError> {
        let ctx = *self.ctx.lock().map_err(|_| BiliVideoError::Poisoned)?;
        let Some(ctx) = ctx else {
            return Err(BiliVideoError::MpvApi(-3)); // 渲染上下文未创建
        };
        let params = [
            mpv_render_param {
                p_type: crate::ffi::MPV_RENDER_PARAM_D3D11_FBO,
                data: fbo as *const crate::ffi::mpv_d3d11_fbo as *mut c_void,
            },
            mpv_render_param {
                p_type: MPV_RENDER_PARAM_INVALID,
                data: std::ptr::null_mut(),
            },
        ];
        let rc = unsafe { (self.lib.render)(ctx, params.as_ptr() as *mut mpv_render_param) };
        if rc < 0 {
            return Err(BiliVideoError::MpvApi(rc));
        }
        Ok(())
    }

    /// SW 渲染一帧到调用方缓冲(bgra,stride = w*4)。
    pub fn render_sw(
        &self,
        width: u32,
        height: u32,
        buffer: &mut [u8],
    ) -> Result<(), BiliVideoError> {
        let ctx = *self.ctx.lock().map_err(|_| BiliVideoError::Poisoned)?;
        let Some(ctx) = ctx else {
            return Err(BiliVideoError::MpvApi(-3)); // 渲染上下文未创建
        };
        let size: [u32; 2] = [width, height];
        let stride: usize = width as usize * 4;
        let format = SW_FORMAT_BGRA.as_ptr() as *mut c_void;
        let params = [
            mpv_render_param {
                p_type: MPV_RENDER_PARAM_SW_SIZE,
                data: size.as_ptr() as *mut c_void,
            },
            mpv_render_param {
                p_type: MPV_RENDER_PARAM_SW_FORMAT,
                data: format,
            },
            mpv_render_param {
                p_type: MPV_RENDER_PARAM_SW_STRIDE,
                data: &stride as *const usize as *mut c_void,
            },
            mpv_render_param {
                p_type: MPV_RENDER_PARAM_SW_POINTER,
                data: buffer.as_mut_ptr() as *mut c_void,
            },
            mpv_render_param {
                p_type: MPV_RENDER_PARAM_INVALID,
                data: std::ptr::null_mut(),
            },
        ];
        let rc = unsafe { (self.lib.render)(ctx, params.as_ptr() as *mut mpv_render_param) };
        if rc < 0 {
            return Err(BiliVideoError::MpvApi(rc));
        }
        Ok(())
    }

    /// 渲染回调(update callback 驱动渲染循环)。
    pub fn set_update_callback(
        &self,
        callback: Option<unsafe extern "C" fn(*mut c_void)>,
        user: *mut c_void,
    ) -> Result<(), BiliVideoError> {
        let ctx = *self.ctx.lock().map_err(|_| BiliVideoError::Poisoned)?;
        let Some(ctx) = ctx else {
            return Err(BiliVideoError::MpvApi(-3));
        };
        unsafe { (self.lib.set_update_callback)(ctx, callback, user) };
        Ok(())
    }

    /// 观察属性(id 为 reply_userdata,format 决定事件值类型)。
    pub fn observe_property(&self, id: u64, name: &str, format: u32) -> Result<(), BiliVideoError> {
        let n = CString::new(name)?;
        let rc = unsafe { (self.lib.observe_property)(self.handle, id, n.as_ptr(), format) };
        if rc < 0 {
            return Err(BiliVideoError::MpvApi(rc));
        }
        Ok(())
    }

    /// 命令(play/pause/seek/set/volume 等)。
    pub fn command(&self, args: &[&str]) -> Result<(), BiliVideoError> {
        let mut cargs: Vec<CString> = Vec::new();
        for a in args {
            cargs.push(CString::new(*a)?);
        }
        let mut ptrs: Vec<*const c_char> =
            cargs.iter().map(|a| a.as_ptr() as *const c_char).collect();
        ptrs.push(std::ptr::null()); // 终止必须是 NULL 指针
        let rc = unsafe { (self.lib.command)(self.handle, ptrs.as_ptr()) };
        if rc < 0 {
            return Err(BiliVideoError::MpvApi(rc));
        }
        Ok(())
    }

    pub fn play(&self) -> Result<(), BiliVideoError> {
        self.command(&["set", "pause", "no"])
    }

    pub fn pause(&self) -> Result<(), BiliVideoError> {
        self.command(&["set", "pause", "yes"])
    }

    pub fn stop(&self) -> Result<(), BiliVideoError> {
        self.command(&["stop"])
    }

    pub fn seek(&self, secs: f64) -> Result<(), BiliVideoError> {
        self.command(&["seek", &format!("{secs}"), "absolute"])
    }

    pub fn set_volume(&self, pct: f64) -> Result<(), BiliVideoError> {
        self.command(&["set", "volume", &format!("{pct}")])
    }

    /// 停止事件线程(等待 join)。
    pub fn stop_event_thread(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        if let Ok(mut guard) = self.event_thread.lock() {
            if let Some(t) = guard.take() {
                let _ = t.join();
            }
        }
    }
}

impl Drop for MpvPlayer {
    fn drop(&mut self) {
        self.stop_event_thread();
        // 渲染上下文必须先于 mpv 实例销毁。
        if let Ok(mut guard) = self.ctx.lock() {
            if let Some(ctx) = guard.take() {
                unsafe { (self.lib.render_context_free)(ctx) };
            }
        }
        unsafe { (self.lib.terminate_destroy)(self.handle) };
    }
}

/// 按 format 分派属性值(NONE → None 值;调用方按 name 语义处理)。
///
/// # Safety
/// `data` 指向 mpv 按格式分配的值(STRING 需 mpv_free 释放)。
unsafe fn property_value(format: u32, data: *mut c_void) -> PropertyValue {
    unsafe {
        match format {
            MPV_FORMAT_DOUBLE => PropertyValue::Double((data as *const f64).read()),
            MPV_FORMAT_FLAG => PropertyValue::Flag((data as *const i32).read() != 0),
            MPV_FORMAT_STRING => {
                let p = *(data as *const *mut c_void);
                let s = crate::ffi::cstr_to_string(p as *const _);
                if !p.is_null() {
                    // STRING 属性值需 mpv_free(data 是 char** 指向的字符串本体)
                    // lib:mpv_free 只作用于 property 返回的 char*(这里 data 存的是指针)
                }
                PropertyValue::Text(s)
            }
            _ => PropertyValue::Text(String::new()),
        }
    }
}
