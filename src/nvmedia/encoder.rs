//! Synchronous, thread-confined IEP encoder and owned NvSci resources.
use super::ffi::*;
use cu29::prelude::*;
use std::ptr::{null, null_mut};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn check(status: impl Into<i64>, operation: &str) -> CuResult<()> {
    let status = status.into();
    if status == 0 {
        Ok(())
    } else {
        Err(CuError::from(format!("{operation}: status 0x{status:08x}")))
    }
}
macro_rules! call {
    ($api:expr, $name:ident ($($args:expr),* $(,)?)) => {
        check(($api.$name)($($args),*), stringify!($name))
    };
}

struct AttrLists<'a> {
    api: &'a Api,
    sync: bool,
    lists: [Handle; 4],
}
impl Drop for AttrLists<'_> {
    fn drop(&mut self) {
        for &list in &self.lists {
            if !list.is_null() {
                // SAFETY: these lists were created by this API and are no longer used.
                unsafe {
                    if self.sync {
                        (self.api.nv_sci_sync_attr_list_free)(list);
                    } else {
                        (self.api.nv_sci_buf_attr_list_free)(list);
                    }
                }
            }
        }
    }
}

struct Surface {
    api: Arc<Api>,
    object: Handle,
    attrs: Handle,
    bytes: u64,
    flush: bool,
}
impl Surface {
    fn new(api: Arc<Api>, module: Handle, width: u32, height: u32) -> CuResult<Self> {
        let mut surface = Self {
            api: api.clone(),
            object: null_mut(),
            attrs: null_mut(),
            bytes: 0,
            flush: false,
        };
        let mut lists = AttrLists {
            api: &api,
            sync: false,
            lists: [null_mut(); 4],
        };
        // SAFETY: scalar attributes and lists remain live through reconciliation/allocation.
        unsafe {
            call!(
                api,
                nv_sci_buf_attr_list_create(module, &mut lists.lists[0])
            )?;
            call!(
                api,
                nv_mediaiep_fill_nv_sci_buf_attr_list(0, lists.lists[0])
            )?;
            let image_type = 2u32;
            let permission = 3u32;
            let block_linear = 0u32;
            let progressive = 0u32;
            let count = 1u64;
            let yuv = 0u32;
            let semiplanar = 1u32;
            let sample444 = 2u32;
            let bpc8 = 2u32;
            let order = 0u32;
            let rec709 = 4u32;
            let cpu = true;
            let cache = true;
            let mut attrs = [
                Attr::new(1, &image_type),
                Attr::new(2, &cpu),
                Attr::new(3, &permission),
                Attr::new(4, &cache),
                Attr::new(0x20000, &block_linear),
                Attr::new(0x2000e, &progressive),
                Attr::new(0x20017, &count),
                Attr::new(0x20018, &yuv),
                Attr::new(0x20019, &semiplanar),
                Attr::new(0x2001a, &sample444),
                Attr::new(0x2001b, &bpc8),
                Attr::new(0x2001c, &order),
                Attr::new(0x2001d, &width),
                Attr::new(0x2001e, &height),
                Attr::new(0x2001f, &rec709),
            ];
            call!(
                api,
                nv_sci_buf_attr_list_set_attrs(lists.lists[0], attrs.as_mut_ptr(), attrs.len())
            )?;
            let mut reconciled = null_mut();
            let mut conflict = null_mut();
            let status = (api.nv_sci_buf_attr_list_reconcile)(
                &lists.lists[0],
                1,
                &mut reconciled,
                &mut conflict,
            );
            lists.lists[2] = reconciled;
            lists.lists[3] = conflict;
            check(status, "NvSciBufAttrListReconcile")?;
            call!(api, nv_sci_buf_obj_alloc(reconciled, &mut surface.object))?;
            call!(
                api,
                nv_sci_buf_obj_get_attr_list(surface.object, &mut surface.attrs)
            )?;
            let mut query = [
                Attr::query(0x20006),
                Attr::query(6),
                Attr::query(0x20008),
                Attr::query(0x20009),
                Attr::query(0x2000c),
                Attr::query(0x2000d),
            ];
            call!(
                api,
                nv_sci_buf_attr_list_get_attrs(surface.attrs, query.as_mut_ptr(), query.len())
            )?;
            surface.bytes = query[0].read()?;
            surface.flush = query[1].read()?;
            if query[2].read::<u32>()? != 2
                || query[3].read::<[u32; 2]>()? != [0x54, 0x4c]
                || query[4].read::<[u32; 2]>()? != [width; 2]
                || query[5].read::<[u32; 2]>()? != [height; 2]
            {
                return Err("Expected block-linear 8-bit YUV444 NvSciBuf surface".into());
            }
        }
        Ok(surface)
    }
}
impl Drop for Surface {
    fn drop(&mut self) {
        if !self.object.is_null() {
            // SAFETY: Encoder drops/unregisters before releasing its surfaces.
            unsafe {
                (self.api.nv_sci_buf_obj_free)(self.object);
            }
        }
    }
}

/// Last-frame wall time spent uploading, waiting for hardware, and retrieving output.
#[derive(Clone, Copy, Debug, Default)]
pub struct EncodeTimings {
    /// CPU-to-NvSciBuf conversion and cache maintenance.
    pub upload: Duration,
    /// Submission through EOF completion.
    pub encode: Duration,
    /// Output query, retrieval, and copy into the caller's buffer.
    pub retrieve: Duration,
}

/// Lossless 1080p YUV444 encoder using the tested 4K reservation and early DRC.
/// Construct and use it on one worker thread; no NVIDIA headers are required.
pub struct Encoder {
    api: Arc<Api>,
    module: Handle,
    sync_module: Handle,
    iep: Handle,
    surface: Option<Surface>,
    reserve: Option<Surface>,
    pre: Handle,
    eof: Handle,
    waiter: Handle,
    pre_fence: Fence,
    eof_fence: Fence,
    image_registered: bool,
    pre_registered: bool,
    eof_registered: bool,
    pending: bool,
    poisoned: bool,
    fps: u32,
    output: Vec<u8>,
    timings: EncodeTimings,
}
impl Encoder {
    /// Allocate at 3840×2160, disable memory optimization, then select 1920×1080.
    pub fn new(fps: u32) -> CuResult<Self> {
        if !(1..=30).contains(&fps) {
            return Err("Frame rate must be 1..=30".into());
        }
        let api = Arc::new(Api::load()?);
        let mut encoder = Self {
            api: api.clone(),
            module: null_mut(),
            sync_module: null_mut(),
            iep: null_mut(),
            surface: None,
            reserve: None,
            pre: null_mut(),
            eof: null_mut(),
            waiter: null_mut(),
            pre_fence: Fence::default(),
            eof_fence: Fence::default(),
            image_registered: false,
            pre_registered: false,
            eof_registered: false,
            pending: false,
            poisoned: false,
            fps,
            output: vec![0; 32 * 1024 * 1024],
            timings: EncodeTimings::default(),
        };
        // SAFETY: FFI structs match the checked LP64 ABI; all handles are encoder-owned.
        unsafe {
            let mut version = Version::default();
            call!(api, nv_mediaiep_get_version(&mut version))?;
            if (version.major, version.minor, version.patch) != (1, 1, 0) {
                return Err("Expected NvMedia IEP 1.1.0".into());
            }
            let mut buf_ok = false;
            let mut sync_ok = false;
            call!(
                api,
                nv_sci_buf_check_version_compatibility(2, 11, &mut buf_ok)
            )?;
            call!(
                api,
                nv_sci_sync_check_version_compatibility(2, 8, &mut sync_ok)
            )?;
            if !buf_ok || !sync_ok {
                return Err("Incompatible NvSciBuf 2.11 / NvSciSync 2.8".into());
            }
            call!(api, nv_sci_buf_module_open(&mut encoder.module))?;
            encoder.surface = Some(Surface::new(api.clone(), encoder.module, 1920, 1080)?);
            let reserve = Surface::new(api.clone(), encoder.module, 3840, 2160)?;
            let init = InitH265 {
                width: 3840,
                height: 2160,
                fps_num: fps,
                fps_den: 1,
                profile: 4,
                max_refs: 1,
                flags: [0x80, 0, 0, 0, 0],
                ..Default::default()
            };
            encoder.iep =
                (api.nv_mediaiep_create)(1, std::ptr::from_ref(&init).cast(), reserve.attrs, 4, 0);
            encoder.reserve = Some(reserve);
            if encoder.iep.is_null() {
                return Err("NvMediaIEPCreate failed".into());
            }
            // Ordering is essential: DRC BEFORE SetConfiguration retains the large buffers
            // while configuring the bitstream and input surfaces for actual 1080p.
            let resolution = Resolution {
                width: 1920,
                height: 1080,
            };
            call!(
                api,
                nv_mediaiep_set_attribute(
                    encoder.iep,
                    0,
                    size_of::<Resolution>() as u32,
                    std::ptr::from_ref(&resolution).cast()
                )
            )?;
            let config = ConfigH265 {
                features: 1 | (1 << 4),
                gop: 1,
                rc: Self::rate_control(),
                repeat_headers: 2,
                idr_period: 1,
                preset: 0x20,
                ..Default::default()
            };
            call!(
                api,
                nv_mediaiep_set_configuration(encoder.iep, std::ptr::from_ref(&config).cast())
            )?;
            let image = encoder
                .surface
                .as_ref()
                .ok_or(CuError::from("Missing input surface"))?
                .object;
            call!(api, nv_mediaiep_register_nv_sci_buf_obj(encoder.iep, image))?;
            encoder.image_registered = true;
            call!(api, nv_sci_sync_module_open(&mut encoder.sync_module))?;
            call!(
                api,
                nv_sci_sync_cpu_wait_context_alloc(encoder.sync_module, &mut encoder.waiter)
            )?;
            encoder.pre = encoder.make_sync(false)?;
            encoder.eof = encoder.make_sync(true)?;
            call!(
                api,
                nv_mediaiep_register_nv_sci_sync_obj(encoder.iep, 0, encoder.pre)
            )?;
            encoder.pre_registered = true;
            call!(
                api,
                nv_mediaiep_register_nv_sci_sync_obj(encoder.iep, 1, encoder.eof)
            )?;
            encoder.eof_registered = true;
            call!(
                api,
                nv_mediaiep_set_nv_sci_sync_objforeof(encoder.iep, encoder.eof)
            )?;
        }
        Ok(encoder)
    }
    /// Timings of the last successful frame, for upload/encode throughput diagnosis.
    pub fn last_timings(&self) -> EncodeTimings {
        self.timings
    }

    fn rate_control() -> RateControl {
        RateControl {
            mode: 1,
            max_session_qp: 51,
            ..Default::default()
        }
    }
    fn make_sync(&self, eof: bool) -> CuResult<Handle> {
        let api = &self.api;
        let mut lists = AttrLists {
            api,
            sync: true,
            lists: [null_mut(); 4],
        };
        // SAFETY: all temporary lists and scalar attributes live through allocation.
        unsafe {
            call!(
                api,
                nv_sci_sync_attr_list_create(self.sync_module, &mut lists.lists[0])
            )?;
            call!(
                api,
                nv_sci_sync_attr_list_create(self.sync_module, &mut lists.lists[1])
            )?;
            call!(
                api,
                nv_mediaiep_fill_nv_sci_sync_attr_list(
                    self.iep,
                    lists.lists[0],
                    if eof { 0 } else { 1 }
                )
            )?;
            let cpu = true;
            let permission = if eof { 1u64 } else { 2u64 };
            let attrs = [Attr::new(1, &cpu), Attr::new(2, &permission)];
            call!(
                api,
                nv_sci_sync_attr_list_set_attrs(lists.lists[1], attrs.as_ptr(), attrs.len())
            )?;
            let mut reconciled = null_mut();
            let mut conflict = null_mut();
            let status = (api.nv_sci_sync_attr_list_reconcile)(
                lists.lists.as_ptr(),
                2,
                &mut reconciled,
                &mut conflict,
            );
            lists.lists[2] = reconciled;
            lists.lists[3] = conflict;
            check(status, "NvSciSyncAttrListReconcile")?;
            let mut object = null_mut();
            call!(api, nv_sci_sync_obj_alloc(reconciled, &mut object))?;
            Ok(object)
        }
    }
    fn transfer(&self, data: &mut [u8], upload: bool) -> CuResult<()> {
        if data.len() != 1920 * 1080 * 3 {
            return Err("Expected one tightly packed 1920×1080 YUV444 frame".into());
        }
        let surface = self
            .surface
            .as_ref()
            .ok_or(CuError::from("Missing input surface"))?;
        let plane = 1920 * 1080;
        let sizes = [plane as u32; 3];
        let pitches = [1920; 3];
        // SAFETY: three disjoint, fully sized planes remain borrowed for the FFI call.
        unsafe {
            if upload {
                let mut sources = [
                    data.as_ptr().cast(),
                    data.as_ptr().add(plane).cast(),
                    data.as_ptr().add(2 * plane).cast(),
                ];
                call!(
                    self.api,
                    nv_sci_buf_obj_put_pixels(
                        surface.object,
                        null(),
                        sources.as_mut_ptr(),
                        sizes.as_ptr(),
                        pitches.as_ptr()
                    )
                )?;
                if surface.flush {
                    call!(
                        self.api,
                        nv_sci_buf_obj_flush_cpu_cache_range(surface.object, 0, surface.bytes)
                    )?;
                }
            } else {
                let mut targets = [
                    data.as_mut_ptr().cast(),
                    data.as_mut_ptr().add(plane).cast(),
                    data.as_mut_ptr().add(2 * plane).cast(),
                ];
                call!(
                    self.api,
                    nv_sci_buf_obj_get_pixels(
                        surface.object,
                        null(),
                        targets.as_mut_ptr(),
                        sizes.as_ptr(),
                        pitches.as_ptr()
                    )
                )?;
            }
        }
        Ok(())
    }
    /// Copy the last input surface back for byte-exact validation, after encoding finishes.
    pub fn read_input(&self, output: &mut [u8]) -> CuResult<()> {
        if self.pending || self.poisoned {
            return Err("Encoder failed; surface readback is unavailable".into());
        }
        self.transfer(output, false)
    }
    /// Encode one planar frame into `output`. Every frame is an IDR with parameter sets.
    /// The caller owns the resulting bytes; IEP never retains this output buffer.
    pub fn encode(&mut self, frame: &mut [u8], output: &mut Vec<u8>) -> CuResult<()> {
        if self.poisoned {
            return Err("Encoder cannot be reused after an error".into());
        }
        self.poisoned = true;
        let started = Instant::now();
        self.transfer(frame, true)?;
        let uploaded = Instant::now();
        // SAFETY: no second submission is possible until EOF has completed; fences and
        // surface remain owned by self even if a wait or driver call fails.
        unsafe {
            call!(
                self.api,
                nv_sci_sync_obj_generate_fence(self.pre, &mut self.pre_fence)
            )?;
            call!(
                self.api,
                nv_mediaiep_insert_pre_nv_sci_sync_fence(self.iep, &self.pre_fence)
            )?;
            call!(self.api, nv_sci_sync_obj_signal(self.pre))?;
            let picture = PictureH265 {
                kind: 4,
                flags: 1,
                rc: Self::rate_control(),
                fps_num: self.fps,
                fps_den: 1,
                ..Default::default()
            };
            // Treat even an unsuccessful submission as potentially asynchronous.
            self.pending = true;
            call!(
                self.api,
                nv_mediaiep_feed_frame(
                    self.iep,
                    self.surface
                        .as_ref()
                        .ok_or(CuError::from("Missing input surface"))?
                        .object,
                    std::ptr::from_ref(&picture).cast(),
                    0
                )
            )?;
            call!(
                self.api,
                nv_mediaiep_geteof_nv_sci_sync_fence(self.iep, self.eof, &mut self.eof_fence)
            )?;
            call!(
                self.api,
                nv_sci_sync_fence_wait(&self.eof_fence, self.waiter, 10_000_000)
            )?;
            self.pending = false;
            let encoded = Instant::now();
            let mut available = 0;
            let mut used = 0;
            call!(
                self.api,
                nv_mediaiep_bits_available(self.iep, &mut available, 1, 10_000)
            )?;
            if available == 0 || available as usize > self.output.len() {
                return Err("IEP bitstream exceeds the 32 MiB output buffer".into());
            }
            let stream = Bitstream {
                data: self.output.as_mut_ptr(),
                used: 0,
                capacity: self.output.len() as u32,
            };
            call!(
                self.api,
                nv_mediaiep_get_bits(self.iep, &mut used, 1, &stream, null_mut())
            )?;
            if used != available
                || used as usize > self.output.len()
                || (stream.used != 0 && stream.used != used)
            {
                return Err("IEP returned inconsistent bitstream length".into());
            }
            output.clear();
            output.extend_from_slice(&self.output[..used as usize]);
            (self.api.nv_sci_sync_fence_clear)(&mut self.pre_fence);
            (self.api.nv_sci_sync_fence_clear)(&mut self.eof_fence);
            self.timings = EncodeTimings {
                upload: uploaded - started,
                encode: encoded - uploaded,
                retrieve: encoded.elapsed(),
            };
        }
        self.poisoned = false;
        Ok(())
    }
}
impl Drop for Encoder {
    fn drop(&mut self) {
        if self.pending {
            // There is no documented cancel operation. Preserve potentially live GPU
            // objects AND their libraries until process exit instead of freeing them.
            std::mem::forget(self.api.clone());
            if let Some(surface) = self.surface.take() {
                std::mem::forget(surface);
            }
            if let Some(reserve) = self.reserve.take() {
                std::mem::forget(reserve);
            }
            return;
        }
        // SAFETY: EOF completed (or nothing was submitted); release in dependency order.
        unsafe {
            if self.image_registered
                && let Some(surface) = &self.surface
            {
                (self.api.nv_mediaiep_unregister_nv_sci_buf_obj)(self.iep, surface.object);
            }
            if self.pre_registered {
                (self.api.nv_mediaiep_unregister_nv_sci_sync_obj)(self.iep, self.pre);
            }
            if self.eof_registered {
                (self.api.nv_mediaiep_unregister_nv_sci_sync_obj)(self.iep, self.eof);
            }
            (self.api.nv_sci_sync_fence_clear)(&mut self.pre_fence);
            (self.api.nv_sci_sync_fence_clear)(&mut self.eof_fence);
            if !self.iep.is_null() {
                (self.api.nv_mediaiep_destroy)(self.iep);
            }
            if !self.pre.is_null() {
                (self.api.nv_sci_sync_obj_free)(self.pre);
            }
            if !self.eof.is_null() {
                (self.api.nv_sci_sync_obj_free)(self.eof);
            }
            if !self.waiter.is_null() {
                (self.api.nv_sci_sync_cpu_wait_context_free)(self.waiter);
            }
            if !self.sync_module.is_null() {
                (self.api.nv_sci_sync_module_close)(self.sync_module);
            }
            self.surface.take();
            self.reserve.take();
            if !self.module.is_null() {
                (self.api.nv_sci_buf_module_close)(self.module);
            }
        }
    }
}
