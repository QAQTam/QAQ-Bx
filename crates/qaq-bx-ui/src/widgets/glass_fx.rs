//! 手写 `IGraphicsEffect` / `IGraphicsEffectD2DInterop` 效果节点(阶段 2)。
//!
//! 背景:fork curated winmd 不含 `Microsoft.UI.Composition.Effects` 效果
//! 结构体,且 fork bindgen 没有为结构体生成接口实现的特判;而 composition
//! 效果图(模糊)必须经 `Compositor.CreateEffectFactory(IGraphicsEffect)`
//! ——运行时会 QI `{2FC57384-A068-44D7-A331-30982FCF7177}`
//! (`IGraphicsEffectD2D1Interop`,见 SDK
//! `winrt/windows.graphics.effects.interop.h`)遍历图并读 D2D 属性默认值。
//!
//! 本模块用 fork core 的 `ComCell`(双 vtable 槽位 COM 单元)手搓效果节点:
//! GaussianBlur / Saturation / Composite / Flood(= ColorSource)。IID 与
//! CLSID 依据:
//! - `IGraphicsEffect` = cb51c0ce-…(Windows.Graphics.Effects 元数据);
//! - `IGraphicsEffectSource` = 2d8f9ddc-…(仅运行时 SourceParameter 实现,
//!   本模块只透传其 raw 指针,不定义包装类型);
//! - D2D CLSID 取自 SDK `um/d2d1effects.h` 的 DEFINE_GUID 表。
//!
//! 与 windows-rs 官方 bindgen 特判行为对齐:属性序 = 各 D2D 效果属性表序,
//! `GetNamedPropertyMapping` 不暴露可动画属性(常量烘焙)。

use core::ffi::c_void;
use core::mem::{forget, size_of, transmute_copy};
use core::ptr::null_mut;

use windows_core::imp::{ComCell, Vtable};
use windows_core::{GUID, HSTRING, HRESULT, IUnknown, Interface as _};

use crate::widgets::glass::{IGraphicsEffect as GraphicsEffectParam, PropertyValue};

/// D2D 视图指针(interface 1)到对象基址的偏移恢复。
const SLOT_D2D: usize = 1;
const SLOT_SOURCE: usize = 2;

unsafe fn cell_at(this: *mut c_void, slot: usize) -> *mut FxCell {
    unsafe { (this as *mut u8).sub(slot * size_of::<usize>()) as *mut FxCell }
}

// (define_interface! 生成的包装器;Vtbl 结构体与实现全部在本模块手搓。)
// IGraphicsEffect:Windows.Graphics.Effects 契约接口。
windows_core::imp::define_interface!(
    IGraphicsEffectHand,
    IGraphicsEffectHand_Vtbl,
    0xcb51c0ce_8fe6_4636_b202_861faa07d8f3,
    inspectable
);
// IGraphicsEffectD2D1Interop:ABI 专用接口(元数据无投影,基接口 IUnknown)。
windows_core::imp::define_interface!(
    IGraphicsEffectD2DHand,
    IGraphicsEffectD2DHand_Vtbl,
    0x2fc57384_a068_44d7_a331_30982fcf7177
);
// IGraphicsEffectSource:效果节点同时是其父节点的 source(图遍历契约)。
windows_core::imp::define_interface!(
    IGraphicsEffectSourceHand,
    IGraphicsEffectSourceHand_Vtbl,
    0x2d8f9ddc_4339_4eb9_9216_f9deb75658a2,
    inspectable
);

/// `IGraphicsEffect` 完整 vtbl(元数据契约:Name get/put)。
#[repr(C)]
pub struct IGraphicsEffectHand_Vtbl {
    pub base__: windows_core::IInspectable_Vtbl,
    pub get_name: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
    pub put_name: unsafe extern "system" fn(*mut c_void, *mut c_void) -> HRESULT,
}

/// `IGraphicsEffectD2D1Interop` vtbl(SDK interop 头方法序)。
#[repr(C)]
pub struct IGraphicsEffectD2DHand_Vtbl {
    pub base__: windows_core::IUnknown_Vtbl,
    pub get_effect_id: unsafe extern "system" fn(*mut c_void, *mut GUID) -> HRESULT,
    pub get_named_property_mapping:
        unsafe extern "system" fn(*mut c_void, *mut u16, *mut u32, *mut i32) -> HRESULT,
    pub get_property_count: unsafe extern "system" fn(*mut c_void, *mut u32) -> HRESULT,
    pub get_property: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> HRESULT,
    pub get_source: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> HRESULT,
    pub get_source_count: unsafe extern "system" fn(*mut c_void, *mut u32) -> HRESULT,
}

/// `IGraphicsEffectSource` marker vtbl(无自有方法)。
#[repr(C)]
pub struct IGraphicsEffectSourceHand_Vtbl {
    pub base__: windows_core::IInspectable_Vtbl,
}

/// D2D GaussianBlur(`CLSID_D2D1GaussianBlur`)。
const CLSID_GAUSSIAN_BLUR: GUID = GUID::from_u128(0x1feb6d69_2fe6_4ac9_8c58_1d7f93e7a6a5);
/// D2D Saturation(`CLSID_D2D1Saturation`)。
const CLSID_SATURATION: GUID = GUID::from_u128(0x5cb2d9cf_327d_459f_a0ce_40c0b2086bf7);

const E_POINTER: HRESULT = HRESULT(-2147467261);
const E_FAIL: HRESULT = HRESULT(-2147467259);
const E_BOUNDS: HRESULT = HRESULT(-2147483637);

/// 效果节点属性(D2D 属性表序,类型决定 IPropertyValue 包装)。
///
/// 注:float4 属性(flood color 等)不支持——IPropertyValue 没有向量
/// 包装静态,tint 一律走独立色层形状(见 `glass::build` 双层结构)。
#[derive(Clone, Debug)]
pub enum FxProp {
    F32(f32),
    U32(u32),
}

/// 效果节点数据(vtable 槽位之后)。
pub struct FxNode {
    effect_id: GUID,
    props: Vec<FxProp>,
    /// 图子节点(SourceParameter 运行时对象或其他节点的 IUnknown)。
    sources: Vec<IUnknown>,
}

type FxCell = ComCell<3, (), FxNode>;

/// 效果节点 COM 句柄(持一份引用;Drop 走 Release)。
pub struct FxNodeHandle {
    cell: *mut FxCell,
}

impl FxNodeHandle {
    /// 构造节点并持一份引用(count: 0 → 1)。
    pub fn new(effect_id: GUID, props: Vec<FxProp>, sources: Vec<IUnknown>) -> Self {
        let cell = Box::into_raw(Box::new(unsafe {
            ComCell::new(
                [
                    Vtable::new(&FxNode::GFX_VTABLE),
                    Vtable::new(&FxNode::D2D_VTABLE),
                    Vtable::new(&FxNode::SOURCE_VTABLE),
                ],
                (),
                FxNode {
                    effect_id,
                    props,
                    sources,
                },
            )
        }));
        unsafe {
            (*cell).count().add_ref();
        }
        Self { cell }
    }

    /// 转移一份新引用(供图遍历的 GetSource 透传;compositor 持有)。
    pub fn transfer_source(&self) -> IUnknown {
        unsafe {
            (*self.cell).count().add_ref();
            IUnknown::from_raw(self.cell.cast())
        }
    }

    /// 转 `CreateEffectFactory` 形参(生成 IGraphicsEffect,同 IID QI)。
    pub fn as_graphics_effect(&self) -> windows_core::Result<GraphicsEffectParam> {
        unsafe {
            // 临时引用交给包装器(其 Drop 释放),QI 产生的引用归调用方。
            (*self.cell).count().add_ref();
            let unknown = IUnknown::from_raw(self.cell.cast());
            unknown.cast::<GraphicsEffectParam>()
        }
    }
}

impl Drop for FxNodeHandle {
    fn drop(&mut self) {
        unsafe {
            let cell = self.cell;
            let remaining = (*cell).count().release();
            if remaining == 0 {
                drop(Box::from_raw(cell));
            }
        }
    }
}

impl FxNode {
    const GFX_VTABLE: IGraphicsEffectHand_Vtbl = IGraphicsEffectHand_Vtbl {
        base__: windows_core::IInspectable_Vtbl {
            base: windows_core::IUnknown_Vtbl {
                QueryInterface: Self::gfx_query_interface,
                AddRef: Self::gfx_add_ref,
                Release: Self::gfx_release,
            },
            GetIids: Self::get_iids,
            GetRuntimeClassName: Self::get_runtime_class_name,
            GetTrustLevel: Self::get_trust_level,
        },
        get_name: Self::gfx_get_name,
        put_name: Self::gfx_put_name,
    };

    const D2D_VTABLE: IGraphicsEffectD2DHand_Vtbl = IGraphicsEffectD2DHand_Vtbl {
        base__: windows_core::IUnknown_Vtbl {
            QueryInterface: Self::d2d_query_interface,
            AddRef: Self::d2d_add_ref,
            Release: Self::d2d_release,
        },
        get_effect_id: Self::get_effect_id,
        get_named_property_mapping: Self::get_named_property_mapping,
        get_property_count: Self::get_property_count,
        get_property: Self::get_property,
        get_source: Self::get_source,
        get_source_count: Self::get_source_count,
    };

    const SOURCE_VTABLE: IGraphicsEffectSourceHand_Vtbl = IGraphicsEffectSourceHand_Vtbl {
        base__: windows_core::IInspectable_Vtbl {
            base: windows_core::IUnknown_Vtbl {
                QueryInterface: Self::source_query_interface,
                AddRef: Self::source_add_ref,
                Release: Self::source_release,
            },
            GetIids: Self::get_iids,
            GetRuntimeClassName: Self::get_runtime_class_name,
            GetTrustLevel: Self::get_trust_level,
        },
    };

    /// IID → vtable 槽位(Identity 自身 IID 也要映射;ComCell 不代查)。
    fn find_slot(iid: &GUID) -> Option<usize> {
        let slot = if *iid == IGraphicsEffectHand::IID {
            Some(0)
        } else if *iid == IGraphicsEffectD2DHand::IID {
            Some(1)
        } else if *iid == IGraphicsEffectSourceHand::IID {
            Some(2)
        } else {
            None
        };
        slot
    }

    /// 图效果接口视图(this = 对象基址 = interface 0)。
    unsafe extern "system" fn gfx_query_interface(
        this: *mut c_void,
        iid: *const GUID,
        interface: *mut *mut c_void,
    ) -> HRESULT {
        unsafe {
            let cell = &*(this as *const FxCell);
            cell.query_interface::<IGraphicsEffectHand>(iid, interface, true, None, Self::find_slot)
        }
    }

    unsafe extern "system" fn gfx_add_ref(this: *mut c_void) -> u32 {
        unsafe { (*(this as *mut FxCell)).count().add_ref() }
    }

    unsafe extern "system" fn gfx_release(this: *mut c_void) -> u32 {
        unsafe {
            let cell = this as *mut FxCell;
            let remaining = (*cell).count().release();
            if remaining == 0 {
                drop(Box::from_raw(cell));
            }
            remaining
        }
    }

    unsafe extern "system" fn gfx_get_name(_this: *mut c_void, value: *mut *mut c_void) -> HRESULT {
        unsafe {
            if value.is_null() {
                return E_POINTER;
            }
            // 节点名不参与图匹配(SourceParameter 才需要),恒回空串。
            let name = HSTRING::new();
            value.write(transmute_copy(&name));
            forget(name);
            HRESULT(0)
        }
    }

    unsafe extern "system" fn gfx_put_name(_this: *mut c_void, _name: *mut c_void) -> HRESULT {
        // 调用方拥有入参字符串;节点名不参与匹配,忽略即可。
        HRESULT(0)
    }

    /// D2D 互操作视图(this = interface 1 = 基址 + 一个槽位)。
    unsafe extern "system" fn d2d_query_interface(
        this: *mut c_void,
        iid: *const GUID,
        interface: *mut *mut c_void,
    ) -> HRESULT {
        unsafe {
            let cell = &*cell_at(this, SLOT_D2D);
            cell.query_interface::<IGraphicsEffectHand>(iid, interface, true, None, Self::find_slot)
        }
    }

    unsafe extern "system" fn d2d_add_ref(this: *mut c_void) -> u32 {
        unsafe { (*cell_at(this, SLOT_D2D)).count().add_ref() }
    }

    unsafe extern "system" fn d2d_release(this: *mut c_void) -> u32 {
        unsafe {
            let cell = cell_at(this, SLOT_D2D);
            let remaining = (*cell).count().release();
            if remaining == 0 {
                drop(Box::from_raw(cell));
            }
            remaining
        }
    }

    /// Source 视图(this = interface 2 = 基址 + 两个槽位)。
    unsafe extern "system" fn source_query_interface(
        this: *mut c_void,
        iid: *const GUID,
        interface: *mut *mut c_void,
    ) -> HRESULT {
        unsafe {
            let cell = &*cell_at(this, SLOT_SOURCE);
            cell.query_interface::<IGraphicsEffectHand>(iid, interface, true, None, Self::find_slot)
        }
    }

    unsafe extern "system" fn source_add_ref(this: *mut c_void) -> u32 {
        unsafe { (*cell_at(this, SLOT_SOURCE)).count().add_ref() }
    }

    unsafe extern "system" fn source_release(this: *mut c_void) -> u32 {
        unsafe {
            let cell = cell_at(this, SLOT_SOURCE);
            let remaining = (*cell).count().release();
            if remaining == 0 {
                drop(Box::from_raw(cell));
            }
            remaining
        }
    }

    unsafe extern "system" fn get_effect_id(this: *mut c_void, id: *mut GUID) -> HRESULT {
        unsafe {
            if id.is_null() {
                return E_POINTER;
            }
            id.write((*ComCell::this_ptr(cell_at(this, SLOT_D2D))).effect_id);
            HRESULT(0)
        }
    }

    unsafe extern "system" fn get_named_property_mapping(
        _this: *mut c_void,
        _name: *mut u16,
        _index: *mut u32,
        _mapping: *mut i32,
    ) -> HRESULT {
        // 常量烘焙:不暴露可动画属性(未传属性名时 compositor 不调用)。
        E_FAIL
    }

    unsafe extern "system" fn get_property_count(this: *mut c_void, count: *mut u32) -> HRESULT {
        unsafe {
            if count.is_null() {
                return E_POINTER;
            }
            count.write(
                (*ComCell::this_ptr(cell_at(this, SLOT_D2D))).props.len() as u32,
            );
            HRESULT(0)
        }
    }

    unsafe extern "system" fn get_property(
        this: *mut c_void,
        index: u32,
        value: *mut *mut c_void,
    ) -> HRESULT {
        unsafe {
            if value.is_null() {
                return E_POINTER;
            }
            let node = &*ComCell::this_ptr(cell_at(this, SLOT_D2D));
            let Some(prop) = node.props.get(index as usize) else {
                return E_BOUNDS;
            };
            let inspectable = match prop {
                FxProp::F32(v) => PropertyValue::CreateSingle(*v),
                FxProp::U32(v) => PropertyValue::CreateUInt32(*v),
            };
            let Ok(inspectable) = inspectable else {
                eprintln!("[glass-fx] GetProperty({index}) -> Create* failed");
                return E_FAIL;
            };
            value.write(inspectable.as_raw());
            forget(inspectable);
            HRESULT(0)
        }
    }

    unsafe extern "system" fn get_source(
        this: *mut c_void,
        index: u32,
        value: *mut *mut c_void,
    ) -> HRESULT {
        unsafe {
            if value.is_null() {
                return E_POINTER;
            }
            let node = &*ComCell::this_ptr(cell_at(this, SLOT_D2D));
            let Some(source) = node.sources.get(index as usize) else {
                return E_BOUNDS;
            };
            let source = source.clone();
            value.write(source.as_raw());
            forget(source);
            HRESULT(0)
        }
    }

    unsafe extern "system" fn get_source_count(this: *mut c_void, count: *mut u32) -> HRESULT {
        unsafe {
            if count.is_null() {
                return E_POINTER;
            }
            count.write(
                (*ComCell::this_ptr(cell_at(this, SLOT_D2D)))
                    .sources
                    .len() as u32,
            );
            HRESULT(0)
        }
    }

    unsafe extern "system" fn get_iids(
        _: *mut c_void,
        count: *mut u32,
        values: *mut *mut GUID,
    ) -> HRESULT {
        unsafe {
            if count.is_null() || values.is_null() {
                return E_POINTER;
            }
            count.write(0);
            values.write(null_mut());
            HRESULT(0)
        }
    }

    unsafe extern "system" fn get_runtime_class_name(
        _: *mut c_void,
        value: *mut *mut c_void,
    ) -> HRESULT {
        unsafe {
            if value.is_null() {
                return E_POINTER;
            }
            let name: HSTRING = "Windows.Graphics.Effects.IGraphicsEffect".into();
            value.write(transmute_copy(&name));
            forget(name);
            HRESULT(0)
        }
    }

    unsafe extern "system" fn get_trust_level(_: *mut c_void, value: *mut i32) -> HRESULT {
        unsafe {
            if value.is_null() {
                return E_POINTER;
            }
            value.write(0);
            HRESULT(0)
        }
    }
}

/// D2D 视图指针(interface 1 = 基址 + 一个 vtable 槽位)还原对象基址。

/// 玻璃效果图:blur(saturation(backdrop))。tint 不进图(无 float4 属性
/// 包装,见 `FxProp` 注),由调用方叠加独立色层形状。
/// 返回顶部节点(传给 `CreateEffectFactory`)。
pub fn glass_graph(blur_std_dev: f32, saturation: f32, backdrop: IUnknown) -> FxNodeHandle {
    let saturation_node = FxNodeHandle::new(
        CLSID_SATURATION,
        vec![FxProp::F32(saturation)],
        vec![backdrop],
    );
    // D2D GaussianBlur 属性:0=StandardDeviation(f32),1=Optimization(u32,BALANCED=1),
    // 2=BorderMode(u32,HARD=1,避免边缘拖尾)。
    FxNodeHandle::new(
        CLSID_GAUSSIAN_BLUR,
        vec![FxProp::F32(blur_std_dev), FxProp::U32(1), FxProp::U32(1)],
        vec![saturation_node.transfer_source()],
    )
}
