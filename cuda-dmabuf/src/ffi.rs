//! Raw FFI bindings for the dmabuf -> CUDA -> GstCudaMemory importer.
//!
//! All native libraries are linked at build time. The crate's `build.rs` emits
//! `cargo:rustc-link-lib=...` for each one; the build toolbox has the required
//! `-devel` packages installed.
//!
//! EGL extension entry points (`eglGetPlatformDisplayEXT`, `eglCreateImageKHR`,
//! `eglDestroyImageKHR`) are NOT directly exported by `libEGL.so` on all
//! implementations — they are resolved at runtime via `eglGetProcAddress` (which
//! IS a core EGL function and is linked).
#![allow(non_snake_case, non_camel_case_types)]

use std::ffi::{c_char, c_int, c_void};

use eyre::{Result, eyre};

// ---------- constants ----------
pub const EGL_PLATFORM_GBM_KHR: u32 = 0x31D7;
pub const EGL_LINUX_DMA_BUF_EXT: u32 = 0x3270;
pub const EGL_NONE: i32 = 0x3038;
pub const EGL_WIDTH: i32 = 0x3057;
pub const EGL_HEIGHT: i32 = 0x3056;
pub const EGL_LINUX_DRM_FOURCC_EXT: i32 = 0x3271;
pub const EGL_DMA_BUF_PLANE0_FD_EXT: i32 = 0x3272;
pub const EGL_DMA_BUF_PLANE0_OFFSET_EXT: i32 = 0x3273;
pub const EGL_DMA_BUF_PLANE0_PITCH_EXT: i32 = 0x3274;
pub const EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT: i32 = 0x3443;
pub const EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT: i32 = 0x3444;

// CUmemorytype
pub const CU_MEMORYTYPE_DEVICE: u32 = 2;
pub const CU_MEMORYTYPE_ARRAY: u32 = 3;

// CUeglFrameType
pub const CU_EGL_FRAME_TYPE_ARRAY: u32 = 0;
/// The pitched (non-tiled) variant. Not matched explicitly (anything that isn't `ARRAY` is
/// treated as pitched device memory in `import`), kept for documentation of the enum.
#[allow(dead_code)]
pub const CU_EGL_FRAME_TYPE_PITCH: u32 = 1;

// ---------- structs ----------
/// Mirrors CUDA's `CUeglFrame_st` (from cudaEGL.h). The leading union is 3 pointers wide.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CUeglFrame {
    pub frame: [*mut c_void; 3], // union { CUarray[3] | void*[3] }
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub pitch: u32,
    pub plane_count: u32,
    pub num_channels: u32,
    pub frame_type: u32,
    pub egl_color_format: u32,
    pub cu_format: u32,
}

/// Mirrors CUDA's `CUDA_MEMCPY2D_st` (v2).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CudaMemcpy2D {
    pub src_x_in_bytes: usize,
    pub src_y: usize,
    pub src_memory_type: u32,
    pub src_host: *const c_void,
    pub src_device: u64,
    pub src_array: *mut c_void,
    pub src_pitch: usize,
    pub dst_x_in_bytes: usize,
    pub dst_y: usize,
    pub dst_memory_type: u32,
    pub dst_host: *mut c_void,
    pub dst_device: u64,
    pub dst_array: *mut c_void,
    pub dst_pitch: usize,
    pub width_in_bytes: usize,
    pub height: usize,
}

impl Default for CudaMemcpy2D {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

// ---------- linked EGL core ----------
#[link(name = "EGL")]
unsafe extern "C" {
    pub fn eglGetProcAddress(name: *const c_char) -> *mut c_void;
    pub fn eglInitialize(dpy: *mut c_void, major: *mut i32, minor: *mut i32) -> u32;
    pub fn eglGetError() -> i32;
}

// ---------- linked GBM ----------
#[link(name = "gbm")]
unsafe extern "C" {
    pub fn gbm_create_device(fd: c_int) -> *mut c_void;
}

// ---------- linked CUDA driver API ----------
#[link(name = "cuda")]
unsafe extern "C" {
    pub fn cuInit(flags: u32) -> i32;
    pub fn cuGraphicsEGLRegisterImage(
        pResource: *mut *mut c_void,
        image: *mut c_void,
        flags: u32,
    ) -> i32;
    pub fn cuGraphicsResourceGetMappedEglFrame(
        pFrame: *mut CUeglFrame,
        hResource: *mut c_void,
        arrayIndex: u32,
        mipLevel: u32,
    ) -> i32;
    pub fn cuGraphicsUnregisterResource(hResource: *mut c_void) -> i32;
    pub fn cuMemAlloc_v2(dptr: *mut u64, bytesize: usize) -> i32;
    pub fn cuMemFree_v2(dptr: u64) -> i32;
    pub fn cuMemcpy2D_v2(pCopy: *const CudaMemcpy2D) -> i32;
    pub fn cuGetErrorString(error: i32, pStr: *mut *const c_char) -> i32;
}

// ---------- linked GStreamer CUDA ----------
#[link(name = "gstcuda-1.0")]
unsafe extern "C" {
    /// Populates libgstcuda's internal CUDA driver vtable (`gst_cuda_vtable`). Normally called by
    /// the nvcodec plugin's `plugin_init`, but we call `gst_cuda_context_new` directly without
    /// having created an nvcodec element yet, so we must load it ourselves first - otherwise
    /// `gst_cuda_context_new` asserts `gst_cuda_vtable.CuInit != nullptr` and aborts.
    pub fn gst_cuda_load_library() -> i32;
    pub fn gst_cuda_context_new(device_id: u32) -> *mut c_void;
    pub fn gst_cuda_context_push(ctx: *mut c_void) -> i32;
    pub fn gst_cuda_context_pop(previous: *mut *mut c_void) -> i32;
    pub fn gst_cuda_allocator_alloc_wrapped(
        allocator: *mut c_void,
        ctx: *mut c_void,
        stream: *mut c_void,
        vinfo: *const c_void,
        devptr: u64,
        user_data: *mut c_void,
        notify: Option<unsafe extern "C" fn(*mut c_void)>,
    ) -> *mut c_void;
    pub fn gst_context_new_cuda_context(ctx: *mut c_void) -> *mut c_void;
}

// ---------- EGL extension entry points (fetched via eglGetProcAddress) ----------

type EglGetPlatformDisplayExtFn =
    unsafe extern "C" fn(u32, *mut c_void, *const i32) -> *mut c_void;
type EglCreateImageKhrFn =
    unsafe extern "C" fn(*mut c_void, *mut c_void, u32, *mut c_void, *const i32) -> *mut c_void;
type EglDestroyImageKhrFn = unsafe extern "C" fn(*mut c_void, *mut c_void) -> u32;

pub struct EglExt {
    pub get_platform_display: EglGetPlatformDisplayExtFn,
    pub create_image: EglCreateImageKhrFn,
    pub destroy_image: EglDestroyImageKhrFn,
}

impl EglExt {
    pub fn load() -> Result<Self> {
        unsafe {
            let gpa = eglGetProcAddress;
            let get_ext = |name: &[u8]| -> Result<*mut c_void> {
                let p = (gpa)(name.as_ptr() as *const c_char);
                if p.is_null() {
                    Err(eyre!(
                        "eglGetProcAddress({}) returned null",
                        String::from_utf8_lossy(name)
                    ))
                } else {
                    Ok(p)
                }
            };

            Ok(EglExt {
                get_platform_display: std::mem::transmute::<*mut c_void, EglGetPlatformDisplayExtFn>(
                    get_ext(b"eglGetPlatformDisplayEXT\0")?,
                ),
                create_image: std::mem::transmute::<*mut c_void, EglCreateImageKhrFn>(
                    get_ext(b"eglCreateImageKHR\0")?,
                ),
                destroy_image: std::mem::transmute::<*mut c_void, EglDestroyImageKhrFn>(
                    get_ext(b"eglDestroyImageKHR\0")?,
                ),
            })
        }
    }
}

/// Human-readable CUDA driver error string.
pub fn cu_error_str(code: i32) -> String {
    let mut s: *const c_char = std::ptr::null();
    unsafe { cuGetErrorString(code, &mut s) };
    if s.is_null() {
        format!("CUDA error {code}")
    } else {
        unsafe { std::ffi::CStr::from_ptr(s) }
            .to_string_lossy()
            .into_owned()
    }
}
