fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    let profile = std::env::var("PROFILE").unwrap_or_default();

    // When compiling in debug mode on MSVC, audiopus_sys links against MSVCRTD
    // but Rust's defaults use MSVCRT.
    if target.contains("msvc") && profile == "debug" {
        println!("cargo:rustc-link-arg=/NODEFAULTLIB:msvcrt");
        println!("cargo:rustc-link-lib=dylib=msvcrtd");
    }
}
