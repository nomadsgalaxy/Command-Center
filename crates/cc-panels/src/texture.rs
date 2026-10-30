//! Pictures on surfaces: the `Textures` boundary (docs/openxr-plan.md, phase 0 step 5) and `Live`,
//! its OpenVR side, the only code here that touches OpenVR. gpu.rs, capture.rs, back.rs, grab.rs,
//! laser.rs and main.rs show their pictures through it. Built into cc-win-spike too (capture.rs and
//! back.rs use it), so it uses only vr.rs and ids.rs.
use std::os::fd::{AsRawFd, BorrowedFd};

use crate::ids::SurfaceId;
use crate::{call, vr};
use openvr_sys as sys;

/// A dmabuf to show: one plane, as gpu.rs and capture.rs import them now.
pub struct Dmabuf<'a> {
    pub fd: BorrowedFd<'a>,
    pub width: u32,
    pub height: u32,
    pub fourcc: u32,
    pub modifier: u64,
    pub offset: u32,
    pub stride: u32,
}

/// An imported picture: the IPC resource handle on OpenVR.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TexHandle(pub u64);

/// Pictures (gpu.rs, capture.rs). The GBM buffers and their stale tracking stay in the core.
///
/// Who owns what, as OpenVR works today (docs/openxr-plan.md, 2.3, has the details):
///
/// - **`import` doesn't take the fd.** The runtime takes its own reference, so the caller can close
///   its fd straight after (gpu.rs does; capture.rs drops its dup with the frame). The picture
///   stays alive until `release`.
/// - **The core may write a buffer again once a tick has passed since `present` replaced it** on
///   screen. Nothing from the runtime says it's done reading: the core assumes it has finished by
///   its next frame. gpu.rs double-buffers and holds the replaced one for a tick (`held`); capture.rs
///   holds the shown buffer and the one before it back from PipeWire.
/// - **The producer says when it's ready, before `present`.** For a PipeWire dmabuf the core polls
///   the fd for its implicit fence and doesn't present until it's readable. For our own GBM
///   buffers the copy is done on the CPU, mapped and unmapped, so it's ready when the copy returns.
/// - **`release` returns at once, with no signal when the runtime has let go.** The core frees the
///   GBM buffer right after it. Only a UI overlay's resize keeps the old pair a tick longer
///   (gpu.rs `raw_tick`); a panel's resize frees both at once, the shown one included.
/// - **One handle can be on two surfaces:** a panel's back shows the panel's handle (back.rs).
pub trait Textures {
    /// None if the runtime wouldn't import it (ImportDmabuf says only that).
    fn import(&mut self, buf: &Dmabuf) -> Option<TexHandle>;
    fn present(&mut self, s: SurfaceId, t: TexHandle) -> Result<(), Refused>;
    fn release(&mut self, t: TexHandle);
    /// CPU pixels, RGBA (SetOverlayRaw today).
    fn present_pixels(&mut self, s: SurfaceId, px: &[u8], w: usize, h: usize) -> Result<(), Refused>;
    fn clear(&mut self, s: SurfaceId);
}

/// The runtime wouldn't take a picture: OpenVR's overlay error. Its name is only looked up when
/// it's printed, so a caller that doesn't print it makes no more calls than it did before.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Refused(pub u32);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&vr::error_name(self.0))
    }
}

/// SteamVR, for real.
pub struct Live;

impl Textures for Live {
    fn import(&mut self, buf: &Dmabuf) -> Option<TexHandle> {
        let mut a = sys::DmabufAttributes_t {
            unWidth: buf.width,
            unHeight: buf.height,
            unDepth: 1,
            unMipLevels: 1,
            unArrayLayers: 1,
            unSampleCount: 1,
            unFormat: buf.fourcc,
            ulModifier: buf.modifier,
            unPlaneCount: 1,
            ..Default::default()
        };
        a.plane[0] = sys::DmabufPlane_t { unOffset: buf.offset, unStride: buf.stride, nFd: buf.fd.as_raw_fd() };
        let mut handle = 0;
        call!(ipc, ImportDmabuf, sys::EVRApplicationType_VRApplication_Overlay, &mut a, &mut handle).then_some(TexHandle(handle))
    }

    fn present(&mut self, s: SurfaceId, t: TexHandle) -> Result<(), Refused> {
        let mut h = t.0;
        let mut t = sys::Texture_t { handle: &mut h as *mut _ as *mut std::ffi::c_void, eType: sys::ETextureType_TextureType_SharedTextureHandle, eColorSpace: sys::EColorSpace_ColorSpace_Gamma };
        match call!(ov, SetOverlayTexture, s.0, &mut t) {
            0 => Ok(()),
            e => Err(Refused(e)),
        }
    }

    fn release(&mut self, t: TexHandle) {
        call!(ipc, UnrefResource, t.0);
    }

    fn present_pixels(&mut self, s: SurfaceId, px: &[u8], w: usize, h: usize) -> Result<(), Refused> {
        // SetOverlayRaw only reads the pixels, though it takes them mutable
        match call!(ov, SetOverlayRaw, s.0, px.as_ptr() as *mut _, w as u32, h as u32, 4) {
            0 => Ok(()),
            e => Err(Refused(e)),
        }
    }

    fn clear(&mut self, s: SurfaceId) {
        call!(ov, ClearOverlayTexture, s.0);
    }
}
