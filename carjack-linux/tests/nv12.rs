//! The compositor's GPU ARGB->NV12 conversion, checked against the bytes it actually writes.
//!
//! This is the path a zero-copy v4l2 encoder is fed from, and nothing downstream would notice a
//! flipped image or swapped chroma - the encoder takes whatever bytes are there. So the test
//! renders a known pattern, maps the NV12 buffer, and reads the luma and chroma planes back.
//!
//! Machine-gated: it needs a render node whose driver can render R8 and GR88. Skips (rather than
//! fails) where that is not true, the way the other GPU tests here do.

use smithay::backend::allocator::dmabuf::{AsDmabuf, Dmabuf};
use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags};
use smithay::backend::allocator::{Allocator, Fourcc, Modifier};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::{Bind, Color32F, Frame, Renderer};
use smithay::utils::{Point, Rectangle, Size, Transform};
use std::os::fd::AsRawFd;

const W: u32 = 256;
const H: u32 = 128;

/// A render node this build can do the conversion on, or `None` to skip.
fn usable_node() -> Option<String> {
    let nodes = std::fs::read_dir("/dev/dri").ok()?;
    let mut paths: Vec<String> = nodes
        .flatten()
        .map(|e| e.path().to_string_lossy().into_owned())
        .filter(|p| p.contains("renderD"))
        .collect();
    paths.sort();
    paths.into_iter().find(|p| carjack::nv12::supported(p))
}

/// Paint `argb` in two horizontal bands, through the same renderer API the compositor draws with,
/// so "the top of the image" means the same thing here as it does there.
fn paint(renderer: &mut GlesRenderer, argb: &mut Dmabuf, top: [f32; 4], bottom: [f32; 4]) {
    let mut fb = renderer.bind(argb).expect("bind argb");
    let size = Size::from((W as i32, H as i32));
    let mut frame = renderer
        .render(&mut fb, size, Transform::Normal)
        .expect("start frame");
    let half = |y| Rectangle::new(Point::from((0, y)), Size::from((W as i32, (H / 2) as i32)));
    frame
        .clear(Color32F::from(top), &[half(0)])
        .expect("clear top");
    frame
        .clear(Color32F::from(bottom), &[half((H / 2) as i32)])
        .expect("clear bottom");
    let sync = frame.finish().expect("finish frame");
    drop(fb);
    // The conversion samples this buffer next; wait for the paint to actually land first.
    renderer.wait(&sync).expect("wait for paint");
}

/// Map an NV12 dmabuf and read (luma, chroma) at a point, in 0-255.
fn sample(nv12: &Dmabuf, x: u32, y: u32) -> (u8, (u8, u8)) {
    let stride = nv12.strides().next().unwrap() as usize;
    let uv_offset = nv12.offsets().nth(1).unwrap() as usize;
    let fd = nv12.handles().next().unwrap();
    let len = uv_offset + stride * (H as usize / 2);
    let map = unsafe {
        let p = libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ,
            libc::MAP_SHARED,
            fd.as_raw_fd(),
            0,
        );
        assert_ne!(p, libc::MAP_FAILED, "mmap nv12 buffer");
        std::slice::from_raw_parts(p as *const u8, len)
    };
    let luma = map[y as usize * stride + x as usize];
    let uv = uv_offset + (y as usize / 2) * stride + (x as usize / 2) * 2;
    let chroma = (map[uv], map[uv + 1]);
    unsafe { libc::munmap(map.as_ptr() as *mut _, len) };
    (luma, chroma)
}

#[test]
fn converts_argb_to_nv12_without_flipping_or_swapping_chroma() {
    let Some(node) = usable_node() else {
        eprintln!("skipping: no render node here can render R8/GR88");
        return;
    };
    let (gbm, mut renderer) = carjack::nv12::open_gpu(&node).expect("open gpu");
    let mut allocator = GbmAllocator::new(gbm, GbmBufferFlags::RENDERING);
    let mut argb = allocator
        .create_buffer(W, H, Fourcc::Argb8888, &[Modifier::Linear])
        .expect("allocate argb")
        .export()
        .expect("export argb");

    // Top half red, bottom half blue: distinct in both luma and chroma, and asymmetric top to
    // bottom so a vertical flip cannot pass.
    paint(
        &mut renderer,
        &mut argb,
        [1.0, 0.0, 0.0, 1.0],
        [0.0, 0.0, 1.0, 1.0],
    );

    let mut converter =
        carjack::nv12::Nv12Converter::new(&mut renderer, &mut allocator, &argb, W, H, 1)
            .expect("build converter");
    let nv12 = converter.convert(&mut renderer).expect("convert");

    let (top_y, (top_cb, top_cr)) = sample(&nv12, W / 2, H / 4);
    let (bot_y, (bot_cb, bot_cr)) = sample(&nv12, W / 2, H * 3 / 4);

    // BT.601 limited range (H is below 576): red is Y=81 Cb=90 Cr=240, blue is Y=41 Cb=240 Cr=110.
    // Generous tolerances - this is checking we produced the right *thing*, not the last bit.
    let near = |got: u8, want: u8| (got as i32 - want as i32).abs() <= 6;

    assert!(
        near(top_y, 81),
        "top half luma {top_y}, expected ~81 for red - a vertical flip would give ~41 (blue)"
    );
    assert!(
        near(bot_y, 41),
        "bottom half luma {bot_y}, expected ~41 for blue"
    );
    assert!(
        near(top_cb, 90) && near(top_cr, 240),
        "top half chroma ({top_cb}, {top_cr}), expected ~(90, 240) for red - \
         swapped Cb/Cr would give ~(240, 90)"
    );
    assert!(
        near(bot_cb, 240) && near(bot_cr, 110),
        "bottom half chroma ({bot_cb}, {bot_cr}), expected ~(240, 110) for blue"
    );

    converter.destroy(&mut renderer);
}

#[test]
fn grey_comes_out_achromatic() {
    let Some(node) = usable_node() else {
        eprintln!("skipping: no render node here can render R8/GR88");
        return;
    };
    let (gbm, mut renderer) = carjack::nv12::open_gpu(&node).expect("open gpu");
    let mut allocator = GbmAllocator::new(gbm, GbmBufferFlags::RENDERING);
    let mut argb = allocator
        .create_buffer(W, H, Fourcc::Argb8888, &[Modifier::Linear])
        .expect("allocate argb")
        .export()
        .expect("export argb");
    let grey = [0.5, 0.5, 0.5, 1.0];
    paint(&mut renderer, &mut argb, grey, grey);

    let mut converter =
        carjack::nv12::Nv12Converter::new(&mut renderer, &mut allocator, &argb, W, H, 1)
            .expect("build converter");
    let nv12 = converter.convert(&mut renderer).expect("convert");
    let (y, (cb, cr)) = sample(&nv12, W / 2, H / 2);

    // Mid grey: luma near the middle of 16-235, both chroma channels at the 128 midpoint. A
    // wrong matrix or a missing offset shows up as a tint here.
    assert!((y as i32 - 126).abs() <= 8, "grey luma {y}, expected ~126");
    assert!((cb as i32 - 128).abs() <= 3, "grey Cb {cb}, expected ~128");
    assert!((cr as i32 - 128).abs() <= 3, "grey Cr {cr}, expected ~128");

    converter.destroy(&mut renderer);
}
