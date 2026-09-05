fn main() {
    // The shared core can link the native Apple speech module. A library's
    // rustc-link-arg is not inherited by its consumers, so each final executable
    // must supply the system Swift runtime search path.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }
}
