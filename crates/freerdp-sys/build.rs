// FreeRDP 3 from Command Center's own build (panels/third_party/prefix: FFmpeg H.264, no X11
// or Wayland clients). FREERDP_PREFIX overrides where it is.
use std::path::PathBuf;

fn main() {
    let here = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let prefix = std::env::var("FREERDP_PREFIX")
        .map(PathBuf::from)
        .unwrap_or_else(|_| here.join("../../panels/third_party/prefix").canonicalize().expect("no FreeRDP build: run install.sh"));
    let lib = prefix.join("lib64");
    println!("cargo:rustc-link-search=native={}", lib.display());
    for l in ["freerdp-client3", "freerdp3", "winpr3"] {
        println!("cargo:rustc-link-lib=dylib={l}");
    }
    println!("cargo:rerun-if-changed=wrapper.h");
    let inc = prefix.join("include");
    bindgen::Builder::default()
        .header("wrapper.h")
        .clang_arg(format!("-I{}", inc.join("freerdp3").display()))
        .clang_arg(format!("-I{}", inc.join("winpr3").display()))
        .allowlist_file(".*/(freerdp3|winpr3)/.*")  // FreeRDP's and WinPR's own declarations
        .derive_default(true)
        .layout_tests(false)
        .generate()
        .expect("FreeRDP bindings")
        .write_to_file(PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("freerdp.rs"))
        .expect("write FreeRDP bindings");
}
