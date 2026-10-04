// libvncclient from Command Center's own build (panels/third_party/vnc-prefix: static, OpenSSL,
// libjpeg-turbo, zlib; cc-home install builds it). VNC_PREFIX overrides where it is. It's static,
// so cc-panels needs no extra RUNPATH, only the container's libssl, libcrypto, libjpeg and libz.
use std::path::PathBuf;

fn main() {
    let here = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    println!("cargo:rerun-if-env-changed=VNC_PREFIX");
    let prefix = std::env::var("VNC_PREFIX")
        .map(PathBuf::from)
        .unwrap_or_else(|_| here.join("../../panels/third_party/vnc-prefix").canonicalize().expect("no libvncclient build: run cc-home install"));
    println!("cargo:rustc-link-search=native={}", prefix.join("lib64").display());
    println!("cargo:rustc-link-lib=static=vncclient");
    for l in ["ssl", "crypto", "jpeg", "z"] {
        println!("cargo:rustc-link-lib=dylib={l}");
    }
    println!("cargo:rerun-if-changed=wrapper.h");
    println!("cargo:rerun-if-changed={}", prefix.join("include/rfb/rfbclient.h").display());
    bindgen::Builder::default()
        .header("wrapper.h")
        .clang_arg(format!("-I{}", prefix.join("include").display()))
        .allowlist_file(".*/rfb/.*")
        .derive_default(true)
        .layout_tests(false)
        .generate()
        .expect("libvncclient bindings")
        .write_to_file(PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("vncclient.rs"))
        .expect("write libvncclient bindings");
}
