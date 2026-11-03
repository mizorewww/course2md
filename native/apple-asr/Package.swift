// swift-tools-version:6.0
import PackageDescription

let package = Package(
    name: "CAppleASR",
    platforms: [.macOS(.v15)],
    products: [
        .library(name: "CAppleASR", type: .static, targets: ["CAppleASR"])
    ],
    dependencies: [
        // Updating: change this revision, run `swift package resolve`, commit both files.
        .package(
            url: "https://github.com/soniqo/speech-swift",
            revision: "6f5aa4d0bca49c10d378723178465afaf813624c")
    ],
    targets: [
        .target(
            name: "CAppleASR",
            dependencies: [
                .product(name: "Qwen3ASR", package: "speech-swift"),
                .product(name: "WhisperASR", package: "speech-swift"),
                .product(name: "SpeechVAD", package: "speech-swift"),
                .product(name: "AudioCommon", package: "speech-swift"),
            ]
        )
    ]
)
