//! Raw bindings to libvncclient (see `wrapper.h` for what's included).
#![allow(non_upper_case_globals, non_camel_case_types, non_snake_case, dead_code, improper_ctypes, clippy::all, unnecessary_transmutes)]
include!(concat!(env!("OUT_DIR"), "/vncclient.rs"));

unsafe extern "C" {
    /// OpenSSL's, for the bytes a TLS record already holds (docs/vnc.md, review C1).
    pub fn SSL_pending(ssl: *const std::ffi::c_void) -> std::os::raw::c_int;
}
