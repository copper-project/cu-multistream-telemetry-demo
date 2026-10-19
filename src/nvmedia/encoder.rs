//! Queued, thread-confined IEP encoder and owned shared GPU resources.
use super::ffi::*;
use super::gpu::{Gpu, Image};
use cu29::prelude::*;
use std::collections::VecDeque;
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
}
impl Surface {
    fn new(
        api: Arc<Api>,
        module: Handle,
        width: u32,
        height: u32,
        gpu: &[u8; 16],
    ) -> CuResult<Self> {
        let mut surface = Self {
            api: api.clone(),
            object: null_mut(),
            attrs: null_mut(),
            bytes: 0,
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
                Attr::new(5, gpu),
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
            if query[1].read::<u32>()? != 2
                || query[2].read::<[u32; 2]>()? != [0x54, 0x4c]
                || query[3].read::<[u32; 2]>()? != [width; 2]
                || query[4].read::<[u32; 2]>()? != [height; 2]
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
    /// Host time importing/launching CUDA conversion and signaling its pre-fence.
    pub upload: Duration,
    /// Time from submission until collection (includes queue residence).
    pub encode: Duration,
    /// Output query, allocation, and direct retrieval.
    pub retrieve: Duration,
}

struct Slot {
    surface: Surface,
    image: Option<Image>,
    pre_fence: Fence,
    eof_fence: Fence,
    registered: bool,
    sample: Option<gstreamer::Sample>,
    started: Instant,
    uploaded: Instant,
}

/// Lossless 1080p YUV444 encoder with three shared GPU surfaces in flight.
/// Construct and use on one worker thread; no NVIDIA headers are required.
pub struct Encoder {
    api: Arc<Api>,
    gpu: Option<Gpu>,
    slots: Vec<Slot>,
    queue: VecDeque<usize>,
    next_slot: usize,
    last_slot: Option<usize>,
    module: Handle,
    sync_module: Handle,
    iep: Handle,
    reserve: Option<Surface>,
    pre: Handle,
    eof: Handle,
    waiter: Handle,
    pre_registered: bool,
    eof_registered: bool,
    pending: bool,
    poisoned: bool,
    fps: u32,
    timings: EncodeTimings,
}
impl Encoder {
    /// Allocate at 3840×2160, disable memory optimization, then select 1920×1080.
    pub fn new(fps: u32) -> CuResult<Self> {
        if !(1..=30).contains(&fps) {
            return Err("Frame rate must be 1..=30".into());
        }
        let api = Arc::new(Api::load()?);
        let gpu = Gpu::new()?;
        let uuid = gpu.uuid;
        let mut encoder = Self {
            api: api.clone(),
            gpu: Some(gpu),
            slots: Vec::with_capacity(3),
            queue: VecDeque::with_capacity(3),
            next_slot: 0,
            last_slot: None,
            module: null_mut(),
            sync_module: null_mut(),
            iep: null_mut(),
            reserve: None,
            pre: null_mut(),
            eof: null_mut(),
            waiter: null_mut(),
            pre_registered: false,
            eof_registered: false,
            pending: false,
            poisoned: false,
            fps,
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
            for _ in 0..3 {
                encoder.slots.push(Slot {
                    surface: Surface::new(api.clone(), encoder.module, 1920, 1080, &uuid)?,
                    image: None,
                    pre_fence: Fence::default(),
                    eof_fence: Fence::default(),
                    registered: false,
                    sample: None,
                    started: Instant::now(),
                    uploaded: Instant::now(),
                });
            }
            let reserve = Surface::new(api.clone(), encoder.module, 3840, 2160, &uuid)?;
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
                gop: fps,
                rc: Self::rate_control(),
                repeat_headers: 2,
                idr_period: fps,
                preset: 0x20,
                ..Default::default()
            };
            call!(
                api,
                nv_mediaiep_set_configuration(encoder.iep, std::ptr::from_ref(&config).cast())
            )?;
            for slot in &mut encoder.slots {
                call!(
                    api,
                    nv_mediaiep_register_nv_sci_buf_obj(encoder.iep, slot.surface.object)
                )?;
                slot.registered = true;
            }
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
        let gpu = encoder
            .gpu
            .as_mut()
            .ok_or(CuError::from("Missing CUDA context"))?;
        gpu.semaphore(encoder.pre)?;
        for slot in &mut encoder.slots {
            let surface = &slot.surface;
            let mut attrs = [
                Attr::query(0x20010),
                Attr::query(0x20014),
                Attr::query(0x2000d),
            ];
            // SAFETY: live reconciled attribute list, checked typed array lengths.
            unsafe {
                call!(
                    api,
                    nv_sci_buf_attr_list_get_attrs(surface.attrs, attrs.as_mut_ptr(), attrs.len())
                )?;
                slot.image = Some(gpu.image(
                    surface.object,
                    surface.bytes,
                    attrs[0].read()?,
                    attrs[1].read()?,
                    attrs[2].read()?,
                )?);
            }
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
            if eof {
                let cpu = true;
                let permission = 1u64;
                let attrs = [Attr::new(1, &cpu), Attr::new(2, &permission)];
                call!(
                    api,
                    nv_sci_sync_attr_list_set_attrs(lists.lists[1], attrs.as_ptr(), attrs.len())
                )?;
            } else {
                self.gpu
                    .as_ref()
                    .ok_or(CuError::from("Missing CUDA context"))?
                    .sync_attrs(lists.lists[1])?;
            }
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
    /// Copy the last completed input surface back for byte-exact validation only.
    /// Call before submitting another frame, which can reuse that surface.
    pub fn read_input(&self, output: &mut [u8]) -> CuResult<()> {
        if self.poisoned || output.len() != 1920 * 1080 * 3 {
            return Err("Input readback requires a healthy encoder and one Y444 frame".into());
        }
        let index = self.last_slot.ok_or(CuError::from("No completed frame"))?;
        if self.queue.contains(&index) {
            return Err("Last completed surface has been reused".into());
        }
        let plane = 1920 * 1080;
        let sizes = [plane as u32; 3];
        let pitches = [1920; 3];
        // SAFETY: surface EOF completed, three disjoint caller-owned output planes.
        unsafe {
            let mut targets = [
                output.as_mut_ptr().cast(),
                output.as_mut_ptr().add(plane).cast(),
                output.as_mut_ptr().add(2 * plane).cast(),
            ];
            call!(
                self.api,
                nv_sci_buf_obj_get_pixels(
                    self.slots[index].surface.object,
                    null(),
                    targets.as_mut_ptr(),
                    sizes.as_ptr(),
                    pitches.as_ptr()
                )
            )?;
        }
        Ok(())
    }
    /// Number of submitted frames whose compressed output has not been collected.
    pub fn queued(&self) -> usize {
        self.queue.len()
    }
    /// Maximum number of outstanding frames; input memory is bounded.
    pub fn capacity(&self) -> usize {
        self.slots.len()
    }

    fn begin(&mut self) -> CuResult<()> {
        if self.poisoned {
            return Err("Encoder cannot be reused after an error".into());
        }
        if self.queue.len() == self.slots.len() {
            return Err("Collect an encoded frame before submitting another".into());
        }
        self.poisoned = true;
        self.pending = true;
        self.slots[self.next_slot].started = Instant::now();
        Ok(())
    }
    /// Release capture buffers once CUDA finishes reading, independently of NVENC.
    pub fn reclaim_inputs(&mut self) -> CuResult<()> {
        if self.poisoned {
            return Err("Encoder cannot be reused after an error".into());
        }
        self.poisoned = true;
        let gpu = self
            .gpu
            .as_ref()
            .ok_or(CuError::from("Missing CUDA context"))?;
        for slot in &mut self.slots {
            if slot.sample.is_some()
                && gpu.converted(
                    slot.image
                        .as_ref()
                        .ok_or(CuError::from("Missing CUDA image"))?,
                )?
            {
                slot.sample.take();
            }
        }
        self.poisoned = false;
        Ok(())
    }
    /// Submit native UYVY DMA memory, retaining the sample until CUDA finishes reading.
    pub fn submit_sample(&mut self, sample: &gstreamer::Sample) -> CuResult<()> {
        self.begin()?;
        let slot = &mut self.slots[self.next_slot];
        slot.sample = Some(sample.clone());
        self.gpu
            .as_mut()
            .ok_or(CuError::from("Missing CUDA context"))?
            .sample(
                slot.image
                    .as_ref()
                    .ok_or(CuError::from("Missing CUDA image"))?,
                sample,
                &mut slot.pre_fence,
            )?;
        self.submit()
    }
    /// Encode one planar validation frame through the same GPU surfaces and fences.
    pub fn encode(&mut self, frame: &mut [u8], output: &mut Arc<[u8]>) -> CuResult<()> {
        if !self.queue.is_empty() {
            return Err("Drain queued camera frames before planar validation".into());
        }
        self.begin()?;
        let slot = &mut self.slots[self.next_slot];
        self.gpu
            .as_mut()
            .ok_or(CuError::from("Missing CUDA context"))?
            .planar(
                slot.image
                    .as_ref()
                    .ok_or(CuError::from("Missing CUDA image"))?,
                frame,
                &mut slot.pre_fence,
            )?;
        self.submit()?;
        self.receive(output)
    }
    /// Synchronous camera helper for byte-exact validation. Capture workers use the queue.
    pub fn encode_sample(
        &mut self,
        sample: &gstreamer::Sample,
        output: &mut Arc<[u8]>,
    ) -> CuResult<()> {
        if !self.queue.is_empty() {
            return Err("Drain queued camera frames before synchronous validation".into());
        }
        self.submit_sample(sample)?;
        self.receive(output)
    }
    fn submit(&mut self) -> CuResult<()> {
        let slot = &mut self.slots[self.next_slot];
        slot.uploaded = Instant::now();
        let picture = PictureH265 {
            kind: 0,
            flags: 1,
            rc: Self::rate_control(),
            fps_num: self.fps,
            fps_den: 1,
            ..Default::default()
        };
        // SAFETY: each queued frame owns a distinct registered surface and persistent
        // fences. CUDA completes conversion/cache maintenance before IEP consumes it.
        unsafe {
            call!(
                self.api,
                nv_mediaiep_insert_pre_nv_sci_sync_fence(self.iep, &slot.pre_fence)
            )?;
            call!(
                self.api,
                nv_mediaiep_feed_frame(
                    self.iep,
                    slot.surface.object,
                    std::ptr::from_ref(&picture).cast(),
                    0
                )
            )?;
            call!(
                self.api,
                nv_mediaiep_geteof_nv_sci_sync_fence(self.iep, self.eof, &mut slot.eof_fence)
            )?;
        }
        self.queue.push_back(self.next_slot);
        self.next_slot = (self.next_slot + 1) % self.slots.len();
        self.poisoned = false;
        Ok(())
    }
    /// Collect the oldest submitted frame directly into its final shared allocation.
    /// IEP never retains the output pointer. No compressed-frame staging copy is made.
    pub fn receive(&mut self, output: &mut Arc<[u8]>) -> CuResult<()> {
        if self.poisoned {
            return Err("Encoder cannot be reused after an error".into());
        }
        let index = *self
            .queue
            .front()
            .ok_or(CuError::from("No submitted frame"))?;
        self.poisoned = true;
        let slot = &mut self.slots[index];
        // SAFETY: slot and its source sample remain live through EOF. GetBits fills
        // exactly the checked allocation before its initialized Arc is published.
        unsafe {
            call!(
                self.api,
                nv_sci_sync_fence_wait(&slot.eof_fence, self.waiter, 10_000_000)
            )?;
            let encoded = Instant::now();
            let mut available = 0;
            call!(
                self.api,
                nv_mediaiep_bits_available(self.iep, &mut available, 1, 10_000)
            )?;
            if available == 0 || available > 32 * 1024 * 1024 {
                return Err("IEP output exceeds the 32 MiB bound".into());
            }
            let mut bytes = Arc::<[u8]>::new_uninit_slice(available as usize);
            let stream = Bitstream {
                data: Arc::get_mut(&mut bytes)
                    .ok_or(CuError::from("Output unexpectedly shared"))?
                    .as_mut_ptr()
                    .cast(),
                used: 0,
                capacity: available,
            };
            let mut used = 0;
            call!(
                self.api,
                nv_mediaiep_get_bits(self.iep, &mut used, 1, &stream, null_mut())
            )?;
            if used != available || (stream.used != 0 && stream.used != used) {
                return Err("IEP returned inconsistent bitstream length".into());
            }
            *output = bytes.assume_init();
            (self.api.nv_sci_sync_fence_clear)(&mut slot.pre_fence);
            (self.api.nv_sci_sync_fence_clear)(&mut slot.eof_fence);
            self.timings = EncodeTimings {
                upload: slot.uploaded - slot.started,
                encode: encoded - slot.uploaded,
                retrieve: encoded.elapsed(),
            };
        }
        slot.sample.take();
        self.queue.pop_front();
        self.last_slot = Some(index);
        self.pending = !self.queue.is_empty();
        self.poisoned = false;
        Ok(())
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        // Drain known submissions on normal shutdown. A failed submission may not
        // have a usable fence, so retain every potentially live resource on failure.
        if !self.poisoned {
            let mut completed = true;
            for &index in &self.queue {
                // SAFETY: known queued submission owns a valid persistent EOF fence.
                unsafe {
                    if (self.api.nv_sci_sync_fence_wait)(
                        &self.slots[index].eof_fence,
                        self.waiter,
                        10_000_000,
                    ) != 0
                    {
                        completed = false;
                        break;
                    }
                }
            }
            self.pending = !completed;
        }
        if !self.pending
            && self
                .gpu
                .as_ref()
                .is_some_and(|gpu| gpu.synchronize().is_err())
        {
            self.pending = true;
        }
        if self.pending {
            std::mem::forget(self.api.clone());
            std::mem::forget(std::mem::take(&mut self.slots));
            if let Some(gpu) = self.gpu.take() {
                std::mem::forget(gpu);
            }
            if let Some(reserve) = self.reserve.take() {
                std::mem::forget(reserve);
            }
            return;
        }
        // SAFETY: all CUDA work and encoder EOFs completed. Release imports before
        // NvSci objects, and registered resources before their IEP owner.
        unsafe {
            for slot in &mut self.slots {
                slot.image.take();
                slot.sample.take();
                if slot.registered {
                    (self.api.nv_mediaiep_unregister_nv_sci_buf_obj)(self.iep, slot.surface.object);
                }
                (self.api.nv_sci_sync_fence_clear)(&mut slot.pre_fence);
                (self.api.nv_sci_sync_fence_clear)(&mut slot.eof_fence);
            }
            self.gpu.take();
            if self.pre_registered {
                (self.api.nv_mediaiep_unregister_nv_sci_sync_obj)(self.iep, self.pre);
            }
            if self.eof_registered {
                (self.api.nv_mediaiep_unregister_nv_sci_sync_obj)(self.iep, self.eof);
            }
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
            self.slots.clear();
            self.reserve.take();
            if !self.module.is_null() {
                (self.api.nv_sci_buf_module_close)(self.module);
            }
        }
    }
}
