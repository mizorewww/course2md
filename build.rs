//! macOS arm64：编译并静态链接 Apple 原生 ASR/VAD 模块（speech-swift）。
//! 其他平台或设置 COURSE2MD_NO_APPLE=1 时跳过（course2md 回落 llama.cpp 路径）。

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    emit_commit_hash();
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let skip = std::env::var_os("COURSE2MD_NO_APPLE").is_some();
    println!("cargo:rerun-if-env-changed=COURSE2MD_NO_APPLE");
    println!("cargo:rerun-if-changed=native/apple-asr/Package.swift");
    println!("cargo:rerun-if-changed=native/apple-asr/Sources");

    if target_os != "macos" || target_arch != "aarch64" || skip {
        return;
    }
    if !swiftc_available() {
        println!(
            "cargo:warning=未找到 swiftc（需要 Xcode Command Line Tools），跳过 Apple 原生模块；coreml 后端不可用"
        );
        return;
    }

    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let pkg = manifest.join("native/apple-asr");
    let build_dir = pkg.join(".build/release");

    // 无条件跑 swift build：SPM 自己做增量（无变动时秒级返回）。
    // 曾经的「libCAppleASR.a 存在即跳过」stamp 判断是真 bug：
    // 只改 Swift 源码（不碰 Package.swift）时会链上旧的静态库。
    //
    // 优先 swiftbuild 构建系统（Swift 6.2+）：旧 native 系统不会编译
    // mlx-swift Cmlx 里的 .metal shader，产不出 default.metallib，
    // CoreML 推理运行时必挂（CI macos-26 默认 Xcode 较老时踩中）。
    // 工具链不支持 swiftbuild 时回退默认构建系统。
    let ok = run(Command::new("swift")
        .args(["build", "-c", "release", "--build-system", "swiftbuild"])
        .current_dir(&pkg))
        || run(Command::new("swift")
            .args(["build", "-c", "release"])
            .current_dir(&pkg));
    if !ok {
        println!(
            "cargo:warning=swift build 失败（见上方输出），跳过 Apple 原生模块；coreml 后端不可用"
        );
        return;
    }

    println!("cargo:rustc-cfg=apple_native");
    println!("cargo:rustc-link-search=native={}", build_dir.display());
    // libCAppleASR.a 已包含 speech-swift 及其依赖（MLX 等）的全部对象
    println!("cargo:rustc-link-lib=static=CAppleASR");
    // 框架（对象内嵌 autolink 提示，这里显式列出关键项以保证链接顺序）
    for fw in [
        "Foundation",
        "CoreML",
        "Metal",
        "Accelerate",
        "CoreFoundation",
        "AVFoundation",
        "AVFAudio",
        "AppKit",
        "CoreAudio",
        "CryptoKit",
        "NaturalLanguage",
        "Network",
        "Security",
        "CoreGraphics",
    ] {
        println!("cargo:rustc-link-lib=framework={fw}");
    }
    // Swift 运行时 overlay（/usr/lib/swift，macOS 15+ 系统自带）。
    // 下面这份手写 dylib 列表来源：`swift build -v` 的实际链接行；
    // Xcode 升级后若出现链接错误（缺 Swift 符号），需先核对本列表是否有增删。
    println!("cargo:rustc-link-search=native=/usr/lib/swift");
    for dylib in [
        "swiftCore",
        "swift_Concurrency",
        "swift_Builtin_float",
        "swift_errno",
        "swiftAccelerate",
        "swiftAVFoundation",
        "swiftCoreAudio",
        "swiftCoreFoundation",
        "swiftCoreImage",
        "swiftCoreMIDI",
        "swiftDarwin",
        "swiftDispatch",
        "swiftIOKit",
        "swiftMetal",
        "swiftNaturalLanguage",
        "swiftObjectiveC",
        "swiftObservation",
        "swiftos",
        "swiftOSLog",
        "swiftQuartzCore",
        "swiftRegexBuilder",
        "swiftsimd",
        "swiftSpatial",
        "swift_StringProcessing",
        "swiftUniformTypeIdentifiers",
        "swiftXPC",
    ] {
        println!("cargo:rustc-link-lib=dylib={dylib}");
    }
    println!("cargo:rustc-link-lib=dylib=c++");
    println!("cargo:rustc-link-lib=dylib=objc");
    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");

    // MLX 运行时需要 metallib 与可执行文件同目录（mlx.metallib）。
    // 默认取 SPM build 产物（mlx-swift_Cmlx.bundle 内的 default.metallib）；
    // 仅当仓库根存在手动放置的 mlx.metallib 时才优先用它（便于 runner 固定版本）。
    // 注意 .build 下的布局随构建系统不同（swiftbuild: out/Products/Release，
    // native: <triple>/release），canonical 路径找不到就递归搜索兜底。
    let vendored = pkg.join("mlx.metallib");
    let products = build_dir.canonicalize().unwrap_or(build_dir);
    let bundle = if vendored.is_file() {
        Some(vendored)
    } else {
        let expect = products.join("mlx-swift_Cmlx.bundle/Contents/Resources/default.metallib");
        if expect.is_file() {
            Some(expect)
        } else {
            find_file_recursive(&pkg.join(".build"), "default.metallib", 8)
        }
    };
    if let Some(bundle) = bundle {
        if let Ok(out_dir) = std::env::var("OUT_DIR") {
            // OUT_DIR = <target>/<profile>/build/<pkg>-<hash>/out
            let exe_dir = PathBuf::from(out_dir).join("../../../");
            let dest = exe_dir.join("mlx.metallib");
            if std::fs::copy(&bundle, &dest).is_err() {
                println!(
                    "cargo:warning=无法复制 mlx.metallib 到 {}",
                    exe_dir.display()
                );
            }
        }
    } else {
        println!(
            "cargo:warning=未找到 mlx.metallib（{} 下递归搜索无果），CoreML 推理可能失败",
            pkg.join(".build").display()
        );
    }
    // Swift 5 语言模式包的兼容钩子 + clang 运行时（___isPlatformVersionAtLeast 等）
    if let Some(swift_lib) = toolchain_swift_lib_dir() {
        println!("cargo:rustc-link-search=native={}", swift_lib.display());
        println!("cargo:rustc-link-lib=static=swiftCompatibility56");
    }
    if let Some(rt) = find_clang_rt_osx() {
        println!("cargo:rustc-link-search=native={}", rt.1.display());
        println!("cargo:rustc-link-lib=static=clang_rt.osx");
    }
}

/// toolchain 的 usr/lib/swift/macosx 目录。
fn toolchain_swift_lib_dir() -> Option<PathBuf> {
    let out = Command::new("xcode-select").arg("-p").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let devdir = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    if let Ok(rd) = std::fs::read_dir(devdir.join("Toolchains")) {
        for e in rd.flatten() {
            let d = e.path().join("usr/lib/swift/macosx");
            if d.join("libswiftCompatibility56.a").is_file() {
                return Some(d);
            }
        }
    }
    None
}

/// 定位 toolchain 内的 libclang_rt.osx.a，返回 (文件, 目录)。
fn find_clang_rt_osx() -> Option<(PathBuf, PathBuf)> {
    let out = Command::new("xcode-select").arg("-p").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let devdir = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let mut candidates = vec![];
    // clang 运行时位于 Toolchains/<TC>/usr/lib/clang/...（也兼容旧布局 usr/lib/clang）
    if let Ok(rd) = std::fs::read_dir(PathBuf::from(&devdir).join("Toolchains")) {
        for e in rd.flatten() {
            collect_a_files(&e.path().join("usr/lib/clang"), &mut candidates);
        }
    }
    collect_a_files(
        &PathBuf::from(&devdir).join("usr/lib/clang"),
        &mut candidates,
    );
    for p in candidates {
        let is_it = p.to_string_lossy().contains("darwin")
            && p.file_name().and_then(|s| s.to_str()) == Some("libclang_rt.osx.a");
        if is_it {
            let dir = p.parent()?.to_path_buf();
            return Some((p, dir));
        }
    }
    None
}

fn collect_a_files(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                collect_a_files(&p, out);
            } else if p.extension().and_then(|s| s.to_str()) == Some("a") {
                out.push(p);
            }
        }
    }
}

fn swiftc_available() -> bool {
    Command::new("xcrun")
        .args(["--find", "swift"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn run(cmd: &mut Command) -> bool {
    match cmd.output() {
        Ok(out) => {
            if !out.status.success() {
                let so = String::from_utf8_lossy(&out.stdout);
                let se = String::from_utf8_lossy(&out.stderr);
                println!(
                    "cargo:warning=swift build 退出码 {:?}（stdout 尾部）:\n{}\n（stderr 尾部）:\n{}",
                    out.status.code(),
                    so.lines()
                        .rev()
                        .take(40)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect::<Vec<_>>()
                        .join("\n"),
                    se.lines()
                        .rev()
                        .take(40)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
            }
            out.status.success()
        }
        Err(e) => {
            println!("cargo:warning=无法执行 {:?}: {e}", cmd.get_program());
            false
        }
    }
}

/// 在 dir 下递归查找指定文件名（限深，跳过 symlink 防环）。
/// 用于 SPM 产物定位：.build 布局随构建系统（swiftbuild/native）不同。
fn find_file_recursive(dir: &Path, name: &str, depth: u32) -> Option<PathBuf> {
    if depth == 0 {
        return None;
    }
    let entries = std::fs::read_dir(dir).ok()?;
    let mut dirs = vec![];
    for e in entries.flatten() {
        let p = e.path();
        if e.file_name() == name && p.is_file() {
            return Some(p);
        }
        // 不跟随符号链接（.build/release → out/Products/Release 会造成重复遍历）
        if p.is_dir() && !e.file_type().map(|t| t.is_symlink()).unwrap_or(false) {
            dirs.push(p);
        }
    }
    dirs.into_iter()
        .find_map(|d| find_file_recursive(&d, name, depth - 1))
}

/// Embed build provenance, including when this checkout is a linked worktree.
fn emit_commit_hash() {
    let root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let git = |args: &[&str]| -> Option<String> {
        let output = Command::new("git")
            .args(args)
            .current_dir(&root)
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let value = String::from_utf8(output.stdout).ok()?.trim().to_owned();
        (!value.is_empty()).then_some(value)
    };
    let mut commit = None;
    // Source archives have no .git; do not accidentally use an enclosing repository.
    if root.join(".git").exists() {
        // A worktree's .git is a pointer file. Watching the repository
        // directory would also rebuild on index refreshes and unrelated refs.
        if root.join(".git").is_file() {
            println!("cargo:rerun-if-changed={}", root.join(".git").display());
        }
        let symbolic_ref = git(&["symbolic-ref", "--quiet", "HEAD"]);
        for name in [
            Some("HEAD"),
            symbolic_ref.as_deref(),
            Some("packed-refs"),
            Some("logs/HEAD"),
        ]
        .into_iter()
        .flatten()
        {
            if let Some(path) = git(&["rev-parse", "--git-path", name]) {
                let path = root.join(path);
                if path.is_file() {
                    println!("cargo:rerun-if-changed={}", path.display());
                }
            }
        }
        commit = git(&["rev-parse", "--short=12", "HEAD"]);
    }
    println!(
        "cargo:rustc-env=COURSE2MD_COMMIT_HASH={}",
        commit.as_deref().unwrap_or("unknown")
    );
}
