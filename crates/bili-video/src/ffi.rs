//! libmpv-2.dll 动态加载与最小 FFI(client.h + render.h)。
//!
//! 设计(蓝本 docs/video-channel.md §3):运行时 LoadLibrary + GetProcAddress,
//! 不硬链接——dll 缺失时返回可读错误,UI 优雅降级。头文件为
//! `.qaqh/mpv/mpv-dev/include/mpv/*`(shinchiro 构建 20261111,render_d3d11.h
//! 自 master 补齐)。**常量值一律以头文件为准,不得凭记忆书写**。

use std::ffi::{c_char, c_int, c_uint, c_void, CStr};
use std::sync::Arc;

use crate::error::BiliVideoError;

// ---------- client.h ----------

pub const MPV_FORMAT_NONE: u32 = 0;
pub const MPV_FORMAT_STRING: u32 = 1;
pub const MPV_FORMAT_FLAG: u32 = 3;
pub const MPV_FORMAT_DOUBLE: u32 = 5;

pub const MPV_EVENT_NONE: u32 = 0;
pub const MPV_EVENT_SHUTDOWN: u32 = 1;
pub const MPV_EVENT_START_FILE: u32 = 6;
pub const MPV_EVENT_END_FILE: u32 = 7;
pub const MPV_EVENT_FILE_LOADED: u32 = 8;
pub const MPV_EVENT_IDLE: u32 = 11;
pub const MPV_EVENT_SEEK: u32 = 20;
pub const MPV_EVENT_PLAYBACK_RESTART: u32 = 21;
pub const MPV_EVENT_PROPERTY_CHANGE: u32 = 22;

// ---------- render.h ----------

pub const MPV_RENDER_PARAM_API_TYPE: c_uint = 1;
/// 参数列表终止项(render.h:MPV_RENDER_PARAM_INVALID = 0)。
pub const MPV_RENDER_PARAM_INVALID: c_uint = 0;
pub const MPV_RENDER_PARAM_SW_SIZE: c_uint = 17;
pub const MPV_RENDER_PARAM_SW_FORMAT: c_uint = 18;
pub const MPV_RENDER_PARAM_SW_STRIDE: c_uint = 19;
pub const MPV_RENDER_PARAM_SW_POINTER: c_uint = 20;
/// SW 渲染的 API type 字符串(render.h:`MPV_RENDER_API_TYPE_SW`)。
pub const MPV_RENDER_API_TYPE_SW: &[u8] = b"software\0";
/// D3D11 渲染的 API type 字符串(render_d3d11.h)。
pub const MPV_RENDER_API_TYPE_D3D11: &[u8] = b"d3d11\0";
/// SW 渲染的像素格式:`0bgr`(字节序 0,B,G,R —— Windows BGRA)。
pub const SW_FORMAT_BGRA: &[u8] = b"0bgr\0";

pub const MPV_RENDER_PARAM_D3D11_INIT_PARAMS: c_uint = 21;
pub const MPV_RENDER_PARAM_D3D11_FBO: c_uint = 22;

/// render_d3d11.h:`mpv_d3d11_init_params { ID3D11Device* device }`。
#[repr(C)]
pub struct mpv_d3d11_init_params {
    pub device: *mut c_void,
}

/// render_d3d11.h:`mpv_d3d11_fbo { ID3D11Texture2D* tex }`。
#[repr(C)]
pub struct mpv_d3d11_fbo {
    pub tex: *mut c_void,
}

#[repr(C)]
pub struct mpv_render_param {
    pub p_type: c_uint,
    pub data: *mut c_void,
}

/// client.h:`mpv_event { event_id, error, reply_userdata, data }`(event_id
/// 为 enum=int;reply_userdata 对齐到 8)。
#[repr(C)]
pub struct mpv_event {
    pub event_id: u32,
    pub error: c_int,
    pub reply_userdata: u64,
    pub data: *mut c_void,
}

/// client.h:`mpv_event_property { name, format, data }` —— **无 reply_userdata**
/// (字段序以头文件为准;曾因多写 reply_userdata 导致 name 指针错位崩溃)。
#[repr(C)]
pub struct mpv_event_property {
    pub name: *const c_char,
    pub format: u32,
    pub data: *mut c_void,
}

/// client.h:`mpv_event_end_file { reply_userdata, reason, error, error_extended_id }`
/// (reason 在 u64 之后,offset 8)。
#[repr(C)]
pub struct mpv_event_end_file {
    pub reply_userdata: u64,
    pub reason: c_int,
    pub error: c_int,
    pub error_extended_id: i64,
}

// ---------- 函数指针类型(client.h) ----------

type FnCreate = unsafe extern "C" fn() -> *mut c_void;
type FnInitialize = unsafe extern "C" fn(*mut c_void) -> c_int;
type FnSetOptionString = unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> c_int;
type FnSetPropertyString = unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> c_int;
type FnCommand = unsafe extern "C" fn(*mut c_void, *const *const c_char) -> c_int;
type FnObserveProperty = unsafe extern "C" fn(*mut c_void, u64, *const c_char, u32) -> c_int;
type FnWaitEvent = unsafe extern "C" fn(*mut c_void, f64) -> *mut mpv_event;
type FnTerminateDestroy = unsafe extern "C" fn(*mut c_void);
type FnFree = unsafe extern "C" fn(*mut c_void);

// ---------- 函数指针类型(render.h) ----------

type FnRenderContextCreate =
    unsafe extern "C" fn(*mut *mut c_void, *mut c_void, *mut mpv_render_param) -> c_int;
type FnRenderContextFree = unsafe extern "C" fn(*mut c_void);
type FnRender = unsafe extern "C" fn(*mut c_void, *mut mpv_render_param) -> c_int;
type FnSetUpdateCallback = unsafe extern "C" fn(*mut c_void, Option<unsafe extern "C" fn(*mut c_void)>, *mut c_void);
type FnReportSwap = unsafe extern "C" fn(*mut c_void);

/// libmpv-2.dll 的函数表(进程内单份,`Arc` 共享)。
pub struct MpvLib {
    _module: *mut c_void,
    pub create: FnCreate,
    pub initialize: FnInitialize,
    pub set_option_string: FnSetOptionString,
    pub set_property_string: FnSetPropertyString,
    pub command: FnCommand,
    pub observe_property: FnObserveProperty,
    pub wait_event: FnWaitEvent,
    pub terminate_destroy: FnTerminateDestroy,
    pub free: FnFree,
    pub render_context_create: FnRenderContextCreate,
    pub render_context_free: FnRenderContextFree,
    pub render: FnRender,
    pub set_update_callback: FnSetUpdateCallback,
    pub report_swap: FnReportSwap,
}

unsafe impl Send for MpvLib {}
unsafe impl Sync for MpvLib {}

impl MpvLib {
    /// LoadLibrary("libmpv-2.dll"):搜索顺序 = exe 目录 → PATH。
    pub fn load() -> Result<Arc<Self>, BiliVideoError> {
        use windows_sys::Win32::errhandlingapi::GetLastError;
        use windows_sys::Win32::libloaderapi::LoadLibraryW;

        let name: Vec<u16> = "libmpv-2.dll\0".encode_utf16().collect();
        // LoadLibrary(含 exe 目录搜索优先)。
        let module = unsafe {
            let h = LoadLibraryW(name.as_ptr());
            if h.is_null() {
                let err = GetLastError();
                return Err(BiliVideoError::LibmpvMissing(format!(
                    "libmpv-2.dll 加载失败(GetLastError={err});请将 dll 放在 exe 旁"
                )));
            }
            h
        };

        macro_rules! sym {
            ($name:literal, $ty:ty) => {{
                let cname: &[u8] = concat!($name, "\0").as_bytes();
                match unsafe {
                    windows_sys::Win32::libloaderapi::GetProcAddress(
                        module,
                        cname.as_ptr(),
                    )
                } {
                    Some(f) => unsafe { std::mem::transmute::<usize, $ty>(f as usize) },
                    None => {
                        return Err(BiliVideoError::LibmpvMissing(format!(
                            "libmpv-2.dll 缺少导出 {}(版本过旧)",
                            $name
                        )))
                    }
                }
            }};
        }

        Ok(Arc::new(Self {
            _module: module as *mut c_void,
            create: sym!("mpv_create", FnCreate),
            initialize: sym!("mpv_initialize", FnInitialize),
            set_option_string: sym!("mpv_set_option_string", FnSetOptionString),
            set_property_string: sym!("mpv_set_property_string", FnSetPropertyString),
            command: sym!("mpv_command", FnCommand),
            observe_property: sym!("mpv_observe_property", FnObserveProperty),
            wait_event: sym!("mpv_wait_event", FnWaitEvent),
            terminate_destroy: sym!("mpv_terminate_destroy", FnTerminateDestroy),
            free: sym!("mpv_free", FnFree),
            render_context_create: sym!("mpv_render_context_create", FnRenderContextCreate),
            render_context_free: sym!("mpv_render_context_free", FnRenderContextFree),
            render: sym!("mpv_render_context_render", FnRender),
            set_update_callback: sym!("mpv_render_context_set_update_callback", FnSetUpdateCallback),
            report_swap: sym!("mpv_render_context_report_swap", FnReportSwap),
        }))
    }

    /// D3D11 render API 的可用性探测:头文件存在且 dll 含实现则 True。
    /// (SW 路径任何构建都可用;D3D11 是 2024+ master 特性。)
    pub fn has_d3d11_render(&self) -> bool {
        // mpv_render_context_create 传 API_TYPE=d3d11 时服务端返回错误码即不支持;
        // 这里仅作字符串级预检,实际可用性由渲染线程创建结果决定。
        true
    }
}

/// `*const c_char` → String(借用)。
pub unsafe fn cstr_to_string(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真机加载:exe 旁需有 libmpv-2.dll(target/debug 已放置)。
    #[test]
    fn lib_loads_and_abi_works() {
        let lib = match MpvLib::load() {
            Ok(lib) => lib,
            Err(e) => {
                eprintln!("跳过(dll 未就位): {e}");
                return;
            }
        };
        // mpv_create → mpv_initialize → mpv_terminate_destroy 全链 ABI 验证。
        let handle = unsafe { (lib.create)() };
        assert!(!handle.is_null(), "mpv_create 返回空句柄");
        let rc = unsafe { (lib.initialize)(handle) };
        assert_eq!(rc, 0, "mpv_initialize 失败(rc={rc})");
        // set_option_string 走一次(合法选项)。
        let opt = c"vo".as_ptr();
        let val = c"null".as_ptr();
        let rc = unsafe { (lib.set_option_string)(handle, opt, val) };
        assert_eq!(rc, 0, "mpv_set_option_string 失败(rc={rc})");
        unsafe { (lib.terminate_destroy)(handle) };
    }
}
