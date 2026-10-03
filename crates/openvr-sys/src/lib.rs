//! Raw bindings to OpenVR's C API (`openvr_capi.h`): `VR_InitInternal2`, `VR_GetGenericInterface`
//! with `FnTable:` interface names, and the interface function tables.
#![allow(non_upper_case_globals, non_camel_case_types, non_snake_case, dead_code, clippy::all)]
include!(concat!(env!("OUT_DIR"), "/openvr.rs"));

// Entry points libopenvr_api exports (openvr_capi.h has them under `#if 0`).
unsafe extern "C" {
    pub fn VR_InitInternal2(peError: *mut EVRInitError, eType: EVRApplicationType, pStartupInfo: *const std::os::raw::c_char) -> isize;
    pub fn VR_GetGenericInterface(pchInterfaceVersion: *const std::os::raw::c_char, peError: *mut EVRInitError) -> *mut std::ffi::c_void;
    pub fn VR_GetInitToken() -> u32;
    pub fn VR_ShutdownInternal();
    pub fn VR_GetVRInitErrorAsEnglishDescription(error: EVRInitError) -> *const std::os::raw::c_char;
}
