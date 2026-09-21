//! Does the DPU's writeback block composite straight into an NV12 buffer on this board?
//!
//! If it does, the compositor can hand the encoder a frame the display hardware converted, and the
//! GLES composition plus [`carjack::nv12`]'s ARGB->NV12 shader pass both leave the hot path.
//!
//! What the kernel says (`qcom-next` 7.2, read rather than run):
//!
//! - sc7280 has one writeback block, `wb_2`, `maxlinewidth` 4096 - `catalog/dpu_7_2_sc7280.h`
//! - it advertises `DRM_FORMAT_NV12`, via `wb2_formats_rgb_yuv`
//! - the RGB->YUV conversion is the **CDM** (chroma-down module), not the WB block: sc7280 declares
//!   `.cdm = &dpu_cdm_5_x` and `dpu_encoder_phys_wb.c` calls `setup_cdm(.., CDM_CDWN_OUTPUT_WB)`.
//!   `dpu_hw_wb.c` defines `WB_CSC_BASE` and never writes it, which looks like a missing CSC and is
//!   not one - those registers are vestigial.
//!
//! None of that has been run. This probe runs it: one plane of colour bars in, one NV12 writeback
//! frame out, and a report of what the conversion actually did.
//!
//! **Board only.** The dev host has no DPU. It also needs DRM master, so stop `carjack.service`
//! first, and root needs a tty on the image: `ssh -tt carjack@board 'su -c "..."'`.
//!
//! ```text
//! writeback-probe [card-path] [out-path]
//! ```

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};

use drm::buffer::PlanarBuffer;
use drm::control::{
    self, atomic, connector, crtc, plane, property, AtomicCommitFlags, Device as ControlDevice,
    FbCmd2Flags,
};
use drm::Device as DrmDevice;
use drm_fourcc::{DrmFourcc, DrmModifier};
use eyre::{bail, eyre, Result, WrapErr};
use gbm::{BufferObjectFlags, Device as GbmDevice};

/// The encode resolution the image is configured for (`config.toml`, `[display]`). The kernel
/// requires the writeback fb to match the crtc mode exactly (`dpu_wb_conn_atomic_check`), so this
/// picks the mode too.
const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;

/// NV12 is 4:2:0, so the allocation is 1.5x the image height. Same trick as `src/nv12.rs`: GBM
/// refuses `NV12` on Adreno for every usage flag, so allocate linear `R8` that tall and describe
/// the one allocation as two NV12 planes.
const NV12_ROWS_NUM: u32 = 3;
const NV12_ROWS_DEN: u32 = 2;

/// Eight bars across, the usual order. Chosen so every channel is exercised at both extremes and
/// the chroma signs differ bar to bar - a CSC that is not running reads as luma tracking one
/// channel and chroma stuck at 128.
const BARS: [(&str, u8, u8, u8); 8] = [
    ("white", 255, 255, 255),
    ("yellow", 255, 255, 0),
    ("cyan", 0, 255, 255),
    ("green", 0, 255, 0),
    ("magenta", 255, 0, 255),
    ("red", 255, 0, 0),
    ("blue", 0, 0, 255),
    ("black", 0, 0, 0),
];

fn main() -> Result<()> {
    color_eyre::install()?;
    let mut args = std::env::args().skip(1);
    let card_path = args.next().unwrap_or_else(|| "/dev/dri/card0".into());
    let out_path = args.next().unwrap_or_else(|| "/tmp/writeback.nv12".into());

    let card = Card::open(&card_path)?;

    // Atomic first: WritebackConnectors is documented as requiring it, and without the cap the
    // writeback connector is hidden from userspace entirely - which is why `modetest -c` does not
    // list it and its absence there means nothing.
    card.set_client_capability(drm::ClientCapability::Atomic, true)
        .wrap_err("enable atomic modesetting")?;
    card.set_client_capability(drm::ClientCapability::UniversalPlanes, true)
        .wrap_err("enable universal planes")?;
    card.set_client_capability(drm::ClientCapability::WritebackConnectors, true)
        .wrap_err("enable writeback connectors - no writeback support in this kernel?")?;

    // Everything up to the commit is read-only, so the report and the resource checks below run
    // without DRM master - which means they also run against a board with carjack live on it.
    let resources = card.resource_handles().wrap_err("read DRM resources")?;
    report_resources(&card, &resources)?;

    let wb = find_writeback_connector(&card, &resources)?;
    let crtc = pick_crtc(&card, &resources, &wb)?;
    let plane = pick_plane(&card, &resources, crtc)?;
    println!("using crtc {crtc:?}, plane {plane:?}");

    // Only the commit needs master, so take it as late as possible.
    card.acquire_master_lock().wrap_err(
        "become DRM master - is carjack still running? it holds master. \
         `systemctl --user -M carjack@ stop carjack.service`",
    )?;

    // One gbm device on the same fd, so the bo handles are already this device's.
    let gbm = GbmDevice::new(card.try_clone_fd()?).wrap_err("open gbm device")?;

    let mut source = gbm
        .create_buffer_object::<()>(
            WIDTH,
            HEIGHT,
            gbm::Format::Xrgb8888,
            BufferObjectFlags::SCANOUT | BufferObjectFlags::LINEAR | BufferObjectFlags::WRITE,
        )
        .wrap_err("allocate the XRGB8888 source buffer")?;
    paint_bars(&mut source)?;
    let source_fb = card
        .add_planar_framebuffer(&source, FbCmd2Flags::MODIFIERS)
        .wrap_err("add the source framebuffer")?;

    // `target` owns the allocation, so it has to outlive both the framebuffer and the readback.
    let target = Nv12Target::allocate(&card, &gbm)?;
    let target_fb = card
        .add_planar_framebuffer(&target, FbCmd2Flags::MODIFIERS)
        .wrap_err("add the NV12 writeback framebuffer")?;

    let (mode, origin) = mode_blob(&card, &wb)?;
    println!("mode: {WIDTH}x{HEIGHT} ({origin})");

    let result = commit_and_wait(&card, &wb, crtc, plane, mode, source_fb, target_fb);

    // Tear down whatever happened, so a failed probe still leaves the card usable.
    let _ = card.destroy_framebuffer(target_fb);
    let _ = card.destroy_framebuffer(source_fb);
    let _ = card.release_master_lock();
    result?;

    let bytes = target.read_back()?;
    File::create(&out_path)
        .and_then(|mut f| f.write_all(&bytes))
        .wrap_err_with(|| format!("write {out_path}"))?;
    println!(
        "\nwrote {} bytes of NV12 to {out_path}\n  view: ffplay -f rawvideo -pix_fmt nv12 -video_size {WIDTH}x{HEIGHT} {out_path}",
        bytes.len()
    );

    verify(&target, &bytes)
}

/// A DRM device. The crate wants the two marker traits on whatever owns the fd.
struct Card(OwnedFd);

impl Card {
    fn open(path: &str) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .wrap_err_with(|| format!("open {path}"))?;
        Ok(Self(file.into()))
    }

    fn try_clone_fd(&self) -> Result<OwnedFd> {
        self.0.try_clone().wrap_err("dup the DRM fd for gbm")
    }
}

impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl DrmDevice for Card {}
impl ControlDevice for Card {}

/// The writeback target: one linear `R8` allocation, described to `addfb2` as two NV12 planes on
/// the same handle. `dpu_wb_conn_atomic_check` rejects any modifier but linear, which is what this
/// scheme needs anyway - the bytes have to be exactly where NV12 says they are.
struct Nv12Target {
    handle: drm::buffer::Handle,
    /// Kept so the allocation outlives the framebuffer, and for the readback.
    bo: gbm::BufferObject<()>,
    stride: u32,
    uv_offset: u32,
}

impl Nv12Target {
    fn allocate(card: &Card, gbm: &GbmDevice<OwnedFd>) -> Result<Self> {
        let rows = HEIGHT * NV12_ROWS_NUM / NV12_ROWS_DEN;
        let bo = gbm
            .create_buffer_object_with_modifiers2::<()>(
                WIDTH,
                rows,
                gbm::Format::R8,
                [gbm::Modifier::Linear].into_iter(),
                BufferObjectFlags::LINEAR,
            )
            .wrap_err_with(|| format!("allocate the {WIDTH}x{rows} R8 writeback target"))?;
        if bo.modifier() != gbm::Modifier::Linear {
            bail!(
                "writeback target came back {:?}, not Linear - its bytes would not be NV12",
                bo.modifier()
            );
        }

        // Through prime rather than `bo.handle()`: the export is the documented way to a
        // `buffer::Handle`, and `src/nv12.rs` already exports this same allocation.
        let fd = bo.fd().map_err(|e| eyre!("export the writeback target: {e}"))?;
        let handle = card
            .prime_fd_to_buffer(fd.as_fd())
            .wrap_err("import the writeback target as a GEM handle")?;

        let stride = bo.stride();
        Ok(Self {
            handle,
            uv_offset: stride * HEIGHT,
            stride,
            bo,
        })
    }

    /// The whole allocation, luma rows then chroma rows.
    fn read_back(&self) -> Result<Vec<u8>> {
        let rows = HEIGHT * NV12_ROWS_NUM / NV12_ROWS_DEN;
        // `map` is read-only and takes the region in pixels, which for R8 is bytes.
        self.bo
            .map(0, 0, WIDTH, rows, |view| view.buffer().to_vec())
            .map_err(|e| eyre!("map the writeback target for readback: {e}"))
    }

    fn luma(&self, bytes: &[u8], x: u32, y: u32) -> u8 {
        bytes[(y * self.stride + x) as usize]
    }

    /// NV12 chroma is one interleaved Cb,Cr pair per 2x2 luma block.
    fn chroma(&self, bytes: &[u8], x: u32, y: u32) -> (u8, u8) {
        let base = self.uv_offset + (y / 2) * self.stride + (x / 2) * 2;
        (bytes[base as usize], bytes[base as usize + 1])
    }
}

impl PlanarBuffer for Nv12Target {
    fn size(&self) -> (u32, u32) {
        (WIDTH, HEIGHT)
    }
    fn format(&self) -> DrmFourcc {
        DrmFourcc::Nv12
    }
    fn modifier(&self) -> Option<DrmModifier> {
        Some(DrmModifier::Linear)
    }
    fn pitches(&self) -> [u32; 4] {
        // Chroma shares the luma stride: width/2 pairs of two bytes is width bytes a row.
        [self.stride, self.stride, 0, 0]
    }
    fn handles(&self) -> [Option<drm::buffer::Handle>; 4] {
        [Some(self.handle), Some(self.handle), None, None]
    }
    fn offsets(&self) -> [u32; 4] {
        [0, self.uv_offset, 0, 0]
    }
}

fn report_resources(card: &Card, resources: &control::ResourceHandles) -> Result<()> {
    println!("connectors:");
    for &handle in resources.connectors() {
        let info = card.get_connector(handle, false)?;
        println!(
            "  {handle:?} {:<9} {:?}, {} modes",
            info.interface().as_str(),
            info.state(),
            info.modes().len()
        );
    }

    println!("crtcs:");
    for &handle in resources.crtcs() {
        let info = card.get_crtc(handle)?;
        let held = info.mode().map_or("free", |_| "in use");
        println!("  {handle:?} {held}");
    }

    // The plane budget is the ceiling on hardware composition: sc7280 has 4 SSPPs, one of which
    // (VIG) is the only one with input scaling and CSC.
    let planes = card.plane_handles()?;
    println!("planes: {}", planes.len());
    for &handle in planes.iter() {
        let info = card.get_plane(handle)?;
        let props = properties(card, handle)?;
        let kind = props
            .get("type")
            .map_or("?".to_string(), |&(_, v)| format!("{v}"));
        println!(
            "  {handle:?} type={kind} {} formats, crtcs={:?}",
            info.formats().len(),
            resources.filter_crtcs(info.possible_crtcs())
        );
    }
    Ok(())
}

fn find_writeback_connector(
    card: &Card,
    resources: &control::ResourceHandles,
) -> Result<connector::Info> {
    for &handle in resources.connectors() {
        // Unforced, like the report: the interface is known without probing, and forcing a probe on
        // this board's DP connector is exactly what the AUX and link-training faults live in.
        if card.get_connector(handle, false)?.interface() != connector::Interface::Writeback {
            continue;
        }
        // Forced, for this one connector only. msm fills its mode list from
        // `dpu_wb_conn_get_modes` -> `drm_add_modes_noedid`, but only when something probes it; an
        // unforced read returns whatever is cached, which on a fresh card is nothing.
        let info = card.get_connector(handle, true)?;
        println!(
            "\nwriteback connector: {handle:?}, {} modes after a forced probe",
            info.modes().len()
        );
        return Ok(info);
    }
    bail!(
        "no writeback connector. The cap is set, so either this kernel's DPU catalog has no WB_2 \
         or the driver did not register it"
    )
}

/// The mode sets the output size, and `dpu_wb_conn_atomic_check` requires the fb to match it
/// exactly. Prefer one the connector offers; synthesize if it offers none, which is allowed - a
/// writeback connector drives no timings, so nothing but the size is load-bearing.
fn mode_blob(card: &Card, wb: &connector::Info) -> Result<(property::Value<'static>, &'static str)> {
    if let Some(mode) = wb
        .modes()
        .iter()
        .find(|m| m.size() == (WIDTH as u16, HEIGHT as u16))
    {
        let blob = card
            .create_property_blob(mode)
            .wrap_err("blob the connector's own mode")?;
        return Ok((blob, "from the connector"));
    }
    let blob = card
        .create_property_blob(&ModeInfo::cea_720p())
        .wrap_err("blob a synthesized mode")?;
    Ok((blob, "synthesized"))
}

/// `drm_mode_modeinfo`, mirrored so a mode can be built without depending on `drm-sys` directly.
/// Stable kernel uAPI.
#[repr(C)]
struct ModeInfo {
    clock: u32,
    hdisplay: u16,
    hsync_start: u16,
    hsync_end: u16,
    htotal: u16,
    hskew: u16,
    vdisplay: u16,
    vsync_start: u16,
    vsync_end: u16,
    vtotal: u16,
    vscan: u16,
    vrefresh: u32,
    flags: u32,
    kind: u32,
    name: [u8; 32],
}

impl ModeInfo {
    /// CEA-861 1280x720p60.
    fn cea_720p() -> Self {
        let mut name = [0u8; 32];
        name[.."1280x720".len()].copy_from_slice(b"1280x720");
        Self {
            clock: 74250,
            hdisplay: 1280,
            hsync_start: 1390,
            hsync_end: 1430,
            htotal: 1650,
            hskew: 0,
            vdisplay: 720,
            vsync_start: 725,
            vsync_end: 730,
            vtotal: 750,
            vscan: 0,
            vrefresh: 60,
            // DRM_MODE_FLAG_PHSYNC | DRM_MODE_FLAG_PVSYNC
            flags: 0x1 | 0x4,
            // DRM_MODE_TYPE_DRIVER
            kind: 1 << 6,
            name,
        }
    }
}

fn pick_crtc(
    card: &Card,
    resources: &control::ResourceHandles,
    wb: &connector::Info,
) -> Result<crtc::Handle> {
    let mut candidates = Vec::new();
    for &enc in wb.encoders() {
        let info = card.get_encoder(enc)?;
        candidates.extend(resources.filter_crtcs(info.possible_crtcs()));
    }
    // Prefer one nothing is scanning out on: fbcon may hold a crtc for the DP console, and taking
    // it would blank the one way in when ssh is not.
    for &handle in &candidates {
        if card.get_crtc(handle)?.mode().is_none() {
            return Ok(handle);
        }
    }
    candidates.first().copied().ok_or_else(|| {
        eyre!("writeback connector's encoders have no possible crtcs - nothing to composite on")
    })
}

fn pick_plane(
    card: &Card,
    resources: &control::ResourceHandles,
    crtc: crtc::Handle,
) -> Result<plane::Handle> {
    /// `DRM_PLANE_TYPE_PRIMARY`. The crate exposes plane type only as the raw enum property.
    const PRIMARY: u64 = 1;

    for &handle in card.plane_handles()?.iter() {
        let info = card.get_plane(handle)?;
        let usable = resources
            .filter_crtcs(info.possible_crtcs())
            .contains(&crtc);
        let primary = properties(card, handle)?
            .get("type")
            .is_some_and(|&(_, v)| v == PRIMARY);
        if usable && primary && info.formats().contains(&(DrmFourcc::Xrgb8888 as u32)) {
            return Ok(handle);
        }
    }
    bail!("no primary plane on {crtc:?} takes XRGB8888")
}

/// Name -> (property, current value), for the atomic request.
fn properties<T: control::ResourceHandle>(
    card: &Card,
    handle: T,
) -> Result<HashMap<String, (property::Handle, property::RawValue)>> {
    let set = card.get_properties(handle)?;
    let (ids, values) = set.as_props_and_values();
    let mut map = HashMap::new();
    for (&id, &value) in ids.iter().zip(values) {
        let info = card.get_property(id)?;
        map.insert(info.name().to_string_lossy().into_owned(), (id, value));
    }
    Ok(map)
}

fn prop(
    props: &HashMap<String, (property::Handle, property::RawValue)>,
    name: &str,
) -> Result<property::Handle> {
    props
        .get(name)
        .map(|&(id, _)| id)
        .ok_or_else(|| eyre!("no {name} property"))
}

fn commit_and_wait(
    card: &Card,
    wb: &connector::Info,
    crtc: crtc::Handle,
    plane: plane::Handle,
    mode: property::Value<'static>,
    source_fb: control::framebuffer::Handle,
    target_fb: control::framebuffer::Handle,
) -> Result<()> {
    let conn_props = properties(card, wb.handle())?;
    let crtc_props = properties(card, crtc)?;
    let plane_props = properties(card, plane)?;

    // The kernel writes the out-fence fd into userspace memory, so the property value is the
    // *address* of this i32, not a handle.
    let mut fence_fd: i32 = -1;

    let mut req = atomic::AtomicModeReq::new();
    req.add_property(crtc, prop(&crtc_props, "ACTIVE")?, property::Value::Boolean(true));
    req.add_property(crtc, prop(&crtc_props, "MODE_ID")?, mode);
    req.add_property(
        wb.handle(),
        prop(&conn_props, "CRTC_ID")?,
        property::Value::CRTC(Some(crtc)),
    );
    req.add_property(
        wb.handle(),
        prop(&conn_props, "WRITEBACK_FB_ID")?,
        property::Value::Framebuffer(Some(target_fb)),
    );
    req.add_property(
        wb.handle(),
        prop(&conn_props, "WRITEBACK_OUT_FENCE_PTR")?,
        property::Value::UnsignedRange((&raw mut fence_fd).addr() as u64),
    );

    req.add_property(
        plane,
        prop(&plane_props, "FB_ID")?,
        property::Value::Framebuffer(Some(source_fb)),
    );
    req.add_property(
        plane,
        prop(&plane_props, "CRTC_ID")?,
        property::Value::CRTC(Some(crtc)),
    );
    // SRC_* are 16.16 fixed point; CRTC_* are plain pixels.
    for (name, value) in [
        ("SRC_X", 0),
        ("SRC_Y", 0),
        ("SRC_W", u64::from(WIDTH) << 16),
        ("SRC_H", u64::from(HEIGHT) << 16),
        ("CRTC_X", 0),
        ("CRTC_Y", 0),
        ("CRTC_W", u64::from(WIDTH)),
        ("CRTC_H", u64::from(HEIGHT)),
    ] {
        req.add_property(
            plane,
            prop(&plane_props, name)?,
            property::Value::UnsignedRange(value),
        );
    }

    // Test first: a rejection here is the atomic_check talking, and its reason is in dmesg with
    // nothing half-applied.
    card.atomic_commit(
        AtomicCommitFlags::TEST_ONLY | AtomicCommitFlags::ALLOW_MODESET,
        req.clone(),
    )
    .wrap_err("atomic TEST_ONLY rejected - check dmesg for dpu_wb_conn_atomic_check")?;

    card.atomic_commit(AtomicCommitFlags::ALLOW_MODESET, req)
        .wrap_err("atomic commit")?;

    if fence_fd < 0 {
        bail!("commit succeeded but the kernel wrote no out-fence fd");
    }
    // Owned so it closes on the way out.
    let fence = unsafe { <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fence_fd) };
    wait_readable(fence.as_fd())?;
    println!("writeback fence signalled");
    Ok(())
}

/// The out-fence becomes readable when the writeback has landed in memory.
fn wait_readable(fd: BorrowedFd<'_>) -> Result<()> {
    use std::os::fd::AsRawFd;
    let mut pfd = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // Generous: one frame at 30fps is 33ms, so a second means it is never coming.
    let rc = unsafe { libc::poll(&mut pfd, 1, 1000) };
    match rc {
        1 => Ok(()),
        0 => bail!("writeback fence never signalled - the DPU did not finish the frame"),
        _ => Err(eyre!("poll the writeback fence: {}", std::io::Error::last_os_error())),
    }
}

fn paint_bars(bo: &mut gbm::BufferObject<()>) -> Result<()> {
    let bar = WIDTH / BARS.len() as u32;
    bo.map_mut(0, 0, WIDTH, HEIGHT, |view| {
        let stride = view.stride();
        let buf = view.buffer_mut();
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let (_, r, g, b) = BARS[(x / bar).min(BARS.len() as u32 - 1) as usize];
                // XRGB8888 is little-endian BGRX in memory.
                let px = (y * stride + x * 4) as usize;
                buf[px] = b;
                buf[px + 1] = g;
                buf[px + 2] = r;
                buf[px + 3] = 0xff;
            }
        }
    })
    .map_err(|e| eyre!("map the source buffer to paint it: {e}"))
}

/// Limited-range RGB->Y'CbCr, the same coefficients `src/nv12.rs` uses.
fn expected(m: ([f32; 3], [f32; 3], [f32; 3]), rgb: (u8, u8, u8)) -> (u8, u8, u8) {
    let (r, g, b) = (f32::from(rgb.0), f32::from(rgb.1), f32::from(rgb.2));
    let dot = |c: [f32; 3]| c[0] * r + c[1] * g + c[2] * b;
    let clamp = |v: f32| v.round().clamp(0.0, 255.0) as u8;
    (
        clamp(dot(m.0) + 16.0),
        clamp(dot(m.1) + 128.0),
        clamp(dot(m.2) + 128.0),
    )
}

/// The real question is not just "did it convert" but "with which matrix" - the GL path picks
/// BT.709 above 576 rows, and Phase 1 has to agree with whatever the CDM does or colours shift.
fn verify(target: &Nv12Target, bytes: &[u8]) -> Result<()> {
    const BT601: ([f32; 3], [f32; 3], [f32; 3]) = (
        [0.257, 0.504, 0.098],
        [-0.148, -0.291, 0.439],
        [0.439, -0.368, -0.071],
    );
    const BT709: ([f32; 3], [f32; 3], [f32; 3]) = (
        [0.183, 0.614, 0.062],
        [-0.101, -0.339, 0.439],
        [0.439, -0.399, -0.040],
    );

    let bar = WIDTH / BARS.len() as u32;
    let y_row = HEIGHT / 2;
    let (mut err601, mut err709) = (0u32, 0u32);

    println!("\n{:<8} {:>14} {:>14} {:>14}", "bar", "got Y/Cb/Cr", "BT.601", "BT.709");
    for (i, &(name, r, g, b)) in BARS.iter().enumerate() {
        let x = i as u32 * bar + bar / 2;
        let (cb, cr) = target.chroma(bytes, x, y_row);
        let got = (target.luma(bytes, x, y_row), cb, cr);
        let e601 = expected(BT601, (r, g, b));
        let e709 = expected(BT709, (r, g, b));
        let diff = |e: (u8, u8, u8)| {
            u32::from(got.0.abs_diff(e.0))
                + u32::from(got.1.abs_diff(e.1))
                + u32::from(got.2.abs_diff(e.2))
        };
        err601 += diff(e601);
        err709 += diff(e709);
        println!(
            "{name:<8} {:>14} {:>14} {:>14}",
            format!("{}/{}/{}", got.0, got.1, got.2),
            format!("{}/{}/{}", e601.0, e601.1, e601.2),
            format!("{}/{}/{}", e709.0, e709.1, e709.2),
        );
    }

    // Per bar, per channel, averaged. A CSC that never ran leaves chroma flat at 128 and lands
    // nowhere near either matrix.
    let per_channel = |total: u32| total as f32 / (BARS.len() * 3) as f32;
    let (m601, m709) = (per_channel(err601), per_channel(err709));
    println!("\nmean error per channel: BT.601 {m601:.1}, BT.709 {m709:.1}");

    let best = m601.min(m709);
    if best > 16.0 {
        bail!(
            "the writeback frame matches neither matrix (best {best:.1}/channel). Either the CDM \
             did not run or the layout assumption is wrong - look at {WIDTH}x{HEIGHT} NV12 by eye \
             before concluding the hardware cannot do this"
        );
    }
    println!(
        "PASS - writeback converted to NV12 in hardware, matching {} \
         (the GL path uses BT.709 above 576 rows; Phase 1 must agree with this)",
        if m709 <= m601 { "BT.709" } else { "BT.601" }
    );
    Ok(())
}
