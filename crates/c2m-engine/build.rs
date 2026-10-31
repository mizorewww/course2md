//! This crate's test binary links the Apple speech module through c2m-asr, and a
//! link argument does not reach it from there.

fn main() {
    if std::env::var_os("DEP_C2M_APPLE_ASR_APPLE_NATIVE").is_some() {
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }
}
