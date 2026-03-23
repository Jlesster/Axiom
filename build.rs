fn main() {
    // Link FreeType for font rendering.
    println!("cargo:rustc-link-lib=freetype");

    // Let pkg-config find the right include/lib paths if needed.
    if let Ok(lib) = pkg_config::probe_library("freetype2") {
        for path in &lib.link_paths {
            println!("cargo:rustc-link-search=native={}", path.display());
        }
    }
}
