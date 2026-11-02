fn main() {
    // The engine can link the native Apple speech module. A library's rustc-link-arg is
    // not inherited by its consumers, so this crate's test executable supplies the system
    // Swift runtime search path itself.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }
}
