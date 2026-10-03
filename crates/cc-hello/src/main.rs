//! Puts a small Warp Cyan overlay 1.5 m in front of the room's origin for five seconds:
//! proof that the OpenVR C API bindings link and work.
use openvr_sys as vr;
use std::ffi::CString;

fn table<T>(name: &str) -> *mut T {
    let mut err = 0;
    let c = CString::new(format!("FnTable:{name}")).unwrap();
    unsafe { vr::VR_GetGenericInterface(c.as_ptr(), &mut err) as *mut T }
}

fn main() {
    unsafe {
        let mut err = 0;
        vr::VR_InitInternal2(&mut err, vr::EVRApplicationType_VRApplication_Overlay, std::ptr::null());
        assert_eq!(err, 0, "VR_InitInternal2 failed: {err}");
        let overlay: &vr::VR_IVROverlay_FnTable = &*table(std::str::from_utf8_unchecked(
            std::ffi::CStr::from_ptr(vr::IVROverlay_Version.as_ptr() as *const _).to_bytes()));
        let key = CString::new("controlcenter.hello").unwrap();
        let mut h: vr::VROverlayHandle_t = 0;
        assert_eq!(overlay.CreateOverlay.unwrap()(key.as_ptr() as *mut _, key.as_ptr() as *mut _, &mut h), 0);
        let (w, hgt) = (64u32, 64u32);
        let px: Vec<u8> = (0..w * hgt).flat_map(|_| [125u8, 249, 255, 255]).collect();
        overlay.SetOverlayRaw.unwrap()(h, px.as_ptr() as *mut _, w, hgt, 4);
        overlay.SetOverlayWidthInMeters.unwrap()(h, 0.2);
        let mut m = vr::HmdMatrix34_t::default();
        m.m = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 1.5], [0.0, 0.0, 1.0, -1.5]];
        overlay.SetOverlayTransformAbsolute.unwrap()(h, vr::ETrackingUniverseOrigin_TrackingUniverseStanding, &mut m);
        overlay.ShowOverlay.unwrap()(h);
        println!("overlay {h} shown");
        std::thread::sleep(std::time::Duration::from_secs(5));
        overlay.DestroyOverlay.unwrap()(h);
        vr::VR_ShutdownInternal();
    }
}
