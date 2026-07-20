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

use eyre::{Result, eyre};
use gstreamer as gst;
use gstreamer_video as gst_video;
use gst::glib::translate::{FromGlibPtrFull, ToGlibPtr};

mod ffi;
use ffi::Libs;

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
    libs: &'static Libs,
    _drm: File,
    egl_display: *mut c_void,
    /// `GstCudaContext*` (owned ref) — shared with `nvh264enc` via [`gst_context`](Self::gst_context)
    /// so the wrapped memory and the encoder live on the same CUDA context.
    gst_cuda_ctx: *mut c_void,
}

// The raw pointers are a CUDA/EGL/GStreamer context bound to one render node; moving the importer
// between threads is fine (CUDA context is made current per-call via gst_cuda_context_push/pop).
unsafe impl Send for CudaImporter {}

struct NotifyData {
    free: ffi::CuMemFree,
    push: ffi::GstCudaContextPush,
    pop: ffi::GstCudaContextPop,
    gst_ctx: *mut c_void,
    devptr: u64,
}

/// GDestroyNotify for the wrapped GstCudaMemory: free the pitched CUDA buffer, making its
/// context current first (this may run on nvh264enc's streaming thread).
extern "C" fn free_devptr(data: *mut c_void) {
    let d = unsafe { Box::from_raw(data as *mut NotifyData) };
    unsafe {
        (d.push)(d.gst_ctx);
        (d.free)(d.devptr);
        (d.pop)(ptr::null_mut());
    }
}

impl CudaImporter {
    pub fn new(render_node: &str) -> Result<Self> {
        let libs = Libs::load()?;
        // GStreamer + CUDA must be initialised before we make a GstCudaContext.
        let _ = gst::init();
        let r = unsafe { (libs.cu_init)(0) };
        if r != 0 {
            return Err(eyre!("cuInit failed: {}", libs.cu_error(r)));
        }

        // Own DRM fd + gbm device so we have an EGLDisplay on the NVIDIA node independent of the
        // compositor's. (EGL_NO_CONTEXT image creation only needs the display.)
        let drm = File::options()
            .read(true)
            .write(true)
            .open(render_node)
            .map_err(|e| eyre!("open {render_node}: {e}"))?;
        let gbm = unsafe { (libs.gbm_create_device)(drm.as_raw_fd()) };
        if gbm.is_null() {
            return Err(eyre!("gbm_create_device({render_node}) failed"));
        }
        let egl_display =
            unsafe { (libs.egl_get_platform_display)(ffi::EGL_PLATFORM_GBM_KHR, gbm, ptr::null()) };
        if egl_display.is_null() {
            return Err(eyre!("eglGetPlatformDisplayEXT failed: 0x{:x}", unsafe {
                (libs.egl_get_error)()
            }));
        }
        let (mut major, mut minor) = (0i32, 0i32);
        if unsafe { (libs.egl_initialize)(egl_display, &mut major, &mut minor) } == 0 {
            return Err(eyre!("eglInitialize failed: 0x{:x}", unsafe {
                (libs.egl_get_error)()
            }));
        }
        tracing::info!("cuda-dmabuf: EGL {major}.{minor} on {render_node}");

        // Device 0 = the NVIDIA GPU (single CUDA device on this class of machine). nvh264enc
        // defaults to device 0 too, so sharing this context lines them up.
        let gst_cuda_ctx = unsafe { (libs.gst_cuda_context_new)(0) };
        if gst_cuda_ctx.is_null() {
            return Err(eyre!("gst_cuda_context_new(0) failed"));
        }
        tracing::info!("cuda-dmabuf: importer ready");

        Ok(CudaImporter {
            libs,
            _drm: drm,
            egl_display,
            gst_cuda_ctx,
        })
    }

    /// `GstContext` to set on the encode pipeline so `nvh264enc` adopts our CUDA context (the one
    /// the imported memory lives on).
    pub fn gst_context(&self) -> gst::Context {
        let ctx = unsafe { (self.libs.gst_context_new_cuda_context)(self.gst_cuda_ctx) };
        unsafe { gst::Context::from_glib_full(ctx as *mut gst::ffi::GstContext) }
    }

    /// Import one dmabuf and return a `GstCudaMemory` buffer ready to push at `nvh264enc`.
    pub fn import(&self, plane: &DmabufPlane) -> Result<gst::Buffer> {
        let libs = self.libs;

        // Make our CUDA context current for the duration; pop on return (incl. errors).
        if unsafe { (libs.gst_cuda_context_push)(self.gst_cuda_ctx) } == 0 {
            return Err(eyre!("gst_cuda_context_push failed"));
        }
        let _pop = PopGuard { libs, gst_ctx: self.gst_cuda_ctx };

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
            (libs.egl_create_image)(
                self.egl_display,
                ptr::null_mut(), // EGL_NO_CONTEXT
                ffi::EGL_LINUX_DMA_BUF_EXT,
                ptr::null_mut(),
                attribs.as_ptr(),
            )
        };
        if image.is_null() {
            return Err(eyre!("eglCreateImageKHR failed: 0x{:x}", unsafe {
                (libs.egl_get_error)()
            }));
        }
        let _img = ImageGuard { libs, dpy: self.egl_display, image };

        // 2. Register with CUDA and map to a CUeglFrame (a CUarray for block-linear surfaces).
        let mut resource: *mut c_void = ptr::null_mut();
        let r = unsafe { (libs.cu_register_image)(&mut resource, image, 0) };
        if r != 0 {
            return Err(eyre!("cuGraphicsEGLRegisterImage failed: {}", libs.cu_error(r)));
        }
        let _res = ResGuard { libs, resource };

        let mut frame: ffi::CUeglFrame = unsafe { std::mem::zeroed() };
        let r = unsafe { (libs.cu_get_egl_frame)(&mut frame, resource, 0, 0) };
        if r != 0 {
            return Err(eyre!(
                "cuGraphicsResourceGetMappedEglFrame failed: {}",
                libs.cu_error(r)
            ));
        }

        // 3. Allocate a pitched CUDA buffer and de-tile into it (GPU->GPU). Use BGRA (DRM
        // ARGB8888 in memory order); nvh264enc converts to NV12 itself.
        let info = gst_video::VideoInfo::builder(gst_video::VideoFormat::Bgra, plane.width, plane.height)
            .build()
            .map_err(|e| eyre!("VideoInfo: {e}"))?;
        let pitch = info.stride()[0] as usize;
        let size = pitch * plane.height as usize;

        let mut devptr: u64 = 0;
        let r = unsafe { (libs.cu_mem_alloc)(&mut devptr, size) };
        if r != 0 {
            return Err(eyre!("cuMemAlloc({size}) failed: {}", libs.cu_error(r)));
        }

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
        let r = unsafe { (libs.cu_memcpy_2d)(&copy) };
        if r != 0 {
            unsafe { (libs.cu_mem_free)(devptr) };
            return Err(eyre!("cuMemcpy2D failed: {}", libs.cu_error(r)));
        }
        // Source EGLImage/registration no longer needed; the pitched copy is populated. Guards
        // (_res, _img) unregister + destroy them on scope exit, freeing the source dmabuf to be
        // recycled by the compositor pool.

        // 4. Wrap the pitched buffer as GstCudaMemory on our shared context.
        let notify = Box::new(NotifyData {
            free: libs.cu_mem_free,
            push: libs.gst_cuda_context_push,
            pop: libs.gst_cuda_context_pop,
            gst_ctx: self.gst_cuda_ctx,
            devptr,
        });
        let info_ptr: *const gst_video::ffi::GstVideoInfo = info.to_glib_none().0;
        let mem = unsafe {
            (libs.gst_cuda_alloc_wrapped)(
                ptr::null_mut(),
                self.gst_cuda_ctx,
                ptr::null_mut(),
                info_ptr as *const c_void,
                devptr,
                Box::into_raw(notify) as *mut c_void,
                Some(free_devptr),
            )
        };
        if mem.is_null() {
            // alloc_wrapped failed before taking ownership; free the buffer ourselves. (The
            // notify box leaks in this rare path — acceptable vs. double-free risk.)
            unsafe { (libs.cu_mem_free)(devptr) };
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

// ---- RAII guards so every early return unwinds the CUDA/EGL state ----
struct PopGuard {
    libs: &'static Libs,
    gst_ctx: *mut c_void,
}
impl Drop for PopGuard {
    fn drop(&mut self) {
        let _ = self.gst_ctx;
        unsafe { (self.libs.gst_cuda_context_pop)(ptr::null_mut()) };
    }
}

struct ImageGuard {
    libs: &'static Libs,
    dpy: *mut c_void,
    image: *mut c_void,
}
impl Drop for ImageGuard {
    fn drop(&mut self) {
        unsafe { (self.libs.egl_destroy_image)(self.dpy, self.image) };
    }
}

struct ResGuard {
    libs: &'static Libs,
    resource: *mut c_void,
}
impl Drop for ResGuard {
    fn drop(&mut self) {
        unsafe { (self.libs.cu_unregister)(self.resource) };
    }
}
