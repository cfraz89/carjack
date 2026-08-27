//! GPU-side ARGB -> NV12 conversion for the compositor.
//!
//! Some hardware encoders import an NV12 dmabuf directly but have no postproc element to convert
//! with: Qualcomm's venus (`v4l2h264enc`) takes `video/x-raw(memory:DMABuf), drm-format=NV12`, but
//! `v4l2convert` only exists on SoCs with an m2m scaler, which Qualcomm has not - so without this,
//! the whole board falls back to reading every frame back to system memory.
//!
//! The awkward part is allocating the NV12 buffer at all: GBM refuses `NV12` on Adreno for every
//! usage flag. So the buffer is allocated as a *linear* `R8` surface `height * 3 / 2` rows tall,
//! and its two regions are imported as separate EGLImages:
//!
//! ```text
//!   offset 0            R8,   width x height       -> luma,   one byte per pixel
//!   offset stride*h     GR88, width/2 x height/2   -> chroma, Cb in .r, Cr in .g
//! ```
//!
//! Linear means those bytes already *are* the NV12 layout, so the same allocation goes to the
//! encoder as `drm-format=NV12` with the two plane offsets spelled out - one fd, no copy.
//!
//! Verified on Adreno (msm/freedreno): both regions come back as complete framebuffers, writes
//! land at the expected byte offsets, and `v4l2h264enc` accepts the result.

use eyre::{Result, eyre};
use smithay::backend::allocator::dmabuf::{AsDmabuf, Dmabuf, DmabufFlags};
use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice};
use smithay::backend::allocator::{Allocator, Buffer as _, Fourcc, Modifier};
use smithay::backend::drm::DrmDeviceFd;
use smithay::backend::egl::EGLDisplay;
use smithay::backend::egl::ffi::egl as egl_ffi;
use smithay::backend::egl::ffi::egl::types::EGLImage;
use smithay::backend::renderer::gles::{GlesRenderer, ffi};
use std::ffi::CString;

/// Rows of chroma per row of luma: NV12 is 4:2:0, so the buffer is 1.5x the image height.
const NV12_ROWS_NUM: u32 = 3;
const NV12_ROWS_DEN: u32 = 2;

/// A fullscreen triangle strip in clip space, reused by both passes.
const QUAD: [f32; 8] = [-1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0];

const VERTEX_SHADER: &str = "\
#version 100
attribute vec2 position;
varying vec2 v_uv;
void main() {
    // Same y direction in and out, so the image keeps the orientation the compositor drew.
    v_uv = position * 0.5 + 0.5;
    gl_Position = vec4(position, 0.0, 1.0);
}
";

/// Luma pass: one byte per pixel into the R8 region.
const LUMA_SHADER: &str = "\
#version 100
precision mediump float;
varying vec2 v_uv;
uniform sampler2D tex;
uniform vec3 coeff;
void main() {
    vec3 rgb = texture2D(tex, v_uv).rgb;
    // Limited range: 16-235 over 0-255, hence the 16/255 offset and pre-scaled coefficients.
    gl_FragColor = vec4(dot(rgb, coeff) + 0.0625, 0.0, 0.0, 1.0);
}
";

/// Chroma pass: Cb in `.r`, Cr in `.g`, which land in a GR88 target as byte 0 and byte 1 - the
/// NV12 interleave.
const CHROMA_SHADER: &str = "\
#version 100
precision mediump float;
varying vec2 v_uv;
uniform sampler2D tex;
uniform vec3 cb_coeff;
uniform vec3 cr_coeff;
void main() {
    // Bilinear sampling at half resolution averages each 2x2 block, which is the 4:2:0 subsample.
    vec3 rgb = texture2D(tex, v_uv).rgb;
    gl_FragColor = vec4(dot(rgb, cb_coeff) + 0.5, dot(rgb, cr_coeff) + 0.5, 0.0, 1.0);
}
";

/// RGB -> Y'CbCr coefficients, limited range, pre-scaled so the shader is three dot products.
struct Matrix {
    name: &'static str,
    luma: [f32; 3],
    cb: [f32; 3],
    cr: [f32; 3],
}

const BT601: Matrix = Matrix {
    name: "BT.601",
    luma: [0.257, 0.504, 0.098],
    cb: [-0.148, -0.291, 0.439],
    cr: [0.439, -0.368, -0.071],
};

const BT709: Matrix = Matrix {
    name: "BT.709",
    luma: [0.183, 0.614, 0.062],
    cb: [-0.101, -0.339, 0.439],
    cr: [0.439, -0.399, -0.040],
};

/// Which matrix to use for a frame this tall, matching GStreamer's own default so the picture
/// does not shift when a machine moves between this path and `videoconvert`/`vapostproc`.
fn matrix_for(height: u32) -> &'static Matrix {
    if height > 576 { &BT709 } else { &BT601 }
}

/// One EGLImage-backed GL texture bound to a framebuffer - half of an NV12 target.
struct Plane {
    image: EGLImage,
    texture: u32,
    fbo: u32,
    width: u32,
    height: u32,
}

/// An NV12 render target: the two framebuffers to draw into, plus the same memory described as
/// NV12 for the encoder.
struct Target {
    /// Handed to the encoder: `Fourcc::Nv12`, two planes, both on the one allocation's fd.
    dmabuf: Dmabuf,
    luma: Plane,
    chroma: Plane,
}

/// The compositor's conversion pass: the ARGB render target imported once as a source texture,
/// a pool of NV12 targets to draw into, and the two shader programs between them.
pub struct Nv12Converter {
    program_luma: u32,
    program_chroma: u32,
    vbo: u32,
    /// The compositor's ARGB render target, imported once and reused every frame.
    source: Plane,
    targets: Vec<Target>,
    next: usize,
    matrix: &'static Matrix,
}

impl Nv12Converter {
    /// Build the conversion for a compositor rendering `width`x`height` into `argb`, with `pool`
    /// NV12 targets to cycle through - GStreamer holds a published buffer until it is consumed.
    pub fn new(
        renderer: &mut GlesRenderer,
        allocator: &mut GbmAllocator<DrmDeviceFd>,
        argb: &Dmabuf,
        width: u32,
        height: u32,
        pool: usize,
    ) -> Result<Self> {
        let mut targets = Vec::with_capacity(pool);
        for _ in 0..pool {
            targets.push(allocate_target(renderer, allocator, width, height)?);
        }
        let source = import_plane(renderer, argb, argb.width(), argb.height())?;
        let (program_luma, program_chroma, vbo) = renderer
            .with_context(|gl| unsafe {
                let luma = build_program(gl, VERTEX_SHADER, LUMA_SHADER)?;
                let chroma = build_program(gl, VERTEX_SHADER, CHROMA_SHADER)?;
                let mut vbo = 0;
                gl.GenBuffers(1, &mut vbo);
                gl.BindBuffer(ffi::ARRAY_BUFFER, vbo);
                gl.BufferData(
                    ffi::ARRAY_BUFFER,
                    size_of_val(&QUAD) as isize,
                    QUAD.as_ptr() as *const _,
                    ffi::STATIC_DRAW,
                );
                gl.BindBuffer(ffi::ARRAY_BUFFER, 0);
                Ok::<_, eyre::Report>((luma, chroma, vbo))
            })
            .map_err(|e| eyre!("make context current: {e}"))??;

        let matrix = matrix_for(height);
        tracing::info!(
            "compositor: NV12 conversion on the GPU ({}x{}, {} limited range, {pool} targets)",
            width,
            height,
            matrix.name
        );
        Ok(Self {
            program_luma,
            program_chroma,
            vbo,
            source,
            targets,
            next: 0,
            matrix,
        })
    }

    /// Convert what the compositor just rendered into the next NV12 target, and return that
    /// target's dmabuf to publish. The caller must have finished rendering into the ARGB buffer.
    pub fn convert(&mut self, renderer: &mut GlesRenderer) -> Result<Dmabuf> {
        let idx = self.next;
        self.next = (self.next + 1) % self.targets.len();
        let target = &self.targets[idx];
        let (src_tex, vbo) = (self.source.texture, self.vbo);
        let (program_luma, program_chroma) = (self.program_luma, self.program_chroma);
        let matrix = self.matrix;

        renderer
            .with_context(|gl| unsafe {
                // Start from known state - a stale scissor or an enabled blend left by the
                // compositor's own render would quietly corrupt the conversion.
                gl.Disable(ffi::BLEND);
                gl.Disable(ffi::SCISSOR_TEST);
                gl.Disable(ffi::DEPTH_TEST);
                gl.Disable(ffi::CULL_FACE);
                gl.ColorMask(ffi::TRUE, ffi::TRUE, ffi::TRUE, ffi::TRUE);

                gl.BindBuffer(ffi::ARRAY_BUFFER, vbo);
                gl.ActiveTexture(ffi::TEXTURE0);
                gl.BindTexture(ffi::TEXTURE_2D, src_tex);

                draw_pass(gl, program_luma, &target.luma, vbo, |gl, program| {
                    set_vec3(gl, program, c"coeff", matrix.luma);
                });
                draw_pass(gl, program_chroma, &target.chroma, vbo, |gl, program| {
                    set_vec3(gl, program, c"cb_coeff", matrix.cb);
                    set_vec3(gl, program, c"cr_coeff", matrix.cr);
                });

                // The encoder is a different device on the same memory, so the writes have to have
                // landed before it is told the buffer is ready.
                gl.Finish();

                gl.BindFramebuffer(ffi::FRAMEBUFFER, 0);
                gl.BindTexture(ffi::TEXTURE_2D, 0);
                gl.BindBuffer(ffi::ARRAY_BUFFER, 0);
                gl.UseProgram(0);
            })
            .map_err(|e| eyre!("nv12 conversion: {e}"))?;
        Ok(target.dmabuf.clone())
    }

    /// Free every GL object and EGLImage. They outlive any single frame, and the pool is rebuilt
    /// whenever the accessory changes the display size.
    pub fn destroy(mut self, renderer: &mut GlesRenderer) {
        let display = renderer.egl_context().display().clone();
        let _ = renderer.with_context(|gl| unsafe {
            for target in &self.targets {
                target.luma.destroy(gl, &display);
                target.chroma.destroy(gl, &display);
            }
            self.source.destroy(gl, &display);
            gl.DeleteProgram(self.program_luma);
            gl.DeleteProgram(self.program_chroma);
            gl.DeleteBuffers(1, &self.vbo);
        });
        self.targets.clear();
    }
}

impl Plane {
    /// Release the GL objects and the EGLImage behind them - leaking these costs a frame's worth
    /// of GPU memory per reconfigure.
    ///
    /// # Safety
    /// The renderer's context must be current.
    unsafe fn destroy(&self, gl: &ffi::Gles2, display: &EGLDisplay) {
        unsafe {
            gl.DeleteFramebuffers(1, &self.fbo);
            gl.DeleteTextures(1, &self.texture);
            egl_ffi::DestroyImageKHR(**display.get_display_handle(), self.image);
        }
    }
}

/// Draw one fullscreen pass into `plane`.
///
/// # Safety
/// The renderer's context must be current and `vbo` must hold [`QUAD`].
unsafe fn draw_pass(
    gl: &ffi::Gles2,
    program: u32,
    plane: &Plane,
    vbo: u32,
    uniforms: impl FnOnce(&ffi::Gles2, u32),
) {
    unsafe {
        gl.BindFramebuffer(ffi::FRAMEBUFFER, plane.fbo);
        gl.Viewport(0, 0, plane.width as i32, plane.height as i32);
        gl.UseProgram(program);
        uniforms(gl, program);
        let tex_loc = gl.GetUniformLocation(program, c"tex".as_ptr());
        gl.Uniform1i(tex_loc, 0);
        let pos = gl.GetAttribLocation(program, c"position".as_ptr()) as u32;
        gl.BindBuffer(ffi::ARRAY_BUFFER, vbo);
        gl.EnableVertexAttribArray(pos);
        gl.VertexAttribPointer(pos, 2, ffi::FLOAT, ffi::FALSE, 0, std::ptr::null());
        gl.DrawArrays(ffi::TRIANGLE_STRIP, 0, 4);
        gl.DisableVertexAttribArray(pos);
    }
}

/// # Safety
/// The renderer's context must be current.
unsafe fn set_vec3(gl: &ffi::Gles2, program: u32, name: &std::ffi::CStr, v: [f32; 3]) {
    unsafe {
        let loc = gl.GetUniformLocation(program, name.as_ptr());
        gl.Uniform3f(loc, v[0], v[1], v[2]);
    }
}

/// Allocate one NV12 target: a linear R8 surface 1.5x as tall as the image, its two regions
/// imported as framebuffers, and an NV12 description of the whole thing for the encoder.
fn allocate_target(
    renderer: &mut GlesRenderer,
    allocator: &mut GbmAllocator<DrmDeviceFd>,
    width: u32,
    height: u32,
) -> Result<Target> {
    let rows = height * NV12_ROWS_NUM / NV12_ROWS_DEN;
    // Linear is not a preference: the scheme rests on the bytes being exactly where NV12 says
    // they are, which no tiled modifier gives.
    let buffer = allocator
        .create_buffer(width, rows, Fourcc::R8, &[Modifier::Linear])
        .map_err(|e| eyre!("allocate {width}x{rows} R8 buffer for NV12: {e}"))?;
    let surface = buffer
        .export()
        .map_err(|e| eyre!("export NV12 buffer: {e}"))?;
    if surface.format().modifier != Modifier::Linear {
        return Err(eyre!(
            "NV12 buffer came back with modifier {:?}, not Linear - its bytes would not be NV12",
            surface.format().modifier
        ));
    }
    let stride = surface
        .strides()
        .next()
        .ok_or_else(|| eyre!("NV12 buffer has no plane"))?;
    let fd = surface
        .handles()
        .next()
        .ok_or_else(|| eyre!("NV12 buffer has no fd"))?;
    let uv_offset = stride * height;

    // The same memory as the encoder wants it: one fd, luma at 0, chroma at `uv_offset`.
    let mut builder = Dmabuf::builder(
        (width as i32, height as i32),
        Fourcc::Nv12,
        Modifier::Linear,
        DmabufFlags::empty(),
    );
    for (idx, offset) in [(0, 0), (1, uv_offset)] {
        let dup = fd
            .try_clone_to_owned()
            .map_err(|e| eyre!("dup NV12 fd: {e}"))?;
        builder.add_plane(dup, idx, offset, stride);
    }
    let dmabuf = builder
        .build()
        .ok_or_else(|| eyre!("build NV12 dmabuf description"))?;

    let luma = import_region(renderer, &surface, Fourcc::R8, width, height, 0, stride)?;
    let chroma = import_region(
        renderer,
        &surface,
        Fourcc::Gr88,
        width / 2,
        height / 2,
        uv_offset,
        stride,
    )?;
    Ok(Target {
        dmabuf,
        luma,
        chroma,
    })
}

/// Import one region of `surface` - a byte range, reinterpreted as `fourcc` at `width`x`height` -
/// as a texture bound to a framebuffer we can render into.
fn import_region(
    renderer: &mut GlesRenderer,
    surface: &Dmabuf,
    fourcc: Fourcc,
    width: u32,
    height: u32,
    offset: u32,
    stride: u32,
) -> Result<Plane> {
    let fd = surface
        .handles()
        .next()
        .ok_or_else(|| eyre!("surface has no fd"))?
        .try_clone_to_owned()
        .map_err(|e| eyre!("dup surface fd: {e}"))?;
    let mut builder = Dmabuf::builder(
        (width as i32, height as i32),
        fourcc,
        Modifier::Linear,
        DmabufFlags::empty(),
    );
    builder.add_plane(fd, 0, offset, stride);
    let region = builder
        .build()
        .ok_or_else(|| eyre!("describe {fourcc:?} region at offset {offset}"))?;
    import_plane(renderer, &region, width, height)
}

/// Import a dmabuf as an EGLImage-backed texture with a framebuffer attached.
fn import_plane(
    renderer: &mut GlesRenderer,
    dmabuf: &Dmabuf,
    width: u32,
    height: u32,
) -> Result<Plane> {
    let image = renderer
        .egl_context()
        .display()
        .create_image_from_dmabuf(dmabuf)
        .map_err(|e| eyre!("EGLImage from {:?} dmabuf: {e}", dmabuf.format().code))?;
    let plane = renderer
        .with_context(|gl| unsafe {
            let mut texture = 0;
            gl.GenTextures(1, &mut texture);
            gl.BindTexture(ffi::TEXTURE_2D, texture);
            gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MIN_FILTER, ffi::LINEAR as i32);
            gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MAG_FILTER, ffi::LINEAR as i32);
            gl.TexParameteri(
                ffi::TEXTURE_2D,
                ffi::TEXTURE_WRAP_S,
                ffi::CLAMP_TO_EDGE as i32,
            );
            gl.TexParameteri(
                ffi::TEXTURE_2D,
                ffi::TEXTURE_WRAP_T,
                ffi::CLAMP_TO_EDGE as i32,
            );
            gl.EGLImageTargetTexture2DOES(ffi::TEXTURE_2D, image);
            let err = gl.GetError();
            if err != ffi::NO_ERROR {
                return Err(eyre!("bind EGLImage to texture: GL error {err:#x}"));
            }
            let mut fbo = 0;
            gl.GenFramebuffers(1, &mut fbo);
            gl.BindFramebuffer(ffi::FRAMEBUFFER, fbo);
            gl.FramebufferTexture2D(
                ffi::FRAMEBUFFER,
                ffi::COLOR_ATTACHMENT0,
                ffi::TEXTURE_2D,
                texture,
                0,
            );
            let status = gl.CheckFramebufferStatus(ffi::FRAMEBUFFER);
            gl.BindFramebuffer(ffi::FRAMEBUFFER, 0);
            gl.BindTexture(ffi::TEXTURE_2D, 0);
            if status != ffi::FRAMEBUFFER_COMPLETE {
                gl.DeleteFramebuffers(1, &fbo);
                gl.DeleteTextures(1, &texture);
                // A GPU can hold the format but refuse to render into it, which is the whole
                // question this path turns on - so name the format.
                return Err(eyre!(
                    "{:?} is not renderable on this GPU (framebuffer status {status:#x})",
                    dmabuf.format().code
                ));
            }
            Ok(Plane {
                image,
                texture,
                fbo,
                width,
                height,
            })
        })
        .map_err(|e| eyre!("make context current: {e}"))??;
    Ok(plane)
}

/// # Safety
/// The renderer's context must be current.
unsafe fn build_program(gl: &ffi::Gles2, vertex: &str, fragment: &str) -> Result<u32> {
    unsafe {
        let vs = compile(gl, ffi::VERTEX_SHADER, vertex)?;
        let fs = compile(gl, ffi::FRAGMENT_SHADER, fragment)?;
        let program = gl.CreateProgram();
        gl.AttachShader(program, vs);
        gl.AttachShader(program, fs);
        gl.LinkProgram(program);
        gl.DeleteShader(vs);
        gl.DeleteShader(fs);
        let mut status = 0;
        gl.GetProgramiv(program, ffi::LINK_STATUS, &mut status);
        if status == 0 {
            return Err(eyre!("link NV12 shader program: {}", info_log(gl, program)));
        }
        Ok(program)
    }
}

/// # Safety
/// The renderer's context must be current.
unsafe fn compile(gl: &ffi::Gles2, kind: u32, source: &str) -> Result<u32> {
    unsafe {
        let shader = gl.CreateShader(kind);
        let src = CString::new(source).map_err(|e| eyre!("shader source: {e}"))?;
        gl.ShaderSource(shader, 1, &src.as_ptr(), std::ptr::null());
        gl.CompileShader(shader);
        let mut status = 0;
        gl.GetShaderiv(shader, ffi::COMPILE_STATUS, &mut status);
        if status == 0 {
            let mut len = 0;
            gl.GetShaderiv(shader, ffi::INFO_LOG_LENGTH, &mut len);
            let mut buf = vec![0u8; len.max(1) as usize];
            gl.GetShaderInfoLog(
                shader,
                len,
                std::ptr::null_mut(),
                buf.as_mut_ptr() as *mut _,
            );
            return Err(eyre!(
                "compile NV12 shader: {}",
                String::from_utf8_lossy(&buf).trim_end_matches('\0')
            ));
        }
        Ok(shader)
    }
}

/// # Safety
/// The renderer's context must be current.
unsafe fn info_log(gl: &ffi::Gles2, program: u32) -> String {
    unsafe {
        let mut len = 0;
        gl.GetProgramiv(program, ffi::INFO_LOG_LENGTH, &mut len);
        let mut buf = vec![0u8; len.max(1) as usize];
        gl.GetProgramInfoLog(
            program,
            len,
            std::ptr::null_mut(),
            buf.as_mut_ptr() as *mut _,
        );
        String::from_utf8_lossy(&buf)
            .trim_end_matches('\0')
            .to_string()
    }
}

/// Whether this GPU can do the conversion at all. Asked at startup, before a backend that needs it
/// is chosen: "no zero-copy backend" beats a compositor that comes up and cannot produce a frame.
/// A 64x64 allocation and two EGL imports - the operations the real pool depends on.
pub fn supported(render_node: &str) -> bool {
    match probe(render_node) {
        Ok(()) => true,
        Err(e) => {
            tracing::debug!("{render_node} cannot render NV12 in the compositor: {e}");
            false
        }
    }
}

fn probe(render_node: &str) -> Result<()> {
    let (gbm, mut renderer) = open_gpu(render_node)?;
    let mut allocator = GbmAllocator::new(gbm, GbmBufferFlags::RENDERING);
    let target = allocate_target(&mut renderer, &mut allocator, 64, 64)?;
    let display = renderer.egl_context().display().clone();
    let _ = renderer.with_context(|gl| unsafe {
        target.luma.destroy(gl, &display);
        target.chroma.destroy(gl, &display);
    });
    Ok(())
}

/// Open a render node and bring up a GLES renderer on it - the same pair the compositor runs on.
/// Public for the startup probe above and for the tests that drive the conversion directly.
pub fn open_gpu(render_node: &str) -> Result<(GbmDevice<DrmDeviceFd>, GlesRenderer)> {
    let file = crate::compositor::open_render_node(render_node)?;
    let device_fd = DrmDeviceFd::new(smithay::utils::DeviceFd::from(std::os::fd::OwnedFd::from(
        file,
    )));
    let gbm = GbmDevice::new(device_fd).map_err(|e| eyre!("create GbmDevice: {e}"))?;
    let renderer = crate::compositor::make_renderer(&gbm)?;
    Ok((gbm, renderer))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two matrices have to agree on grey: an achromatic input must put both chroma channels
    /// at the 0.5 midpoint, or the picture comes out tinted.
    #[test]
    fn grey_is_achromatic_in_both_matrices() {
        for m in [&BT601, &BT709] {
            let grey = [0.5f32; 3];
            let dot = |c: [f32; 3]| c[0] * grey[0] + c[1] * grey[1] + c[2] * grey[2];
            assert!(dot(m.cb).abs() < 0.001, "{} Cb {}", m.name, dot(m.cb));
            assert!(dot(m.cr).abs() < 0.001, "{} Cr {}", m.name, dot(m.cr));
        }
    }

    /// Full-scale white lands at the top of the limited range (235/255), not at 1.0 - the
    /// coefficients carry that scaling, so a typo shows up here.
    #[test]
    fn white_lands_at_the_top_of_the_limited_range() {
        for m in [&BT601, &BT709] {
            let y = m.luma.iter().sum::<f32>() + 0.0625;
            assert!(
                (y - 235.0 / 255.0).abs() < 0.002,
                "{} white = {y}, expected {}",
                m.name,
                235.0 / 255.0
            );
        }
    }

    #[test]
    fn sd_uses_bt601_and_hd_uses_bt709() {
        assert_eq!(matrix_for(480).name, "BT.601");
        assert_eq!(matrix_for(576).name, "BT.601");
        assert_eq!(matrix_for(720).name, "BT.709");
        assert_eq!(matrix_for(1080).name, "BT.709");
    }
}
