//! Zero-copy(-ish) dmabuf -> NVENC bridge for NVIDIA.
//!
//! carjack's compositor renders into a **block-linear** dmabuf on the NVIDIA GPU. `glupload`
//! can't import that (NVIDIA EGL won't hand its own tiled buffer back as a GL texture), but
//! CUDA's EGL interop can. This crate imports the dmabuf via
//! `eglCreateImageKHR` + `cuGraphicsEGLRegisterImage`, de-tiles it with a GPU-side
//! `cuMemcpy2D` into a pitched CUDA buffer, and wraps that as a `GstCudaMemory` buffer that
//! `nvh264enc` consumes directly (it does the RGB->NV12 conversion internally). No GPU->CPU
//! readback, no software encode.
//!
//! The one unavoidable copy on NVIDIA is the GPU->GPU de-tile (block-linear surfaces map to a
//! `CUarray`, and NVENC needs pitched memory); it stays on the GPU and is cheap.

use std::ffi::c_void;
use std::fs::File;
use std::os::fd::{AsRawFd, RawFd};
use std::ptr;
use std::sync::{Arc, Mutex};

use eyre::{Result, eyre};
use gst::glib::translate::ToGlibPtr;
use gstreamer as gst;
use gstreamer_video as gst_video;

mod ffi;

/// A single-plane dmabuf to import (carjack fills this from the compositor's smithay `Dmabuf`,
/// keeping smithay out of this crate). The `fd` is borrowed for the duration of [`CudaImporter::import`]
/// and never closed here — the caller retains ownership.
pub struct DmabufPlane {
    pub fd: RawFd,
    pub width: u32,
    pub height: u32,
    /// DRM fourcc (e.g. `DRM_FORMAT_ARGB8888`).
    pub fourcc: u32,
    pub modifier: u64,
    pub offset: u32,
    pub stride: u32,
}

/// Imports NVIDIA dmabufs into GstCudaMemory buffers for `nvh264enc`. Construct once per session
/// on the NVIDIA render node; call [`import`](Self::import) per frame from the encode thread.
pub struct CudaImporter {
    _drm: File,
    egl_display: *mut c_void,
    egl_ext: ffi::EglExt,
    /// `GstCudaContext*` (owned ref) — shared with `nvh264enc` via [`gst_context`](Self::gst_context)
    /// so the wrapped memory and the encoder live on the same CUDA context.
    gst_cuda_ctx: *mut c_void,
    /// Reused pitched device buffers, so `import` doesn't `cuMemAlloc`/`cuMemFree` every frame.
    pool: Arc<CudaPool>,
}

unsafe impl Send for CudaImporter {}
// SAFETY: `import` uses per-call CUDA context push/pop, so it's safe to call through a shared
// reference. carjack shares one importer across encode sessions (behind an `Arc`) but only ever
// drives it from one encode task at a time - sessions don't overlap.
unsafe impl Sync for CudaImporter {}

/// A free-list of pitched CUDA device buffers (`cuMemAlloc`'d), keyed by size. The de-tile in
/// `import` allocates one per frame; without pooling that's a `cuMemAlloc`+`cuMemFree` per frame
/// (30/s), whose latency spikes show up as video jitter. Buffers return here when GStreamer frees
/// the wrapped memory (see `free_devptr`) and are handed straight back out.
struct CudaPool {
    /// The shared `GstCudaContext*`, needed to make the context current when actually freeing.
    gst_ctx: *mut c_void,
    free: Mutex<Vec<Pooled>>,
}

#[derive(Clone, Copy)]
struct Pooled {
    devptr: u64,
    size: usize,
}

// SAFETY: the only shared state is the mutex-guarded free-list; the raw `gst_ctx` is only used
// under CUDA's context push/pop, which is thread-safe.
unsafe impl Send for CudaPool {}
unsafe impl Sync for CudaPool {}

impl CudaPool {
    /// Get a device buffer of exactly `size` bytes - reusing a pooled one or allocating a fresh
    /// one. Must be called with the CUDA context already current (it is, inside `import`); any
    /// pooled buffers left at a *different* size (stale after a resolution change) are freed here.
    fn acquire(&self, size: usize) -> Result<u64> {
        let mut free = self.free.lock().unwrap();
        if let Some(pos) = free.iter().position(|b| b.size == size) {
            return Ok(free.swap_remove(pos).devptr);
        }
        // No match: drop any stale-size buffers (context is current) so the pool doesn't grow
        // across resolution changes, then allocate.
        for b in free.drain(..) {
            unsafe { ffi::cuMemFree_v2(b.devptr) };
        }
        let mut devptr: u64 = 0;
        let r = unsafe { ffi::cuMemAlloc_v2(&mut devptr, size) };
        if r != 0 {
            return Err(eyre!("cuMemAlloc({size}) failed: {}", ffi::cu_error_str(r)));
        }
        Ok(devptr)
    }

    /// Return a buffer to the pool. Called from the GstCudaMemory free callback (possibly on
    /// nvh264enc's thread); just stashes it - no CUDA call, so no context juggling needed.
    fn release(&self, devptr: u64, size: usize) {
        self.free.lock().unwrap().push(Pooled { devptr, size });
    }
}

impl Drop for CudaPool {
    fn drop(&mut self) {
        // All wrapped buffers hold an `Arc<CudaPool>`, so by the time this runs every buffer has
        // been returned to the free-list - actually free them now (context made current first).
        let free = self.free.get_mut().unwrap();
        if free.is_empty() {
            return;
        }
        unsafe {
            ffi::gst_cuda_context_push(self.gst_ctx);
            for b in free.drain(..) {
                ffi::cuMemFree_v2(b.devptr);
            }
            ffi::gst_cuda_context_pop(ptr::null_mut());
        }
    }
}

struct NotifyData {
    pool: Arc<CudaPool>,
    devptr: u64,
    size: usize,
}

/// GDestroyNotify for the wrapped GstCudaMemory: return the pitched buffer to the pool for reuse
/// (this may run on nvh264enc's streaming thread).
extern "C" fn free_devptr(data: *mut c_void) {
    let d = unsafe { Box::from_raw(data as *mut NotifyData) };
    d.pool.release(d.devptr, d.size);
}

impl CudaImporter {
    pub fn new(render_node: &str) -> Result<Self> {
        let _ = gst::init();
        let r = unsafe { ffi::cuInit(0) };
        if r != 0 {
            return Err(eyre!("cuInit failed: {}", ffi::cu_error_str(r)));
        }

        let drm = File::options()
            .read(true)
            .write(true)
            .open(render_node)
            .map_err(|e| eyre!("open {render_node}: {e}"))?;
        let gbm = unsafe { ffi::gbm_create_device(drm.as_raw_fd()) };
        if gbm.is_null() {
            return Err(eyre!("gbm_create_device({render_node}) failed"));
        }
        let egl_ext = ffi::EglExt::load()?;
        let egl_display =
            unsafe { (egl_ext.get_platform_display)(ffi::EGL_PLATFORM_GBM_KHR, gbm, ptr::null()) };
        if egl_display.is_null() {
            return Err(eyre!("eglGetPlatformDisplayEXT failed: 0x{:x}", unsafe {
                ffi::eglGetError()
            }));
        }
        let (mut major, mut minor) = (0i32, 0i32);
        if unsafe { ffi::eglInitialize(egl_display, &mut major, &mut minor) } == 0 {
            return Err(eyre!("eglInitialize failed: 0x{:x}", unsafe {
                ffi::eglGetError()
            }));
        }
        tracing::info!("cuda-dmabuf: EGL {major}.{minor} on {render_node}");

        // Load libgstcuda's CUDA driver vtable before creating a context - it's normally loaded
        // by the nvcodec plugin's init, which hasn't run yet at this point.
        if unsafe { ffi::gst_cuda_load_library() } == 0 {
            return Err(eyre!("gst_cuda_load_library failed (no CUDA driver?)"));
        }
        let gst_cuda_ctx = unsafe { ffi::gst_cuda_context_new(0) };
        if gst_cuda_ctx.is_null() {
            return Err(eyre!("gst_cuda_context_new(0) failed"));
        }
        tracing::info!("cuda-dmabuf: importer ready");

        let pool = Arc::new(CudaPool {
            gst_ctx: gst_cuda_ctx,
            free: Mutex::new(Vec::new()),
        });
        Ok(CudaImporter {
            _drm: drm,
            egl_display,
            egl_ext,
            gst_cuda_ctx,
            pool,
        })
    }

    /// `GstContext` to set on the encode pipeline so `nvh264enc` adopts our CUDA context (the one
    /// the imported memory lives on).
    pub fn gst_context(&self) -> gst::Context {
        let ctx = unsafe { ffi::gst_context_new_cuda_context(self.gst_cuda_ctx) };
        unsafe { gst::Context::from_glib_full(ctx as *mut gst::ffi::GstContext) }
    }

    /// Import one dmabuf and return a `GstCudaMemory` buffer ready to push at `nvh264enc`.
    pub fn import(&self, plane: &DmabufPlane) -> Result<gst::Buffer> {
        if unsafe { ffi::gst_cuda_context_push(self.gst_cuda_ctx) } == 0 {
            return Err(eyre!("gst_cuda_context_push failed"));
        }
        let _pop = PopGuard;

        // 1. Wrap the dmabuf as an EGLImage (no GL context needed).
        let attribs: [i32; 19] = [
            ffi::EGL_WIDTH,
            plane.width as i32,
            ffi::EGL_HEIGHT,
            plane.height as i32,
            ffi::EGL_LINUX_DRM_FOURCC_EXT,
            plane.fourcc as i32,
            ffi::EGL_DMA_BUF_PLANE0_FD_EXT,
            plane.fd,
            ffi::EGL_DMA_BUF_PLANE0_OFFSET_EXT,
            plane.offset as i32,
            ffi::EGL_DMA_BUF_PLANE0_PITCH_EXT,
            plane.stride as i32,
            ffi::EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT,
            (plane.modifier & 0xffff_ffff) as i32,
            ffi::EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT,
            (plane.modifier >> 32) as i32,
            ffi::EGL_NONE,
            0,
            0,
        ];
        let image = unsafe {
            (self.egl_ext.create_image)(
                self.egl_display,
                ptr::null_mut(),
                ffi::EGL_LINUX_DMA_BUF_EXT,
                ptr::null_mut(),
                attribs.as_ptr(),
            )
        };
        if image.is_null() {
            return Err(eyre!("eglCreateImageKHR failed: 0x{:x}", unsafe {
                ffi::eglGetError()
            }));
        }
        let _img = ImageGuard {
            egl_ext: &self.egl_ext,
            dpy: self.egl_display,
            image,
        };

        // 2. Register with CUDA and map to a CUeglFrame (a CUarray for block-linear surfaces).
        let mut resource: *mut c_void = ptr::null_mut();
        let r = unsafe { ffi::cuGraphicsEGLRegisterImage(&mut resource, image, 0) };
        if r != 0 {
            return Err(eyre!(
                "cuGraphicsEGLRegisterImage failed: {}",
                ffi::cu_error_str(r)
            ));
        }
        let _res = ResGuard { resource };

        let mut frame: ffi::CUeglFrame = unsafe { std::mem::zeroed() };
        let r = unsafe { ffi::cuGraphicsResourceGetMappedEglFrame(&mut frame, resource, 0, 0) };
        if r != 0 {
            return Err(eyre!(
                "cuGraphicsResourceGetMappedEglFrame failed: {}",
                ffi::cu_error_str(r)
            ));
        }

        // 3. Allocate a pitched CUDA buffer and de-tile into it (GPU->GPU).
        let info =
            gst_video::VideoInfo::builder(gst_video::VideoFormat::Bgra, plane.width, plane.height)
                .build()
                .map_err(|e| eyre!("VideoInfo: {e}"))?;
        let pitch = info.stride()[0] as usize;
        let size = pitch * plane.height as usize;

        // Reuse a pooled pitched buffer if one's free (the context is current here).
        let devptr = self.pool.acquire(size)?;

        let copy = ffi::CudaMemcpy2D {
            src_memory_type: if frame.frame_type == ffi::CU_EGL_FRAME_TYPE_ARRAY {
                ffi::CU_MEMORYTYPE_ARRAY
            } else {
                ffi::CU_MEMORYTYPE_DEVICE
            },
            src_array: frame.frame[0],
            src_device: frame.frame[0] as u64,
            src_pitch: frame.pitch as usize,
            dst_memory_type: ffi::CU_MEMORYTYPE_DEVICE,
            dst_device: devptr,
            dst_pitch: pitch,
            width_in_bytes: plane.width as usize * 4,
            height: plane.height as usize,
            ..Default::default()
        };
        let r = unsafe { ffi::cuMemcpy2D_v2(&copy) };
        if r != 0 {
            self.pool.release(devptr, size);
            return Err(eyre!("cuMemcpy2D failed: {}", ffi::cu_error_str(r)));
        }

        // 4. Wrap the pitched buffer as GstCudaMemory on our shared context.
        let notify = Box::new(NotifyData {
            pool: self.pool.clone(),
            devptr,
            size,
        });
        let info_ptr: *const gst_video::ffi::GstVideoInfo = info.to_glib_none().0;
        let notify_ptr = Box::into_raw(notify);
        let mem = unsafe {
            ffi::gst_cuda_allocator_alloc_wrapped(
                ptr::null_mut(),
                self.gst_cuda_ctx,
                ptr::null_mut(),
                info_ptr as *const c_void,
                devptr,
                notify_ptr as *mut c_void,
                Some(free_devptr),
            )
        };
        if mem.is_null() {
            // `notify` wasn't handed over, so free_devptr won't run - reclaim it and return the
            // buffer to the pool ourselves.
            drop(unsafe { Box::from_raw(notify_ptr) });
            self.pool.release(devptr, size);
            return Err(eyre!("gst_cuda_allocator_alloc_wrapped failed"));
        }
        let mem = unsafe { gst::Memory::from_glib_full(mem as *mut gst::ffi::GstMemory) };

        let mut buffer = gst::Buffer::new();
        {
            let b = buffer.get_mut().unwrap();
            b.append_memory(mem);
            gst_video::VideoMeta::add_full(
                b,
                gst_video::VideoFrameFlags::empty(),
                gst_video::VideoFormat::Bgra,
                plane.width,
                plane.height,
                &[0],
                &[pitch as i32],
            )
            .map_err(|e| eyre!("VideoMeta: {e}"))?;
        }
        Ok(buffer)
    }
}

// ---- RAII guards ----
struct PopGuard;
impl Drop for PopGuard {
    fn drop(&mut self) {
        unsafe { ffi::gst_cuda_context_pop(ptr::null_mut()) };
    }
}

struct ImageGuard<'a> {
    egl_ext: &'a ffi::EglExt,
    dpy: *mut c_void,
    image: *mut c_void,
}
impl Drop for ImageGuard<'_> {
    fn drop(&mut self) {
        unsafe { (self.egl_ext.destroy_image)(self.dpy, self.image) };
    }
}

struct ResGuard {
    resource: *mut c_void,
}
impl Drop for ResGuard {
    fn drop(&mut self) {
        unsafe { ffi::cuGraphicsUnregisterResource(self.resource) };
    }
}
