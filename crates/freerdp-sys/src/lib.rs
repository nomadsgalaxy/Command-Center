//! Raw bindings to FreeRDP 3 and WinPR (see `wrapper.h` for what's included).
#![allow(non_upper_case_globals, non_camel_case_types, non_snake_case, dead_code, improper_ctypes, clippy::all, unnecessary_transmutes)]
include!(concat!(env!("OUT_DIR"), "/freerdp.rs"));

/// FREERDP_PIXEL_FORMAT(bpp, type, a, r, g, b): a function-like macro bindgen can't see.
pub const fn pixel_format(bpp: u32, kind: u32, a: u32, r: u32, g: u32, b: u32) -> u32 {
    (bpp << 24) | (kind << 16) | (a << 12) | (r << 8) | (g << 4) | b
}
/// PIXEL_FORMAT_BGRA32: the server's byte order (B, G, R, A in memory).
pub const PIXEL_FORMAT_BGRA32: u32 = pixel_format(32, FREERDP_PIXEL_FORMAT_TYPE_BGRA, 8, 8, 8, 8);
