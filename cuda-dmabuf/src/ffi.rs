//! Raw FFI bindings for the dmabuf -> CUDA -> GstCudaMemory importer.
//!
//! Everything is resolved at runtime with `libloading` rather than linked, because the build
//! happens inside the toolbox (where the NVIDIA driver / GStreamer CUDA plugin libraries are
//! absent) while carjack runs on the host (where they exist). dlopen keeps the crate buildable
//! in the toolbox and binds the real symbols on the host.
#![allow(non_snake_case, non_camel_case_types)]

use std::ffi::{c_char, c_int, c_void};

use eyre::{Context, Result, eyre};
use libloading::{Library, Symbol};

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

pub const O_RDWR: c_int = 2;
pub const O_CLOEXEC: c_int = 0o2000000;

// CUmemorytype
pub const CU_MEMORYTYPE_DEVICE: u32 = 2;
pub const CU_MEMORYTYPE_ARRAY: u32 = 3;

// CUeglFrameType
pub const CU_EGL_FRAME_TYPE_ARRAY: u32 = 0;
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

/// Mirrors CUDA's `CUDA_MEMCPY2D_st` (v2). `#[repr(C)]` reproduces the C padding (the u32
/// `*MemoryType` fields pad to 8 before the following pointer/u64 fields).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CudaMemcpy2D {
    pub src_x_in_bytes: usize,
    pub src_y: usize,
    pub src_memory_type: u32,
    pub src_host: *const c_void,
    pub src_device: u64, // CUdeviceptr
    pub src_array: *mut c_void,
    pub src_pitch: usize,
    pub dst_x_in_bytes: usize,
    pub dst_y: usize,
    pub dst_memory_type: u32,
    pub dst_host: *mut c_void,
    pub dst_device: u64, // CUdeviceptr
    pub dst_array: *mut c_void,
    pub dst_pitch: usize,
    pub width_in_bytes: usize,
    pub height: usize,
}

impl Default for CudaMemcpy2D {
    fn default() -> Self {
        // SAFETY: all-zero is a valid CUDA_MEMCPY2D (unset fields ignored per memory type).
        unsafe { std::mem::zeroed() }
    }
}

// ---------- function pointer types ----------
type EglGetProcAddress = unsafe extern "C" fn(*const c_char) -> *mut c_void;
type EglInitialize = unsafe extern "C" fn(*mut c_void, *mut i32, *mut i32) -> u32;
type EglGetError = unsafe extern "C" fn() -> i32;
type EglGetPlatformDisplayEXT =
    unsafe extern "C" fn(u32, *mut c_void, *const i32) -> *mut c_void;
type EglCreateImageKHR =
    unsafe extern "C" fn(*mut c_void, *mut c_void, u32, *mut c_void, *const i32) -> *mut c_void;
type EglDestroyImageKHR = unsafe extern "C" fn(*mut c_void, *mut c_void) -> u32;

type GbmCreateDevice = unsafe extern "C" fn(c_int) -> *mut c_void;

type CuInit = unsafe extern "C" fn(u32) -> i32;
type CuGraphicsEGLRegisterImage =
    unsafe extern "C" fn(*mut *mut c_void, *mut c_void, u32) -> i32;
type CuGraphicsResourceGetMappedEglFrame =
    unsafe extern "C" fn(*mut CUeglFrame, *mut c_void, u32, u32) -> i32;
type CuGraphicsUnregisterResource = unsafe extern "C" fn(*mut c_void) -> i32;
type CuMemAlloc = unsafe extern "C" fn(*mut u64, usize) -> i32;
pub type CuMemFree = unsafe extern "C" fn(u64) -> i32;
type CuMemcpy2D = unsafe extern "C" fn(*const CudaMemcpy2D) -> i32;
type CuGetErrorString = unsafe extern "C" fn(i32, *mut *const c_char) -> i32;

type GstCudaContextNew = unsafe extern "C" fn(u32) -> *mut c_void;
type GstCudaContextGetHandle = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
pub type GstCudaContextPush = unsafe extern "C" fn(*mut c_void) -> i32;
pub type GstCudaContextPop = unsafe extern "C" fn(*mut *mut c_void) -> i32;
type GstCudaAllocatorAllocWrapped = unsafe extern "C" fn(
    *mut c_void,                       // GstCudaAllocator* (NULL => default)
    *mut c_void,                       // GstCudaContext*
    *mut c_void,                       // CUstream
    *const c_void,                     // const GstVideoInfo*
    u64,                               // CUdeviceptr
    *mut c_void,                       // user_data
    Option<extern "C" fn(*mut c_void)>, // GDestroyNotify
) -> *mut c_void; // GstMemory*
type GstContextNewCudaContext = unsafe extern "C" fn(*mut c_void) -> *mut c_void;

/// Turn a `Symbol` into a `'static` function pointer. Sound as long as the owning `Library`
/// outlives every use — which it does here (both live for the process in [`Libs`]).
unsafe fn sym<T: Copy>(lib: &Library, name: &[u8]) -> Result<T> {
    let s: Symbol<T> =
        unsafe { lib.get(name) }.wrap_err_with(|| format!("symbol {:?}", String::from_utf8_lossy(name)))?;
    // Detach the lifetime: RawSymbol derefs to T and outlives the borrow (the Library is leaked).
    Ok(*s.into_raw())
}

/// All dlopen'd libraries and the resolved entry points. Libraries are kept alive for the
/// lifetime of the process (leaked in [`Libs::load`]) so the function pointers stay valid.
pub struct Libs {
    pub egl_initialize: EglInitialize,
    pub egl_get_error: EglGetError,
    pub egl_get_platform_display: EglGetPlatformDisplayEXT,
    pub egl_create_image: EglCreateImageKHR,
    pub egl_destroy_image: EglDestroyImageKHR,

    pub gbm_create_device: GbmCreateDevice,

    pub cu_init: CuInit,
    pub cu_register_image: CuGraphicsEGLRegisterImage,
    pub cu_get_egl_frame: CuGraphicsResourceGetMappedEglFrame,
    pub cu_unregister: CuGraphicsUnregisterResource,
    pub cu_mem_alloc: CuMemAlloc,
    pub cu_mem_free: CuMemFree,
    pub cu_memcpy_2d: CuMemcpy2D,
    pub cu_get_error_string: CuGetErrorString,

    pub gst_cuda_context_new: GstCudaContextNew,
    pub gst_cuda_context_get_handle: GstCudaContextGetHandle,
    pub gst_cuda_context_push: GstCudaContextPush,
    pub gst_cuda_context_pop: GstCudaContextPop,
    pub gst_cuda_alloc_wrapped: GstCudaAllocatorAllocWrapped,
    pub gst_context_new_cuda_context: GstContextNewCudaContext,
}

impl Libs {
    pub fn load() -> Result<&'static Libs> {
        // Leak the libraries so they (and the function pointers into them) live for the whole
        // process; the importer is a process-long singleton anyway.
        let egl = Box::leak(Box::new(open("libEGL.so.1")?));
        let gbm = Box::leak(Box::new(open("libgbm.so.1")?));
        let cuda = Box::leak(Box::new(open("libcuda.so.1")?));
        let gstcuda = Box::leak(Box::new(open("libgstcuda-1.0.so.0")?));

        unsafe {
            // EGL extension entry points come through eglGetProcAddress.
            let get_proc: EglGetProcAddress = sym(egl, b"eglGetProcAddress")?;
            let get_ext = |name: &[u8]| -> Result<*mut c_void> {
                let p = get_proc(name.as_ptr() as *const c_char);
                if p.is_null() {
                    Err(eyre!("eglGetProcAddress({}) returned null", String::from_utf8_lossy(name)))
                } else {
                    Ok(p)
                }
            };

            let libs = Libs {
                egl_initialize: sym(egl, b"eglInitialize")?,
                egl_get_error: sym(egl, b"eglGetError")?,
                egl_get_platform_display: std::mem::transmute::<*mut c_void, EglGetPlatformDisplayEXT>(
                    get_ext(b"eglGetPlatformDisplayEXT\0")?,
                ),
                egl_create_image: std::mem::transmute::<*mut c_void, EglCreateImageKHR>(
                    get_ext(b"eglCreateImageKHR\0")?,
                ),
                egl_destroy_image: std::mem::transmute::<*mut c_void, EglDestroyImageKHR>(
                    get_ext(b"eglDestroyImageKHR\0")?,
                ),

                gbm_create_device: sym(gbm, b"gbm_create_device")?,

                cu_init: sym(cuda, b"cuInit")?,
                cu_register_image: sym(cuda, b"cuGraphicsEGLRegisterImage")?,
                cu_get_egl_frame: sym(cuda, b"cuGraphicsResourceGetMappedEglFrame")?,
                cu_unregister: sym(cuda, b"cuGraphicsUnregisterResource")?,
                cu_mem_alloc: sym(cuda, b"cuMemAlloc_v2")?,
                cu_mem_free: sym(cuda, b"cuMemFree_v2")?,
                cu_memcpy_2d: sym(cuda, b"cuMemcpy2D_v2")?,
                cu_get_error_string: sym(cuda, b"cuGetErrorString")?,

                gst_cuda_context_new: sym(gstcuda, b"gst_cuda_context_new")?,
                gst_cuda_context_get_handle: sym(gstcuda, b"gst_cuda_context_get_handle")?,
                gst_cuda_context_push: sym(gstcuda, b"gst_cuda_context_push")?,
                gst_cuda_context_pop: sym(gstcuda, b"gst_cuda_context_pop")?,
                gst_cuda_alloc_wrapped: sym(gstcuda, b"gst_cuda_allocator_alloc_wrapped")?,
                gst_context_new_cuda_context: sym(gstcuda, b"gst_context_new_cuda_context")?,
            };
            Ok(Box::leak(Box::new(libs)))
        }
    }

    /// Human-readable CUDA driver error string.
    pub fn cu_error(&self, code: i32) -> String {
        let mut s: *const c_char = std::ptr::null();
        unsafe { (self.cu_get_error_string)(code, &mut s) };
        if s.is_null() {
            format!("CUDA error {code}")
        } else {
            unsafe { std::ffi::CStr::from_ptr(s) }.to_string_lossy().into_owned()
        }
    }
}

fn open(name: &str) -> Result<Library> {
    unsafe { Library::new(name) }.wrap_err_with(|| format!("dlopen {name}"))
}
