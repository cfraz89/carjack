//! DPU-side ARGB -> NV12 conversion, through the display controller's writeback block.
//!
//! Does the same job as [`crate::nv12`] - turn the composited ARGB frame into the NV12 dmabuf the
//! hardware encoder wants - but on fixed-function display hardware instead of the GPU. The frame
//! goes out over a DRM plane, through the display pipeline's chroma-down module, and lands in
//! memory as NV12. No shader pass, no GPU round trip.
//!
//! Only the conversion moves. Composition is still GLES: this takes the buffer
//! `render_and_publish` has already rendered into and puts *that* on the plane. Handing client
//! buffers to planes directly, so the GPU is out of the path entirely, is the next step and is
//! why [`Self::new`] takes the source buffer rather than assuming it.
//!
//! Verified on a Radxa Dragon Q6A (QCS6490, sc7280). What the hardware gives:
//!
//! - one writeback block, `wb_2`, max line width 4096
//! - NV12 among its formats, and `DRM_FORMAT_MOD_LINEAR` required for the target
//! - RGB->YUV in the CDM, BT.601 upstream and BT.709 above 576 lines with the board's kernel
//!   patch - matching what [`crate::nv12`] picks, so a frame that falls back does not shift
//!
//! Everything here is best-effort: [`Self::new`] validates the whole path with a test commit and
//! returns `Err` if any of it is unavailable, so the caller falls back to the GPU pass.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::Path;

use drm::buffer::PlanarBuffer;
use drm::control::{
    self, atomic, connector, crtc, plane, property, AtomicCommitFlags, Device as ControlDevice,
    FbCmd2Flags,
};
use drm::Device as DrmDevice;
use eyre::{bail, eyre, Result, WrapErr};
use smithay::backend::allocator::dmabuf::{AsDmabuf, Dmabuf};
use smithay::backend::allocator::gbm::GbmAllocator;
use smithay::backend::allocator::{Allocator, Buffer, Fourcc, Modifier};
use smithay::backend::drm::DrmDeviceFd;

/// Rows of chroma per row of luma: NV12 is 4:2:0, so the target is 1.5x the image height. Same
/// scheme as [`crate::nv12`], for the same reason - GBM refuses `NV12` on Adreno, so the buffer is
/// allocated as linear `R8` that tall and described to DRM as two NV12 planes on the one fd.
const NV12_ROWS_NUM: u32 = 3;
const NV12_ROWS_DEN: u32 = 2;

/// Where the line between BT.601 and BT.709 falls, matching `nv12::matrix_for` and the kernel's
/// CDM patch. Here only to be reported, so a mismatch is visible in the log rather than on screen.
const SD_HEIGHT: u32 = 576;

pub struct WritebackConverter {
    card: Card,
    connector: connector::Handle,
    crtc: crtc::Handle,
    plane: plane::Handle,
    mode: property::Value<'static>,
    conn_props: PropMap,
    crtc_props: PropMap,
    plane_props: PropMap,
    /// The compositor's ARGB render target, imported into the card node once.
    source_fb: control::framebuffer::Handle,
    targets: Vec<Target>,
    next: usize,
    /// The first commit has to carry the mode and light the CRTC up; the rest only swap buffers.
    modeset_done: bool,
    width: u32,
    height: u32,
}

impl WritebackConverter {
    /// Build the writeback path for a compositor rendering `width`x`height` into `source`, with
    /// `pool` NV12 targets to cycle through - the encoder holds a published buffer until it is
    /// done with it.
    pub fn new(
        render_node: &str,
        allocator: &mut GbmAllocator<DrmDeviceFd>,
        source: &Dmabuf,
        width: u32,
        height: u32,
        pool: usize,
    ) -> Result<Self> {
        let card_path = card_node_for(render_node)?;
        let card = Card::open(&card_path)?;

        // Atomic first: the writeback connector is hidden from userspace without the cap, and the
        // cap is rejected without atomic.
        card.set_client_capability(drm::ClientCapability::Atomic, true)
            .wrap_err("enable atomic modesetting")?;
        card.set_client_capability(drm::ClientCapability::UniversalPlanes, true)
            .wrap_err("enable universal planes")?;
        card.set_client_capability(drm::ClientCapability::WritebackConnectors, true)
            .wrap_err("enable writeback connectors")?;
        card.acquire_master_lock()
            .wrap_err("become DRM master (is something else holding it?)")?;

        let resources = card.resource_handles().wrap_err("read DRM resources")?;
        let connector = find_writeback(&card, &resources)?;
        let crtc = pick_crtc(&card, &resources, &connector)?;
        let plane = pick_plane(&card, &resources, crtc, source.format().code)?;

        let conn_props = properties(&card, connector.handle())?;
        let crtc_props = properties(&card, crtc)?;
        let plane_props = properties(&card, plane)?;

        // The kernel requires the writeback buffer to match the CRTC mode exactly, so the mode is
        // what fixes the output size.
        let mode = mode_for(&card, &connector, width, height)?;

        let source_fb = import_fb(&card, source)?;

        let mut targets = Vec::with_capacity(pool);
        for _ in 0..pool {
            targets.push(Target::allocate(&card, allocator, width, height)?);
        }

        let this = Self {
            card,
            connector: connector.handle(),
            crtc,
            plane,
            mode,
            conn_props,
            crtc_props,
            plane_props,
            source_fb,
            targets,
            next: 0,
            modeset_done: false,
            width,
            height,
        };

        // Prove the whole path before the caller commits to it. A rejection here is the driver's
        // atomic_check talking and costs nothing; finding out at the first frame would mean a
        // black stream instead of a fallback.
        let req = this.request(this.targets[0].fb, true);
        this.card
            .atomic_commit(
                AtomicCommitFlags::TEST_ONLY | AtomicCommitFlags::ALLOW_MODESET,
                req,
            )
            .wrap_err("the driver rejected the writeback configuration")?;

        tracing::info!(
            "compositor: NV12 conversion on the display controller ({width}x{height}, \
             {} limited range, {pool} targets, {card_path})",
            if height > SD_HEIGHT { "BT.709" } else { "BT.601" },
        );
        Ok(this)
    }

    /// Re-point at a new render target at a new size.
    ///
    /// In place rather than by building a replacement, because DRM master is held per open file:
    /// a second converter could not take it while this one is alive, so the mode, the source
    /// framebuffer and the targets are swapped under the same open device. Everything new is
    /// built before anything old is destroyed, so a failure leaves the converter usable at the
    /// old size.
    pub fn resize(
        &mut self,
        allocator: &mut GbmAllocator<DrmDeviceFd>,
        source: &Dmabuf,
        width: u32,
        height: u32,
    ) -> Result<()> {
        let info = self
            .card
            .get_connector(self.connector, true)
            .wrap_err("re-read the writeback connector")?;
        let mode = mode_for(&self.card, &info, width, height)?;
        let source_fb = import_fb(&self.card, source)?;
        let mut targets = Vec::with_capacity(self.targets.len());
        for _ in 0..self.targets.len() {
            targets.push(Target::allocate(&self.card, allocator, width, height)?);
        }

        for old in std::mem::replace(&mut self.targets, targets) {
            let _ = self.card.destroy_framebuffer(old.fb);
        }
        let _ = self
            .card
            .destroy_framebuffer(std::mem::replace(&mut self.source_fb, source_fb));
        self.mode = mode;
        self.next = 0;
        // The new mode has to be committed before the next capture.
        self.modeset_done = false;
        self.width = width;
        self.height = height;
        Ok(())
    }

    /// Put the source buffer through the display pipeline and return the NV12 target it landed in.
    /// The caller must have finished rendering into the source.
    pub fn convert(&mut self) -> Result<Dmabuf> {
        let idx = self.next;
        self.next = (self.next + 1) % self.targets.len();
        let fb = self.targets[idx].fb;

        if !self.modeset_done {
            // Light the crtc up and throw away what that commit captures. The first capture after
            // a modeset is not trustworthy on this hardware - `examples/writeback-probe.rs` run
            // back to back alternates correct and blank, delay or no delay - so the frame is taken
            // from a second commit once the crtc is live. Costs one extra commit per startup and
            // per resize, and nothing per frame.
            self.capture(fb, true)
                .wrap_err("writeback modeset commit")?;
            self.modeset_done = true;
        }
        self.capture(fb, false).wrap_err("writeback commit")?;

        Ok(self.targets[idx].dmabuf.clone())
    }

    /// One commit, waited to completion.
    fn capture(&self, target_fb: control::framebuffer::Handle, modeset: bool) -> Result<()> {
        let mut req = self.request(target_fb, modeset);

        // The kernel writes the out-fence fd into userspace memory, so the property value is the
        // address of this i32 rather than a handle.
        let mut fence_fd: i32 = -1;
        req.add_property(
            self.connector,
            prop(&self.conn_props, "WRITEBACK_OUT_FENCE_PTR")?,
            property::Value::UnsignedRange((&raw mut fence_fd).addr() as u64),
        );

        let flags = if modeset {
            AtomicCommitFlags::ALLOW_MODESET
        } else {
            AtomicCommitFlags::empty()
        };
        self.card.atomic_commit(flags, req)?;

        if fence_fd < 0 {
            bail!("the commit carried no out-fence");
        }
        let fence = unsafe { <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fence_fd) };
        wait_signalled(fence.as_fd())
    }

    /// The properties every writeback commit sets. `modeset` adds the ones that only matter while
    /// the CRTC is being lit up.
    fn request(&self, target_fb: control::framebuffer::Handle, modeset: bool) -> atomic::AtomicModeReq {
        let mut req = atomic::AtomicModeReq::new();

        if modeset {
            if let (Ok(active), Ok(mode_id)) = (
                prop(&self.crtc_props, "ACTIVE"),
                prop(&self.crtc_props, "MODE_ID"),
            ) {
                req.add_property(self.crtc, active, property::Value::Boolean(true));
                req.add_property(self.crtc, mode_id, self.mode);
            }
            if let Ok(crtc_id) = prop(&self.conn_props, "CRTC_ID") {
                req.add_property(self.connector, crtc_id, property::Value::CRTC(Some(self.crtc)));
            }
            if let Ok(crtc_id) = prop(&self.plane_props, "CRTC_ID") {
                req.add_property(self.plane, crtc_id, property::Value::CRTC(Some(self.crtc)));
            }
            // SRC_* are 16.16 fixed point; CRTC_* are plain pixels. No scaling: the source is
            // rendered at exactly the output size.
            for (name, value) in [
                ("SRC_X", 0),
                ("SRC_Y", 0),
                ("SRC_W", u64::from(self.width) << 16),
                ("SRC_H", u64::from(self.height) << 16),
                ("CRTC_X", 0),
                ("CRTC_Y", 0),
                ("CRTC_W", u64::from(self.width)),
                ("CRTC_H", u64::from(self.height)),
            ] {
                if let Ok(id) = prop(&self.plane_props, name) {
                    req.add_property(self.plane, id, property::Value::UnsignedRange(value));
                }
            }
        }

        if let Ok(fb_id) = prop(&self.plane_props, "FB_ID") {
            req.add_property(
                self.plane,
                fb_id,
                property::Value::Framebuffer(Some(self.source_fb)),
            );
        }
        if let Ok(wb_fb) = prop(&self.conn_props, "WRITEBACK_FB_ID") {
            req.add_property(
                self.connector,
                wb_fb,
                property::Value::Framebuffer(Some(target_fb)),
            );
        }
        req
    }
}

impl Drop for WritebackConverter {
    fn drop(&mut self) {
        // Put the CRTC back as it was found, *before* dropping master. Releasing master does not
        // undo a modeset: a CRTC left ACTIVE stays bound to its plane, and the next process to ask
        // for that plane on another CRTC is refused with EINVAL. One leak per run is enough to
        // make this path look intermittent.
        let mut req = atomic::AtomicModeReq::new();
        if let Ok(id) = prop(&self.plane_props, "FB_ID") {
            req.add_property(self.plane, id, property::Value::Framebuffer(None));
        }
        if let Ok(id) = prop(&self.plane_props, "CRTC_ID") {
            req.add_property(self.plane, id, property::Value::CRTC(None));
        }
        if let Ok(id) = prop(&self.conn_props, "CRTC_ID") {
            req.add_property(self.connector, id, property::Value::CRTC(None));
        }
        if let Ok(id) = prop(&self.crtc_props, "MODE_ID") {
            req.add_property(self.crtc, id, property::Value::Blob(0));
        }
        if let Ok(id) = prop(&self.crtc_props, "ACTIVE") {
            req.add_property(self.crtc, id, property::Value::Boolean(false));
        }
        if let Err(e) = self
            .card
            .atomic_commit(AtomicCommitFlags::ALLOW_MODESET, req)
        {
            tracing::warn!("writeback: could not release the crtc ({e}); a restart may fall back");
        }

        for target in &self.targets {
            let _ = self.card.destroy_framebuffer(target.fb);
        }
        let _ = self.card.destroy_framebuffer(self.source_fb);
        let _ = self.card.release_master_lock();
    }
}

/// One NV12 writeback target: the dmabuf the encoder gets, and the same memory as a DRM
/// framebuffer for the display controller to write into.
struct Target {
    dmabuf: Dmabuf,
    fb: control::framebuffer::Handle,
}

impl Target {
    fn allocate(
        card: &Card,
        allocator: &mut GbmAllocator<DrmDeviceFd>,
        width: u32,
        height: u32,
    ) -> Result<Self> {
        let rows = height * NV12_ROWS_NUM / NV12_ROWS_DEN;
        // Linear is not a preference: the scheme rests on the bytes being exactly where NV12 says
        // they are, and the driver rejects any other modifier on a writeback buffer anyway.
        let buffer = allocator
            .create_buffer(width, rows, Fourcc::R8, &[Modifier::Linear])
            .map_err(|e| eyre!("allocate {width}x{rows} R8 writeback target: {e}"))?;
        let surface = buffer
            .export()
            .map_err(|e| eyre!("export writeback target: {e}"))?;
        if surface.format().modifier != Modifier::Linear {
            bail!(
                "writeback target came back {:?}, not Linear - its bytes would not be NV12",
                surface.format().modifier
            );
        }
        let stride = surface
            .strides()
            .next()
            .ok_or_else(|| eyre!("writeback target has no plane"))?;
        let fd = surface
            .handles()
            .next()
            .ok_or_else(|| eyre!("writeback target has no fd"))?;
        let uv_offset = stride * height;

        let handle = card
            .prime_fd_to_buffer(fd)
            .wrap_err("import the writeback target into the card node")?;
        let fb = card
            .add_planar_framebuffer(
                &Nv12Fb {
                    handle,
                    width,
                    height,
                    stride,
                    uv_offset,
                },
                FbCmd2Flags::MODIFIERS,
            )
            .wrap_err("add the NV12 writeback framebuffer")?;

        // The same memory as the encoder wants it: one fd, luma at 0, chroma at `uv_offset`.
        let mut builder = Dmabuf::builder(
            (width as i32, height as i32),
            Fourcc::Nv12,
            Modifier::Linear,
            smithay::backend::allocator::dmabuf::DmabufFlags::empty(),
        );
        for (idx, offset) in [(0, 0), (1, uv_offset)] {
            let dup = fd
                .try_clone_to_owned()
                .map_err(|e| eyre!("dup writeback target fd: {e}"))?;
            builder.add_plane(dup, idx, offset, stride);
        }
        let dmabuf = builder
            .build()
            .ok_or_else(|| eyre!("build NV12 dmabuf description"))?;

        Ok(Self { dmabuf, fb })
    }
}

/// The linear `R8` allocation described to `addfb2` as two NV12 planes on the one handle.
struct Nv12Fb {
    handle: drm::buffer::Handle,
    width: u32,
    height: u32,
    stride: u32,
    uv_offset: u32,
}

impl PlanarBuffer for Nv12Fb {
    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
    fn format(&self) -> Fourcc {
        Fourcc::Nv12
    }
    fn modifier(&self) -> Option<Modifier> {
        Some(Modifier::Linear)
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

/// A DRM card node. The crate wants the two marker traits on whatever owns the fd.
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
}

impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl DrmDevice for Card {}
impl ControlDevice for Card {}

type PropMap = HashMap<String, property::Handle>;

/// The card node beside a render node, via sysfs: `/sys/class/drm/renderD128/device/drm/` lists
/// every node on the same device.
fn card_node_for(render_node: &str) -> Result<String> {
    let name = Path::new(render_node)
        .file_name()
        .ok_or_else(|| eyre!("{render_node} has no file name"))?
        .to_string_lossy()
        .into_owned();
    let dir = format!("/sys/class/drm/{name}/device/drm");
    let entries = std::fs::read_dir(&dir).wrap_err_with(|| format!("read {dir}"))?;
    for entry in entries.flatten() {
        let node = entry.file_name().to_string_lossy().into_owned();
        if node.starts_with("card") {
            return Ok(format!("/dev/dri/{node}"));
        }
    }
    bail!("no card node beside {render_node} in {dir}")
}

fn find_writeback(card: &Card, resources: &control::ResourceHandles) -> Result<connector::Info> {
    for &handle in resources.connectors() {
        // Unforced: the interface is known without a probe, and probing a real display connector
        // is not free on every driver.
        if card.get_connector(handle, false)?.interface() != connector::Interface::Writeback {
            continue;
        }
        // Forced, for this one connector: its mode list is filled on probe, and an unforced read
        // returns whatever is cached, which on a fresh device is nothing.
        return Ok(card.get_connector(handle, true)?);
    }
    bail!("no writeback connector on this device")
}

fn pick_crtc(
    card: &Card,
    resources: &control::ResourceHandles,
    wb: &connector::Info,
) -> Result<crtc::Handle> {
    let mut candidates = Vec::new();
    for &enc in wb.encoders() {
        candidates.extend(resources.filter_crtcs(card.get_encoder(enc)?.possible_crtcs()));
    }
    // Prefer one nothing is scanning out on, so taking it cannot blank a real display.
    for &handle in &candidates {
        if card.get_crtc(handle)?.mode().is_none() {
            return Ok(handle);
        }
    }
    candidates
        .first()
        .copied()
        .ok_or_else(|| eyre!("the writeback connector's encoders have no usable crtc"))
}

fn pick_plane(
    card: &Card,
    resources: &control::ResourceHandles,
    crtc: crtc::Handle,
    format: Fourcc,
) -> Result<plane::Handle> {
    /// `DRM_PLANE_TYPE_PRIMARY`. The crate exposes plane type only as the raw enum property.
    const PRIMARY: u64 = 1;

    for &handle in card.plane_handles()?.iter() {
        let info = card.get_plane(handle)?;
        if !resources.filter_crtcs(info.possible_crtcs()).contains(&crtc) {
            continue;
        }
        // Skip one already bound elsewhere. It would be refused anyway, and the usual reason for
        // finding one is a previous run that died before it could release its CRTC - in which
        // case the other primary plane is still free and this succeeds instead of inheriting the
        // mess.
        if info.crtc().is_some_and(|c| c != crtc) {
            continue;
        }
        let set = card.get_properties(handle)?;
        let (ids, values) = set.as_props_and_values();
        let is_primary = ids.iter().zip(values).any(|(&id, &v)| {
            card.get_property(id)
                .map(|i| i.name().to_string_lossy() == "type" && v == PRIMARY)
                .unwrap_or(false)
        });
        if is_primary && info.formats().contains(&(format as u32)) {
            return Ok(handle);
        }
    }
    bail!("no primary plane on {crtc:?} takes {format:?}")
}

/// Name -> property handle, for building atomic requests.
fn properties<T: control::ResourceHandle>(card: &Card, handle: T) -> Result<PropMap> {
    let set = card.get_properties(handle)?;
    let (ids, _) = set.as_props_and_values();
    let mut map = HashMap::new();
    for &id in ids {
        let info = card.get_property(id)?;
        map.insert(info.name().to_string_lossy().into_owned(), id);
    }
    Ok(map)
}

fn prop(props: &PropMap, name: &str) -> Result<property::Handle> {
    props
        .get(name)
        .copied()
        .ok_or_else(|| eyre!("no {name} property"))
}

/// The mode fixes the output size, and the driver requires the writeback buffer to match it
/// exactly. Only the size is load-bearing - a writeback connector drives no timings, so when the
/// connector offers nothing at this size one is synthesized.
///
/// That is the normal case here rather than the exception: the connector's list comes from
/// `drm_add_modes_noedid`, which is the standard display resolutions, and the compositor renders
/// at whatever the accessory reported - 1080x600 on the bench unit, which is in no such list.
fn mode_for(
    card: &Card,
    wb: &connector::Info,
    width: u32,
    height: u32,
) -> Result<property::Value<'static>> {
    if let Some(mode) = wb
        .modes()
        .iter()
        .find(|m| m.size() == (width as u16, height as u16))
    {
        return card
            .create_property_blob(mode)
            .wrap_err("blob the connector's own mode");
    }
    card.create_property_blob(&ModeInfo::for_size(width, height)?)
        .wrap_err("blob a synthesized mode")
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
    /// Nothing scans this out, so the blanking intervals are arbitrary - they only have to be
    /// self-consistent and leave the clock in a sane range. Only `hdisplay`/`vdisplay` are
    /// checked, by `dpu_wb_conn_atomic_check` against the framebuffer.
    fn for_size(width: u32, height: u32) -> Result<Self> {
        let (w, h) = (
            u16::try_from(width).map_err(|_| eyre!("{width} is too wide for a mode"))?,
            u16::try_from(height).map_err(|_| eyre!("{height} is too tall for a mode"))?,
        );
        let htotal = w.checked_add(160).ok_or_else(|| eyre!("{width} overflows htotal"))?;
        let vtotal = h.checked_add(40).ok_or_else(|| eyre!("{height} overflows vtotal"))?;
        let mut name = [0u8; 32];
        let label = format!("{width}x{height}");
        let n = label.len().min(name.len() - 1);
        name[..n].copy_from_slice(&label.as_bytes()[..n]);
        Ok(Self {
            // kHz, for 60Hz over the total including blanking.
            clock: (u32::from(htotal) * u32::from(vtotal) * 60) / 1000,
            hdisplay: w,
            hsync_start: w + 48,
            hsync_end: w + 96,
            htotal,
            hskew: 0,
            vdisplay: h,
            vsync_start: h + 8,
            vsync_end: h + 16,
            vtotal,
            vscan: 0,
            vrefresh: 60,
            // DRM_MODE_FLAG_PHSYNC | DRM_MODE_FLAG_PVSYNC
            flags: 0x1 | 0x4,
            // DRM_MODE_TYPE_DRIVER
            kind: 1 << 6,
            name,
        })
    }
}

/// Import a dmabuf the compositor rendered into as a DRM framebuffer on the card node.
fn import_fb(card: &Card, buf: &Dmabuf) -> Result<control::framebuffer::Handle> {
    let fd = buf
        .handles()
        .next()
        .ok_or_else(|| eyre!("source dmabuf has no fd"))?;
    let handle = card
        .prime_fd_to_buffer(fd)
        .wrap_err("import the source buffer into the card node")?;
    let stride = buf
        .strides()
        .next()
        .ok_or_else(|| eyre!("source dmabuf has no stride"))?;
    let offset = buf.offsets().next().unwrap_or(0);

    card.add_planar_framebuffer(
        &SingleFb {
            handle,
            width: buf.width(),
            height: buf.height(),
            format: buf.format().code,
            modifier: buf.format().modifier,
            stride,
            offset,
        },
        FbCmd2Flags::MODIFIERS,
    )
    .wrap_err_with(|| {
        format!(
            "add a framebuffer for the {:?} source buffer (modifier {:?}) - the display \
             controller may not accept what the GPU renders into",
            buf.format().code,
            buf.format().modifier
        )
    })
}

/// A single-plane buffer, for the compositor's own render target.
struct SingleFb {
    handle: drm::buffer::Handle,
    width: u32,
    height: u32,
    format: Fourcc,
    modifier: Modifier,
    stride: u32,
    offset: u32,
}

impl PlanarBuffer for SingleFb {
    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
    fn format(&self) -> Fourcc {
        self.format
    }
    fn modifier(&self) -> Option<Modifier> {
        Some(self.modifier)
    }
    fn pitches(&self) -> [u32; 4] {
        [self.stride, 0, 0, 0]
    }
    fn handles(&self) -> [Option<drm::buffer::Handle>; 4] {
        [Some(self.handle), None, None, None]
    }
    fn offsets(&self) -> [u32; 4] {
        [self.offset, 0, 0, 0]
    }
}

/// The out-fence becomes readable once the writeback has landed in memory.
fn wait_signalled(fd: BorrowedFd<'_>) -> Result<()> {
    use std::os::fd::AsRawFd;
    let mut pfd = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // One frame at 30fps is 33ms; a second means the frame is never coming.
    match unsafe { libc::poll(&mut pfd, 1, 1000) } {
        1 => Ok(()),
        0 => bail!("the writeback never signalled - the display controller did not finish"),
        _ => Err(eyre!(
            "poll the writeback fence: {}",
            std::io::Error::last_os_error()
        )),
    }
}
