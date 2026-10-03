// OpenVR's C API, from the header pinned in this crate (Valve's openvr v2.15.6, BSD-3-Clause):
// SteamVR's own bundled header predates IVRIPCResourceManagerClient (ImportDmabuf).
// libopenvr_api comes from the SteamVR runtime (/opt/steamvr in the container).
fn main() {
    let lib = std::env::var("OPENVR_LIB").unwrap_or("/opt/steamvr/bin/linuxarm64".into());
    println!("cargo:rustc-link-search=native={lib}");
    println!("cargo:rustc-link-lib=dylib=openvr_api");
    println!("cargo:rerun-if-changed=openvr_capi.h");
    bindgen::Builder::default()
        .header("openvr_capi.h")
        .clang_args(["-x", "c", "-DEXTERN_C="])
        .prepend_enum_name(false)
        .derive_default(true)
        .generate()
        .expect("openvr bindings")
        .write_to_file(std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("openvr.rs"))
        .expect("write openvr bindings");
}
