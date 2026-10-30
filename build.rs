//! macOS arm64：编译并静态链接 Apple 原生 ASR/VAD 模块（speech-swift）。
//! 其他平台或设置 COURSE2MD_NO_APPLE=1 时跳过（course2md 回落 llama.cpp 路径）。

use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "build_support/git.rs"]
mod git;

fn main() {
    emit_commit_hash();
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let skip = std::env::var_os("COURSE2MD_NO_APPLE").is_some();
    println!("cargo:rerun-if-env-changed=COURSE2MD_NO_APPLE");
    println!("cargo:rerun-if-changed=native/apple-asr/Package.swift");
    // swift package update 只改 resolved 文件；漏掉它会让本地构建静默链接旧静态库
    println!("cargo:rerun-if-changed=native/apple-asr/Package.resolved");
    println!("cargo:rerun-if-changed=native/apple-asr/Sources");

    if target_os != "macos" || target_arch != "aarch64" || skip {
        return;
    }
    if !swiftc_available() {
        without_apple_module(
            "未找到 swiftc（需要 Xcode Command Line Tools），跳过 Apple 原生模块；coreml 后端不可用",
            &[],
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
    //
    // 关闭 swift-syntax 预编译包：新工具链首次构建会从 download.swift.org
    // 下载预编译宏库，SwiftPM 的下载没有超时，网络慢时整个 cargo build
    // 无限挂起；从源码构建宏库只慢在首次。不认识该参数的旧工具链本就不下载
    // 预编译包。
    //
    // 不认识这两个参数的旧工具链依次回退。`--help` 的文字因版本而异，不能据此
    // 判断（旧 Xcode 的帮助不列出 swiftbuild，却支持它），所以按实际构建结果
    // 选择；成功的参数组合按工具链版本缓存在 OUT_DIR，之后只跑一次 swift build。
    if let Err(reports) = build_swift_package(&pkg) {
        without_apple_module(
            "swift build 失败（见上方输出），跳过 Apple 原生模块；coreml 后端不可用",
            &reports,
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
            // OUT_DIR = <target>/[<triple>/]<profile>/build/<pkg>-<hash>/out, so
            // this is the executable's directory. Cargo has no stable way to
            // name it; the layout is also hard-coded by the consumers of this
            // copy: release.yml ("Collect mlx.metallib"),
            // desktop/scripts/package.py and desktop/scripts/ux_fixture.py.
            let exe_dir = PathBuf::from(out_dir).join("../../../");
            let dest = exe_dir.join("mlx.metallib");
            if let Err(error) = std::fs::copy(&bundle, &dest) {
                without_metallib(&format!(
                    "无法复制 mlx.metallib 到 {}：{error}",
                    exe_dir.display()
                ));
            }
        }
    } else {
        without_metallib(&format!(
            "未找到 mlx.metallib（{} 下递归搜索无果），CoreML 推理可能失败",
            pkg.join(".build").display()
        ));
    }
    // Swift 5 语言模式包的兼容钩子 + clang 运行时（___isPlatformVersionAtLeast 等）
    let devdir = xcode_devdir();
    if let Some(swift_lib) = devdir.as_deref().and_then(toolchain_swift_lib_dir) {
        println!("cargo:rustc-link-search=native={}", swift_lib.display());
        println!("cargo:rustc-link-lib=static=swiftCompatibility56");
    }
    if let Some(rt) = devdir.as_deref().and_then(find_clang_rt_osx_dir) {
        println!("cargo:rustc-link-search=native={}", rt.display());
        println!("cargo:rustc-link-lib=static=clang_rt.osx");
    }
}

/// The Apple module cannot be built. A development build carries on without it, so
/// a machine lacking Xcode can still work on everything else; a release build stops,
/// because it would otherwise ship without local speech recognition and say nothing.
/// `COURSE2MD_NO_APPLE=1` is how a release leaves the module out on purpose.
fn without_apple_module(reason: &str, reports: &[String]) {
    if std::env::var("PROFILE").as_deref() == Ok("release") {
        for report in reports {
            eprintln!("{report}");
        }
        eprintln!("{reason}");
        eprintln!(
            "release builds must include the Apple module; set COURSE2MD_NO_APPLE=1 to build without it"
        );
        std::process::exit(1);
    }
    for report in reports {
        println!("cargo:warning={report}");
    }
    println!("cargo:warning={reason}");
}

/// The Swift build succeeded but left no Metal library, as the native build system
/// does, for one. Apple recognition would fail at run time, so a release stops here
/// as it does for a failed build; a development build only warns.
fn without_metallib(reason: &str) {
    if std::env::var("PROFILE").as_deref() == Ok("release") {
        eprintln!("{reason}");
        eprintln!(
            "release builds must include mlx.metallib; set COURSE2MD_NO_APPLE=1 to build without the Apple module"
        );
        std::process::exit(1);
    }
    println!("cargo:warning={reason}");
}

/// Builds the Swift package with the newest option set the toolchain accepts:
/// the Swift Build engine (which compiles MLX's Metal shaders into
/// default.metallib) and no prebuilt swift-syntax download. The set that
/// worked is cached per toolchain version, so a rebuild runs swift once.
fn build_swift_package(pkg: &Path) -> Result<(), Vec<String>> {
    const ATTEMPTS: [&[&str]; 4] = [
        &[
            "--build-system",
            "swiftbuild",
            "--disable-experimental-prebuilts",
        ],
        &["--build-system", "swiftbuild"],
        &["--disable-experimental-prebuilts"],
        &[],
    ];
    let version = Command::new("swift")
        .arg("--version")
        .output()
        .map(|out| {
            let text = String::from_utf8_lossy(&out.stdout);
            text.lines().next().unwrap_or_default().trim().to_owned()
        })
        .unwrap_or_default();
    let cache = PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("swift-build-args");
    let cached: Option<Vec<String>> = std::fs::read_to_string(&cache).ok().and_then(|text| {
        let mut lines = text.lines();
        (lines.next() == Some(version.as_str())).then(|| lines.map(str::to_owned).collect())
    });
    let build = |extra: &[String]| {
        run(Command::new("swift")
            // Only what Package.resolved records: a build never moves a dependency.
            .args(["build", "-c", "release", "--force-resolved-versions"])
            .args(extra)
            .current_dir(pkg))
    };
    if let Some(extra) = &cached
        && build(extra).is_ok()
    {
        return Ok(());
    }
    let mut reports = Vec::new();
    for attempt in ATTEMPTS {
        let extra: Vec<String> = attempt.iter().map(|arg| (*arg).to_owned()).collect();
        if cached.as_ref() == Some(&extra) {
            continue;
        }
        match build(&extra) {
            Ok(()) => {
                let _ = std::fs::write(&cache, [version, extra.join("\n")].join("\n"));
                return Ok(());
            }
            Err(report) => reports.push(report),
        }
    }
    Err(reports)
}

fn xcode_devdir() -> Option<PathBuf> {
    let out = Command::new("xcode-select").arg("-p").output().ok()?;
    out.status
        .success()
        .then(|| PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
}

/// toolchain 的 usr/lib/swift/macosx 目录。
fn toolchain_swift_lib_dir(devdir: &Path) -> Option<PathBuf> {
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

/// 定位 toolchain 内的 libclang_rt.osx.a，返回其所在目录。
fn find_clang_rt_osx_dir(devdir: &Path) -> Option<PathBuf> {
    let mut candidates = vec![];
    // clang 运行时位于 Toolchains/<TC>/usr/lib/clang/...（也兼容旧布局 usr/lib/clang）
    if let Ok(rd) = std::fs::read_dir(devdir.join("Toolchains")) {
        for e in rd.flatten() {
            collect_a_files(&e.path().join("usr/lib/clang"), &mut candidates);
        }
    }
    collect_a_files(&devdir.join("usr/lib/clang"), &mut candidates);
    for p in candidates {
        let is_it = p.to_string_lossy().contains("darwin")
            && p.file_name().and_then(|s| s.to_str()) == Some("libclang_rt.osx.a");
        if is_it {
            return p.parent().map(Path::to_path_buf);
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

fn tail(text: &str, lines: usize) -> String {
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

/// 运行构建命令；失败时返回可直接打印的输出尾部。
fn run(cmd: &mut Command) -> Result<(), String> {
    match cmd.output() {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => Err(format!(
            "swift build 退出码 {:?}（stdout 尾部）:\n{}\n（stderr 尾部）:\n{}",
            out.status.code(),
            tail(&String::from_utf8_lossy(&out.stdout), 40),
            tail(&String::from_utf8_lossy(&out.stderr), 40),
        )),
        Err(e) => Err(format!("无法执行 {:?}: {e}", cmd.get_program())),
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
    let commit = git::git_commit(&root);
    println!(
        "cargo:rustc-env=COURSE2MD_COMMIT_HASH={}",
        commit.as_deref().unwrap_or("unknown")
    );
}
