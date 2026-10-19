//! Minimal LP64 ABI port of nvmedia-probe/iep_compat.h, verified on IEP 1.1.0.
use cu29::prelude::*;
use libloading::Library;
use std::ffi::c_void;

pub(super) type Handle = *mut c_void;
#[repr(C)]
#[derive(Default)]
pub(super) struct Version {
    pub major: u8,
    pub minor: u8,
    pub patch: u8,
}
#[repr(C)]
pub(super) struct Resolution {
    pub width: u16,
    pub height: u16,
}
#[repr(C)]
pub(super) struct Attr {
    pub key: u32,
    pub value: *const c_void,
    pub len: usize,
}
impl Attr {
    pub fn new<T>(key: u32, value: &T) -> Self {
        Self {
            key,
            value: std::ptr::from_ref(value).cast(),
            len: size_of::<T>(),
        }
    }
    pub fn query(key: u32) -> Self {
        Self {
            key,
            value: std::ptr::null(),
            len: 0,
        }
    }
    pub unsafe fn read<T: Copy>(&self) -> CuResult<T> {
        if self.len != size_of::<T>() || self.value.is_null() {
            return Err("Unexpected NvSciBuf attribute layout".into());
        }
        // SAFETY: successful GetAttrs supplies an aligned, live value; size checked above.
        Ok(unsafe { self.value.cast::<T>().read_unaligned() })
    }
}
#[repr(C)]
#[derive(Default)]
pub(super) struct Fence {
    pub payload: [u64; 6],
}
#[repr(C)]
pub(super) struct Bitstream {
    pub data: *mut u8,
    pub used: u32,
    pub capacity: u32,
}
#[repr(C)]
#[derive(Default, Clone, Copy)]
pub(super) struct Qp {
    pub p: i16,
    pub b: i16,
    pub i: i16,
    pub reserved: [i16; 3],
}
#[repr(C)]
#[derive(Default, Clone, Copy)]
pub(super) struct RateControl {
    pub mode: i32,
    pub b_frames: u32,
    // All rate-control union alternatives are integer-only; constant QP zeroes this union.
    pub params: [u32; 7],
    pub constant_frame_qp: bool,
    pub max_session_qp: i8,
    pub reserved: [i8; 3],
}
#[repr(C)]
#[derive(Default)]
pub(super) struct MeCounts {
    pub blocks: [u32; 4],
}
#[repr(C)]
#[derive(Default)]
pub(super) struct InitH265 {
    pub width: u16,
    pub reserved1: u16,
    pub height: u16,
    pub reserved2: u16,
    pub limited_rgb: bool,
    pub slice_level: bool,
    pub fps_num: u32,
    pub fps_den: u32,
    pub profile: u8,
    pub level: u8,
    pub tier: u8,
    pub max_refs: u8,
    pub external_me: bool,
    pub max_me: [MeCounts; 2],
    pub recon_crc: bool,
    pub mvc: bool,
    pub roi: bool,
    pub slice: bool,
    pub b_frames_as_refs: u32,
    pub two_pass: bool,
    pub half_scaled: bool,
    // C bitfields begin at byte 70. disableMemoryOptimization is bit 7 there.
    // The reserved bitfield and C allocation-unit padding occupy bytes 71..74.
    pub flags: [u8; 5],
    pub amp_disable: bool,
}
#[repr(C)]
#[derive(Default)]
pub(super) struct ConfigH265 {
    pub features: u32,
    pub gop: u32,
    pub rc: RateControl,
    pub repeat_headers: i32,
    pub idr_period: u32,
    pub slices_minus_one: u16,
    pub disable_deblock: u8,
    pub weighted_prediction: u8,
    pub intra_refresh_period: u32,
    pub intra_refresh_count: u32,
    pub max_slice_bytes: u32,
    pub ctus_per_slice: u32,
    pub vui: Handle,
    pub initial_qp: Qp,
    pub max_qp: Qp,
    pub preset: i32,
}
#[repr(C)]
#[derive(Default)]
pub(super) struct PictureH265 {
    pub kind: i32,
    pub flags: u32,
    pub next_b_frames: u32,
    pub rc: RateControl,
    pub sei_count: u32,
    pub sei: Handle,
    pub me_counts: [MeCounts; 2],
    pub me_hints: Handle,
    pub fps_num: u32,
    pub fps_den: u32,
    pub view: u32,
}
const _: () = {
    assert!(size_of::<usize>() == 8);
    assert!(size_of::<Version>() == 3);
    assert!(size_of::<Attr>() == 24);
    assert!(size_of::<Fence>() == 48);
    assert!(size_of::<Bitstream>() == 16);
    assert!(size_of::<RateControl>() == 44);
    assert!(size_of::<InitH265>() == 76);
    assert!(std::mem::offset_of!(InitH265, max_me) == 28);
    assert!(std::mem::offset_of!(InitH265, flags) == 70);
    assert!(std::mem::offset_of!(InitH265, amp_disable) == 75);
    assert!(size_of::<ConfigH265>() == 120);
    assert!(std::mem::offset_of!(ConfigH265, vui) == 80);
    assert!(size_of::<PictureH265>() == 128);
    assert!(std::mem::offset_of!(PictureH265, fps_num) == 112);
};

macro_rules! api {
    ($($name:ident: $symbol:ident, $lib:literal, ($($arg:ty),*) -> $ret:ty;)*) => {
        pub(super) struct Api {
            $(pub $name: unsafe extern "C" fn($($arg),*) -> $ret,)*
            _libraries: [Library; 3],
        }
        impl Api {
            pub fn load() -> CuResult<Self> {
                // SAFETY: libraries and ABI are pinned to the tested Delta runtime.
                unsafe {
                    let libraries = [
                        Library::new("libnvmedia_iep_sci.so"),
                        Library::new("libnvscibuf.so"),
                        Library::new("libnvscisync.so"),
                    ];
                    let [iep, buf, sync] = libraries;
                    let libraries = [iep.map_err(load_error)?, buf.map_err(load_error)?, sync.map_err(load_error)?];
                    Ok(Self {
                        $($name: *libraries[$lib].get(concat!(stringify!($symbol), "\0").as_bytes()).map_err(load_error)?,)*
                        _libraries: libraries,
                    })
                }
            }
        }
    }
}
fn load_error(error: libloading::Error) -> CuError {
    CuError::new_with_cause("Cannot load Delta NvMedia/NvSci ABI", error)
}
api! {
    nv_mediaiep_get_version: NvMediaIEPGetVersion, 0, (*mut Version) -> i32;
    nv_mediaiep_fill_nv_sci_buf_attr_list: NvMediaIEPFillNvSciBufAttrList, 0, (i32, Handle) -> i32;
    nv_mediaiep_create: NvMediaIEPCreate, 0, (i32, *const c_void, Handle, u8, i32) -> Handle;
    nv_mediaiep_destroy: NvMediaIEPDestroy, 0, (Handle) -> ();
    nv_mediaiep_set_configuration: NvMediaIEPSetConfiguration, 0, (Handle, *const c_void) -> i32;
    nv_mediaiep_set_attribute: NvMediaIEPSetAttribute, 0, (Handle, i32, u32, *const c_void) -> i32;
    nv_mediaiep_register_nv_sci_buf_obj: NvMediaIEPRegisterNvSciBufObj, 0, (Handle, Handle) -> i32;
    nv_mediaiep_unregister_nv_sci_buf_obj: NvMediaIEPUnregisterNvSciBufObj, 0, (Handle, Handle) -> i32;
    nv_mediaiep_fill_nv_sci_sync_attr_list: NvMediaIEPFillNvSciSyncAttrList, 0, (Handle, Handle, i32) -> i32;
    nv_mediaiep_register_nv_sci_sync_obj: NvMediaIEPRegisterNvSciSyncObj, 0, (Handle, i32, Handle) -> i32;
    nv_mediaiep_unregister_nv_sci_sync_obj: NvMediaIEPUnregisterNvSciSyncObj, 0, (Handle, Handle) -> i32;
    nv_mediaiep_set_nv_sci_sync_objforeof: NvMediaIEPSetNvSciSyncObjforEOF, 0, (Handle, Handle) -> i32;
    nv_mediaiep_insert_pre_nv_sci_sync_fence: NvMediaIEPInsertPreNvSciSyncFence, 0, (Handle, *const Fence) -> i32;
    nv_mediaiep_feed_frame: NvMediaIEPFeedFrame, 0, (Handle, Handle, *const c_void, i32) -> i32;
    nv_mediaiep_geteof_nv_sci_sync_fence: NvMediaIEPGetEOFNvSciSyncFence, 0, (Handle, Handle, *mut Fence) -> i32;
    nv_mediaiep_bits_available: NvMediaIEPBitsAvailable, 0, (Handle, *mut u32, i32, u32) -> i32;
    nv_mediaiep_get_bits: NvMediaIEPGetBits, 0, (Handle, *mut u32, u32, *const Bitstream, *mut c_void) -> i32;
    nv_sci_buf_check_version_compatibility: NvSciBufCheckVersionCompatibility, 1, (u32, u32, *mut bool) -> u32;
    nv_sci_buf_module_open: NvSciBufModuleOpen, 1, (*mut Handle) -> u32;
    nv_sci_buf_module_close: NvSciBufModuleClose, 1, (Handle) -> ();
    nv_sci_buf_attr_list_create: NvSciBufAttrListCreate, 1, (Handle, *mut Handle) -> u32;
    nv_sci_buf_attr_list_free: NvSciBufAttrListFree, 1, (Handle) -> ();
    nv_sci_buf_attr_list_set_attrs: NvSciBufAttrListSetAttrs, 1, (Handle, *mut Attr, usize) -> u32;
    nv_sci_buf_attr_list_get_attrs: NvSciBufAttrListGetAttrs, 1, (Handle, *mut Attr, usize) -> u32;
    nv_sci_buf_attr_list_reconcile: NvSciBufAttrListReconcile, 1, (*const Handle, usize, *mut Handle, *mut Handle) -> u32;
    nv_sci_buf_obj_alloc: NvSciBufObjAlloc, 1, (Handle, *mut Handle) -> u32;
    nv_sci_buf_obj_free: NvSciBufObjFree, 1, (Handle) -> ();
    nv_sci_buf_obj_get_attr_list: NvSciBufObjGetAttrList, 1, (Handle, *mut Handle) -> u32;
    nv_sci_buf_obj_put_pixels: NvSciBufObjPutPixels, 1, (Handle, *const c_void, *mut *const c_void, *const u32, *const u32) -> u32;
    nv_sci_buf_obj_get_pixels: NvSciBufObjGetPixels, 1, (Handle, *const c_void, *mut *mut c_void, *const u32, *const u32) -> u32;
    nv_sci_buf_obj_flush_cpu_cache_range: NvSciBufObjFlushCpuCacheRange, 1, (Handle, u64, u64) -> u32;
    nv_sci_sync_check_version_compatibility: NvSciSyncCheckVersionCompatibility, 2, (u32, u32, *mut bool) -> u32;
    nv_sci_sync_module_open: NvSciSyncModuleOpen, 2, (*mut Handle) -> u32;
    nv_sci_sync_module_close: NvSciSyncModuleClose, 2, (Handle) -> ();
    nv_sci_sync_cpu_wait_context_alloc: NvSciSyncCpuWaitContextAlloc, 2, (Handle, *mut Handle) -> u32;
    nv_sci_sync_cpu_wait_context_free: NvSciSyncCpuWaitContextFree, 2, (Handle) -> ();
    nv_sci_sync_attr_list_create: NvSciSyncAttrListCreate, 2, (Handle, *mut Handle) -> u32;
    nv_sci_sync_attr_list_free: NvSciSyncAttrListFree, 2, (Handle) -> ();
    nv_sci_sync_attr_list_set_attrs: NvSciSyncAttrListSetAttrs, 2, (Handle, *const Attr, usize) -> u32;
    nv_sci_sync_attr_list_reconcile: NvSciSyncAttrListReconcile, 2, (*const Handle, usize, *mut Handle, *mut Handle) -> u32;
    nv_sci_sync_obj_alloc: NvSciSyncObjAlloc, 2, (Handle, *mut Handle) -> u32;
    nv_sci_sync_obj_free: NvSciSyncObjFree, 2, (Handle) -> ();
    nv_sci_sync_obj_generate_fence: NvSciSyncObjGenerateFence, 2, (Handle, *mut Fence) -> u32;
    nv_sci_sync_obj_signal: NvSciSyncObjSignal, 2, (Handle) -> u32;
    nv_sci_sync_fence_wait: NvSciSyncFenceWait, 2, (*const Fence, Handle, i64) -> u32;
    nv_sci_sync_fence_clear: NvSciSyncFenceClear, 2, (*mut Fence) -> ();
}
