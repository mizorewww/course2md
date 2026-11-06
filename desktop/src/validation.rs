//! Validation-build diagnostics.

/// Opt-in diagnostics for validation builds launched through LaunchServices.
/// Do not capture field values or broad application logs. A panic records its
/// source location and backtrace; its payload may contain user data and is omitted.
pub fn init_validation_diagnostics() {
    #[cfg(all(debug_assertions, unix))]
    {
        use std::{
            fs::OpenOptions,
            io::Write,
            os::unix::fs::OpenOptionsExt,
            sync::{Arc, Mutex},
        };
        let Some(path) = std::env::var_os("COURSE2MD_VALIDATION_PANIC_LOG") else {
            return;
        };
        let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
        else {
            return;
        };
        let _ = writeln!(
            file,
            "validation panic hook installed; pid={}",
            std::process::id()
        );
        let file = Arc::new(Mutex::new(file));
        std::panic::set_hook(Box::new(move |info| {
            if let Ok(mut file) = file.lock() {
                let _ = writeln!(file, "panic at {:?}", info.location());
                let _ = writeln!(file, "{}", std::backtrace::Backtrace::force_capture());
                let _ = file.flush();
            }
        }));
    }
}
