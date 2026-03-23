fn main() {
    // FreeType for font rendering.
    println!("cargo:rustc-link-lib=freetype");
    if let Ok(lib) = pkg_config::probe_library("freetype2") {
        for path in &lib.link_paths {
            println!("cargo:rustc-link-search=native={}", path.display());
        }
    }

    // EGL — needed for eglGetProcAddress / eglCreateImageKHR / eglDestroyImageKHR.
    // libEGL.so.1 is provided by Mesa (or the vendor driver) and is always
    // present if GBM + DRM rendering works at all.
    println!("cargo:rustc-link-lib=dylib=EGL");
}
