//! CUDA/NvSci interop. Camera DMA buffers never pass through a CPU pixel map.
use super::ffi::{Fence, Handle};
use cu29::prelude::*;
use libloading::Library;
use std::ffi::{CString, c_char, c_void};
use std::os::fd::{AsRawFd, BorrowedFd, IntoRawFd};
use std::os::unix::fs::MetadataExt;
use std::ptr::{null, null_mut};
use std::sync::Arc;

#[repr(C)]
#[derive(Default)]
struct MemoryDesc {
    kind: i32,
    handle: [u64; 2],
    size: u64,
    flags: u32,
    reserved: [u32; 16],
}
#[repr(C)]
#[derive(Default)]
struct BufferDesc {
    offset: u64,
    size: u64,
    flags: u32,
    reserved: [u32; 16],
}
#[repr(C)]
#[derive(Default)]
struct ArrayDesc {
    width: usize,
    height: usize,
    depth: usize,
    format: i32,
    channels: u32,
    flags: u32,
}
#[repr(C)]
#[derive(Default)]
struct MipmapDesc {
    offset: u64,
    array: ArrayDesc,
    levels: u32,
    reserved: [u32; 16],
}
#[repr(C)]
#[derive(Default)]
struct ResourceDesc {
    kind: i32,
    resource: [u64; 16],
    flags: u32,
}
#[repr(C)]
#[derive(Default)]
struct SemaphoreDesc {
    kind: i32,
    handle: [u64; 2],
    flags: u32,
    reserved: [u32; 16],
}
#[repr(C)]
#[derive(Default)]
struct SignalParams {
    value: u64,
    fence: u64,
    key: u64,
    params_reserved: [u32; 12],
    flags: u32,
    reserved: [u32; 16],
}
const _: () = {
    assert!(size_of::<MemoryDesc>() == 104);
    assert!(size_of::<BufferDesc>() == 88);
    assert!(size_of::<MipmapDesc>() == 120);
    assert!(size_of::<ResourceDesc>() == 144);
    assert!(size_of::<SemaphoreDesc>() == 96);
    assert!(size_of::<SignalParams>() == 144);
    assert!(std::mem::offset_of!(MemoryDesc, handle) == 8);
    assert!(std::mem::offset_of!(MemoryDesc, size) == 24);
    assert!(std::mem::offset_of!(SignalParams, fence) == 8);
    assert!(std::mem::offset_of!(SignalParams, flags) == 72);
};
fn check(status: i32, name: &str) -> CuResult<()> {
    if status == 0 {
        Ok(())
    } else {
        Err(CuError::from(format!("{name}: CUDA/NVRTC status {status}")))
    }
}
macro_rules! driver {
    ($($field:ident: $symbol:ident ($($arg:ty),*) ;)*) => {
        struct Driver {
            $($field: unsafe extern "C" fn($($arg),*) -> i32,)*
            _library: Library,
        }
        impl Driver {
            fn load() -> CuResult<Arc<Self>> {
                // SAFETY: signatures follow the installed CUDA driver LP64 ABI.
                unsafe {
                    let library = Library::new("libcuda.so.1").map_err(load_error)?;
                    Ok(Arc::new(Self {
                        $($field: *library.get(concat!(stringify!($symbol), "\0").as_bytes()).map_err(load_error)?,)*
                        _library: library,
                    }))
                }
            }
        }
    }
}
fn load_error(error: libloading::Error) -> CuError {
    CuError::new_with_cause("Cannot load CUDA driver/NVRTC/GStreamer DMA ABI", error)
}
driver! {
    init: cuInit(u32);
    device: cuDeviceGet(*mut i32, i32);
    uuid: cuDeviceGetUuid_v2(*mut [u8; 16], i32);
    retain: cuDevicePrimaryCtxRetain(*mut Handle, i32);
    release: cuDevicePrimaryCtxRelease_v2(i32);
    current: cuCtxSetCurrent(Handle);
    stream_create: cuStreamCreate(*mut Handle, u32);
    stream_destroy: cuStreamDestroy_v2(Handle);
    stream_sync: cuStreamSynchronize(Handle);
    event_create: cuEventCreate(*mut Handle, u32);
    event_record: cuEventRecord(Handle, Handle);
    event_query: cuEventQuery(Handle);
    event_destroy: cuEventDestroy_v2(Handle);
    module_load: cuModuleLoadData(*mut Handle, *const c_void);
    module_unload: cuModuleUnload(Handle);
    function: cuModuleGetFunction(*mut Handle, Handle, *const c_char);
    launch: cuLaunchKernel(Handle, u32, u32, u32, u32, u32, u32, u32, Handle, *mut *mut c_void, *mut *mut c_void);
    alloc: cuMemAlloc_v2(*mut u64, usize);
    free: cuMemFree_v2(u64);
    upload: cuMemcpyHtoD_v2(u64, *const c_void, usize);
    import: cuImportExternalMemory(*mut Handle, *const MemoryDesc);
    destroy: cuDestroyExternalMemory(Handle);
    map_buffer: cuExternalMemoryGetMappedBuffer(*mut u64, Handle, *const BufferDesc);
    map_array: cuExternalMemoryGetMappedMipmappedArray(*mut Handle, Handle, *const MipmapDesc);
    array_level: cuMipmappedArrayGetLevel(*mut Handle, Handle, u32);
    array_destroy: cuMipmappedArrayDestroy(Handle);
    surface_create: cuSurfObjectCreate(*mut u64, *const ResourceDesc);
    surface_destroy: cuSurfObjectDestroy(u64);
    sync_attrs: cuDeviceGetNvSciSyncAttributes(Handle, i32, i32);
    semaphore_import: cuImportExternalSemaphore(*mut Handle, *const SemaphoreDesc);
    semaphore_destroy: cuDestroyExternalSemaphore(Handle);
    signal: cuSignalExternalSemaphoresAsync(*const Handle, *const SignalParams, u32, Handle);
}

fn compile() -> CuResult<Vec<u8>> {
    // SAFETY: NVRTC owns the program until destruction; every buffer is sized by NVRTC.
    unsafe {
        let library = Library::new("libnvrtc.so.13").map_err(load_error)?;
        let create = library
            .get::<unsafe extern "C" fn(
                *mut Handle,
                *const c_char,
                *const c_char,
                i32,
                *const *const c_char,
                *const *const c_char,
            ) -> i32>(b"nvrtcCreateProgram\0")
            .map_err(load_error)?;
        let compile = library
            .get::<unsafe extern "C" fn(Handle, i32, *const *const c_char) -> i32>(
                b"nvrtcCompileProgram\0",
            )
            .map_err(load_error)?;
        let log_size = library
            .get::<unsafe extern "C" fn(Handle, *mut usize) -> i32>(b"nvrtcGetProgramLogSize\0")
            .map_err(load_error)?;
        let log = library
            .get::<unsafe extern "C" fn(Handle, *mut u8) -> i32>(b"nvrtcGetProgramLog\0")
            .map_err(load_error)?;
        let size = library
            .get::<unsafe extern "C" fn(Handle, *mut usize) -> i32>(b"nvrtcGetPTXSize\0")
            .map_err(load_error)?;
        let ptx = library
            .get::<unsafe extern "C" fn(Handle, *mut u8) -> i32>(b"nvrtcGetPTX\0")
            .map_err(load_error)?;
        let destroy = library
            .get::<unsafe extern "C" fn(*mut Handle) -> i32>(b"nvrtcDestroyProgram\0")
            .map_err(load_error)?;
        let source = CString::new(include_str!("unpack.cu"))
            .map_err(|e| CuError::new_with_cause("CUDA source contains NUL", e))?;
        let mut program = null_mut();
        check(
            create(
                &mut program,
                source.as_ptr(),
                c"unpack.cu".as_ptr(),
                0,
                null(),
                null(),
            ),
            "nvrtcCreateProgram",
        )?;
        // Keep cleanup on every error path after successful creation.
        let result = (|| {
            let options = [c"--gpu-architecture=compute_87".as_ptr()];
            let status = compile(program, options.len() as i32, options.as_ptr());
            if status != 0 {
                let mut length = 0;
                check(log_size(program, &mut length), "nvrtcGetProgramLogSize")?;
                let mut bytes = vec![0; length];
                check(log(program, bytes.as_mut_ptr()), "nvrtcGetProgramLog")?;
                return Err(CuError::from(format!(
                    "CUDA kernel compilation: {}",
                    String::from_utf8_lossy(&bytes)
                )));
            }
            let mut length = 0;
            check(size(program, &mut length), "nvrtcGetPTXSize")?;
            let mut bytes = vec![0; length];
            check(ptx(program, bytes.as_mut_ptr()), "nvrtcGetPTX")?;
            Ok(bytes)
        })();
        destroy(&mut program);
        result
    }
}

/// CUDA owns an imported fd, pinning allocation identity without retaining GstMemory.
struct DmaInput {
    driver: Arc<Driver>,
    identity: (u64, u64, usize),
    external: Handle,
    pointer: u64,
}
impl Drop for DmaInput {
    fn drop(&mut self) {
        // SAFETY: only dropped after the CUDA stream is synchronized.
        unsafe {
            if self.pointer != 0 {
                (self.driver.free)(self.pointer);
            }
            if !self.external.is_null() {
                (self.driver.destroy)(self.external);
            }
        }
    }
}

/// CUDA view of the two block-linear NvSciBuf image planes.
pub(super) struct Image {
    driver: Arc<Driver>,
    external: Handle,
    arrays: [Handle; 2],
    surfaces: [u64; 2],
    converted: Handle,
}
impl Drop for Image {
    fn drop(&mut self) {
        // SAFETY: encoder releases CUDA views before freeing its NvSciBuf objects.
        unsafe {
            if !self.converted.is_null() {
                (self.driver.event_destroy)(self.converted);
            }
            for surface in self.surfaces {
                if surface != 0 {
                    (self.driver.surface_destroy)(surface);
                }
            }
            for array in self.arrays {
                if !array.is_null() {
                    (self.driver.array_destroy)(array);
                }
            }
            if !self.external.is_null() {
                (self.driver.destroy)(self.external);
            }
        }
    }
}

pub(super) struct Gpu {
    driver: Arc<Driver>,
    device: i32,
    context: Handle,
    pub uuid: [u8; 16],
    stream: Handle,
    module: Handle,
    function: Handle,
    semaphore: Handle,
    scratch: u64,
    dma: Vec<DmaInput>,
    is_dma: unsafe extern "C" fn(*mut gstreamer::ffi::GstMemory) -> i32,
    dma_fd: unsafe extern "C" fn(*mut gstreamer::ffi::GstMemory) -> i32,
    _allocators: Library,
}
impl Gpu {
    pub fn new() -> CuResult<Self> {
        let driver = Driver::load()?;
        // SAFETY: creation and all use happen on this encoder's worker thread.
        unsafe {
            let allocators = Library::new("libgstallocators-1.0.so.0").map_err(load_error)?;
            let is_dma = *allocators
                .get(b"gst_is_dmabuf_memory\0")
                .map_err(load_error)?;
            let dma_fd = *allocators
                .get(b"gst_dmabuf_memory_get_fd\0")
                .map_err(load_error)?;
            check((driver.init)(0), "cuInit")?;
            let mut device = 0;
            check((driver.device)(&mut device, 0), "cuDeviceGet")?;
            let mut gpu = Self {
                driver,
                device,
                context: null_mut(),
                uuid: [0; 16],
                stream: null_mut(),
                module: null_mut(),
                function: null_mut(),
                semaphore: null_mut(),
                scratch: 0,
                dma: Vec::new(),
                is_dma,
                dma_fd,
                _allocators: allocators,
            };
            check(
                (gpu.driver.retain)(&mut gpu.context, device),
                "cuDevicePrimaryCtxRetain",
            )?;
            check((gpu.driver.current)(gpu.context), "cuCtxSetCurrent")?;
            check((gpu.driver.uuid)(&mut gpu.uuid, device), "cuDeviceGetUuid")?;
            check(
                (gpu.driver.stream_create)(&mut gpu.stream, 1),
                "cuStreamCreate",
            )?;
            let ptx = compile()?;
            check(
                (gpu.driver.module_load)(&mut gpu.module, ptx.as_ptr().cast()),
                "cuModuleLoadData",
            )?;
            check(
                (gpu.driver.function)(&mut gpu.function, gpu.module, c"unpack".as_ptr()),
                "cuModuleGetFunction",
            )?;
            Ok(gpu)
        }
    }
    pub fn sync_attrs(&self, list: Handle) -> CuResult<()> {
        // SAFETY: list is an unreconciled live NvSciSync attribute list.
        unsafe {
            check(
                (self.driver.sync_attrs)(list, self.device, 1),
                "cuDeviceGetNvSciSyncAttributes",
            )
        }
    }
    pub fn semaphore(&mut self, object: Handle) -> CuResult<()> {
        let desc = SemaphoreDesc {
            kind: 6,
            handle: [object as u64, 0],
            ..Default::default()
        };
        // SAFETY: encoder retains the NvSciSync object until after this import is destroyed.
        unsafe {
            check(
                (self.driver.semaphore_import)(&mut self.semaphore, &desc),
                "cuImportExternalSemaphore",
            )
        }
    }
    pub fn image(
        &self,
        object: Handle,
        size: u64,
        offsets: [u64; 2],
        pitches: [u32; 2],
        heights: [u32; 2],
    ) -> CuResult<Image> {
        let mut image = Image {
            driver: self.driver.clone(),
            external: null_mut(),
            arrays: [null_mut(); 2],
            surfaces: [0; 2],
            converted: null_mut(),
        };
        let desc = MemoryDesc {
            kind: 8,
            handle: [object as u64, 0],
            size,
            ..Default::default()
        };
        // SAFETY: NvSciBuf is retained by Encoder; reconciled plane layout determines mapping.
        unsafe {
            check(
                (self.driver.import)(&mut image.external, &desc),
                "cuImportExternalMemory(NvSciBuf)",
            )?;
            check(
                (self.driver.event_create)(&mut image.converted, 2),
                "cuEventCreate",
            )?;
            for i in 0..2 {
                let channels = if i == 0 { 1 } else { 2 };
                let desc = MipmapDesc {
                    offset: offsets[i],
                    array: ArrayDesc {
                        width: pitches[i] as usize / channels as usize,
                        height: heights[i] as usize,
                        format: 1,
                        channels,
                        flags: 2,
                        ..Default::default()
                    },
                    levels: 1,
                    ..Default::default()
                };
                check(
                    (self.driver.map_array)(&mut image.arrays[i], image.external, &desc),
                    "cuExternalMemoryGetMappedMipmappedArray",
                )?;
                let mut array = null_mut();
                check(
                    (self.driver.array_level)(&mut array, image.arrays[i], 0),
                    "cuMipmappedArrayGetLevel",
                )?;
                let desc = ResourceDesc {
                    resource: {
                        let mut r = [0; 16];
                        r[0] = array as u64;
                        r
                    },
                    ..Default::default()
                };
                check(
                    (self.driver.surface_create)(&mut image.surfaces[i], &desc),
                    "cuSurfObjectCreate",
                )?;
            }
        }
        Ok(image)
    }
    pub fn planar(&mut self, image: &Image, data: &[u8], fence: &mut Fence) -> CuResult<()> {
        if data.len() != 1920 * 1080 * 3 {
            return Err("Expected tightly packed 1080p Y444".into());
        }
        // SAFETY: scratch is owned by this GPU and used only on its ordered stream.
        unsafe {
            if self.scratch == 0 {
                check(
                    (self.driver.alloc)(&mut self.scratch, data.len()),
                    "cuMemAlloc",
                )?;
            }
            check(
                (self.driver.upload)(self.scratch, data.as_ptr().cast(), data.len()),
                "cuMemcpyHtoD",
            )?;
        }
        self.launch(image, self.scratch, 1920, 1, fence)
    }
    pub fn sample(
        &mut self,
        image: &Image,
        sample: &gstreamer::Sample,
        fence: &mut Fence,
    ) -> CuResult<()> {
        let info = gstreamer_video::VideoInfo::from_caps(
            sample
                .caps()
                .ok_or(CuError::from("Camera sample has no caps"))?,
        )
        .map_err(|e| CuError::new_with_cause("Invalid camera caps", e))?;
        if info.format() != gstreamer_video::VideoFormat::Uyvy
            || info.width() != 1920
            || info.height() != 1080
        {
            return Err("CUDA capture requires 1920×1080 UYVY".into());
        }
        let buffer = sample
            .buffer()
            .ok_or(CuError::from("Camera sample has no buffer"))?;
        if buffer.n_memory() != 1 {
            return Err("CUDA capture requires one contiguous DMA allocation".into());
        }
        let memory = buffer.peek_memory(0);
        let (stride, offset) = if let Some(meta) = buffer.meta::<gstreamer_video::VideoMeta>() {
            (meta.stride()[0], meta.offset()[0])
        } else {
            (info.stride()[0], info.offset()[0])
        };
        let stride =
            usize::try_from(stride).map_err(|_| CuError::from("Negative camera stride"))?;
        let needed = offset
            .checked_add(stride * 1079 + 3840)
            .ok_or(CuError::from("Camera layout overflow"))?;
        if stride < 3840 || needed > memory.size() {
            return Err("Camera DMA allocation is too small".into());
        }
        // Only the submitted sample retains GstMemory. Keeping extra GstMemory refs
        // here makes the V4L2 pool allocate/copy frames instead of recycling its DMA buffers.
        // CUDA owns a duplicated fd, keeping the underlying inode/allocation alive.
        let duplicate = unsafe {
            if (self.is_dma)(memory.as_ptr().cast_mut()) == 0 {
                return Err("Camera did not provide DMA memory; set io-mode=dmabuf".into());
            }
            let fd = (self.dma_fd)(memory.as_ptr().cast_mut());
            BorrowedFd::borrow_raw(fd)
                .try_clone_to_owned()
                .map_err(|e| CuError::new_with_cause("Cannot duplicate camera DMA fd", e))?
        };
        let file = std::fs::File::from(duplicate);
        let metadata = file
            .metadata()
            .map_err(|e| CuError::new_with_cause("Cannot identify camera DMA allocation", e))?;
        let identity = (metadata.dev(), metadata.ino(), memory.maxsize());
        let index =
            if let Some(index) = self.dma.iter().position(|input| input.identity == identity) {
                index
            } else {
                // Bounded capture pool cache. Never evict a mapping while a kernel may run.
                if self.dma.len() >= 32 {
                    self.synchronize()?;
                    self.dma.clear();
                }
                // SAFETY: CUDA takes ownership of a duplicated fd only on import success.
                // Encoder retains the sample through EOF, preventing capture from overwriting it.
                unsafe {
                    let desc = MemoryDesc {
                        kind: 1,
                        handle: [file.as_raw_fd() as u64, 0],
                        size: memory.maxsize() as u64,
                        ..Default::default()
                    };
                    let mut input = DmaInput {
                        driver: self.driver.clone(),
                        identity,
                        external: null_mut(),
                        pointer: 0,
                    };
                    check(
                        (self.driver.import)(&mut input.external, &desc),
                        "cuImportExternalMemory(camera DMA)",
                    )?;
                    let _ = file.into_raw_fd();
                    let desc = BufferDesc {
                        size: memory.maxsize() as u64,
                        ..Default::default()
                    };
                    check(
                        (self.driver.map_buffer)(&mut input.pointer, input.external, &desc),
                        "cuExternalMemoryGetMappedBuffer",
                    )?;
                    self.dma.push(input);
                    self.dma.len() - 1
                }
            };
        self.launch(
            image,
            self.dma[index].pointer + memory.offset() as u64 + offset as u64,
            stride as u32,
            0,
            fence,
        )
    }
    fn launch(
        &self,
        image: &Image,
        mut input: u64,
        mut pitch: u32,
        mut planar: u32,
        fence: &mut Fence,
    ) -> CuResult<()> {
        let mut y = image.surfaces[0];
        let mut uv = image.surfaces[1];
        let mut args = [
            std::ptr::from_mut(&mut input).cast(),
            std::ptr::from_mut(&mut pitch).cast(),
            std::ptr::from_mut(&mut planar).cast(),
            std::ptr::from_mut(&mut y).cast(),
            std::ptr::from_mut(&mut uv).cast(),
        ];
        // SAFETY: all arguments are copied during launch. NvSciSync fences make encoder
        // access wait for the kernel and CUDA's NvSciBuf cache maintenance (flags=0).
        unsafe {
            check(
                (self.driver.launch)(
                    self.function,
                    120,
                    68,
                    1,
                    16,
                    16,
                    1,
                    0,
                    self.stream,
                    args.as_mut_ptr(),
                    null_mut(),
                ),
                "cuLaunchKernel",
            )?;
            let params = SignalParams {
                fence: std::ptr::from_mut(fence) as u64,
                ..Default::default()
            };
            check(
                (self.driver.signal)(&self.semaphore, &params, 1, self.stream),
                "cuSignalExternalSemaphoresAsync",
            )?;
            check(
                (self.driver.event_record)(image.converted, self.stream),
                "cuEventRecord",
            )
        }
    }
    pub fn converted(&self, image: &Image) -> CuResult<bool> {
        // SAFETY: the event was recorded after this slot's conversion and cache flush.
        match unsafe { (self.driver.event_query)(image.converted) } {
            0 => Ok(true),
            600 => Ok(false),
            status => {
                check(status, "cuEventQuery")?;
                Ok(false)
            }
        }
    }
    pub fn synchronize(&self) -> CuResult<()> {
        // SAFETY: live stream owned by this worker.
        unsafe {
            check(
                (self.driver.stream_sync)(self.stream),
                "cuStreamSynchronize",
            )
        }
    }
}
impl Drop for Gpu {
    fn drop(&mut self) {
        // SAFETY: Encoder synchronizes before dropping its CUDA views. Partial init
        // has no queued work. On asynchronous errors Encoder leaks all live objects.
        unsafe {
            if !self.context.is_null() {
                (self.driver.current)(self.context);
            }
            if !self.stream.is_null() {
                (self.driver.stream_sync)(self.stream);
            }
            self.dma.clear();
            if self.scratch != 0 {
                (self.driver.free)(self.scratch);
            }
            if !self.semaphore.is_null() {
                (self.driver.semaphore_destroy)(self.semaphore);
            }
            if !self.module.is_null() {
                (self.driver.module_unload)(self.module);
            }
            if !self.stream.is_null() {
                (self.driver.stream_destroy)(self.stream);
            }
            if !self.context.is_null() {
                (self.driver.release)(self.device);
            }
        }
    }
}
