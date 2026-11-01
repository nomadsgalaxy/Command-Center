//! Raw bindings to libvncclient (see `wrapper.h` for what's included).
#![allow(non_upper_case_globals, non_camel_case_types, non_snake_case, dead_code, improper_ctypes, clippy::all, unnecessary_transmutes)]
include!(concat!(env!("OUT_DIR"), "/vncclient.rs"));

unsafe extern "C" {
    /// OpenSSL's, for the bytes a TLS record already holds (docs/vnc.md, review C1).
    pub fn SSL_pending(ssl: *const std::ffi::c_void) -> std::os::raw::c_int;
    /// OpenSSL's, for the server's own certificate and its SHA-256, which vnc.rs checks against the
    /// pin itself (libvncclient takes a CA-valid certificate without looking at the pin).
    pub fn SSL_get1_peer_certificate(ssl: *const std::ffi::c_void) -> *mut std::ffi::c_void;
    pub fn X509_free(x: *mut std::ffi::c_void);
    pub fn X509_digest(x: *const std::ffi::c_void, md: *const std::ffi::c_void, out: *mut u8, len: *mut std::os::raw::c_uint) -> std::os::raw::c_int;
    pub fn EVP_sha256() -> *const std::ffi::c_void;
}
