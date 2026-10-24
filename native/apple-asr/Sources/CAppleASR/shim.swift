// C ABI shim：Silero VAD (CoreML/ANE) + Qwen3-ASR (CoreML)。
// 供 Rust 侧静态链接；仅 macOS arm64 构建。

import Foundation
import Qwen3ASR
import WhisperASR
import SpeechVAD
import AudioCommon

/// 在同步 C ABI 里执行 async 代码（信号量 + 锁保护结果）。
private final class SyncBox<T>: @unchecked Sendable {
    private var value: Result<T, Error>?
    private let lock = NSLock()
    func set(_ r: Result<T, Error>) { lock.lock(); value = r; lock.unlock() }
    func get() throws -> T { lock.lock(); defer { lock.unlock() }; return try value!.get() }
}

private func runSync<T>(_ body: @escaping @Sendable () async throws -> T) throws -> T {
    let box = SyncBox<T>()
    let sem = DispatchSemaphore(value: 0)
    Task.detached {
        do { box.set(.success(try await body())) } catch { box.set(.failure(error)) }
        sem.signal()
    }
    sem.wait()
    return try box.get()
}

private func loadWav(_ path: String) throws -> [Float] {
    let url = URL(fileURLWithPath: path)
    let (samples, rate) = try AudioFileLoader.loadWAV(url: url)
    if rate == 16000 { return samples }
    return AudioFileLoader.resample(samples, from: rate, to: 16000)
}

// The last error is kept per thread: every exported function records its
// failure on the calling thread, and the Rust side reads it on that same
// thread right after the call, so concurrent callers cannot overwrite each
// other's message.
private let lastErrorKey = "dev.course2md.apple-asr.last-error"

private func setErr(_ message: String) {
    Thread.current.threadDictionary[lastErrorKey] = message as NSString
}

/// 本线程最近一次错误的可读描述（指针在本线程下次 setErr 前有效）。
@_cdecl("c2m_last_error")
public func c2mLastError() -> UnsafePointer<CChar> {
    if let message = Thread.current.threadDictionary[lastErrorKey] as? NSString,
       let text = message.utf8String {
        return text
    }
    // A string literal's storage is static and NUL-terminated.
    let fallback: StaticString = "unknown error"
    return UnsafeRawPointer(fallback.utf8Start).assumingMemoryBound(to: CChar.self)
}

// MARK: - Silero VAD

@_cdecl("c2m_vad_load")
public func c2mVadLoad() -> Int32 {
    do {
        _ = try runSync {
            try await SileroVADModel.fromPretrained(
                engine: .coreml, offlineMode: true, progressHandler: cachedLoadProgress)
        }
        return 0
    } catch {
        setErr("语音检测模型加载失败 / VAD model failed to load: \(error)")
        return -1
    }
}

@_cdecl("c2m_vad_detect")
public func c2mVadDetect(
    wavPath: UnsafePointer<CChar>,
    minSpeech: Double,
    minSilence: Double,
    outStarts: UnsafeMutablePointer<UnsafeMutablePointer<Double>?>,
    outEnds: UnsafeMutablePointer<UnsafeMutablePointer<Double>?>,
    outN: UnsafeMutablePointer<Int32>
) -> Int32 {
    outStarts.pointee = nil
    outEnds.pointee = nil
    outN.pointee = 0
    do {
        let samples = try loadWav(String(cString: wavPath))
        let vad = try runSync {
            try await SileroVADModel.fromPretrained(engine: .coreml, offlineMode: true, progressHandler: cachedLoadProgress)
        }
        var config = VADConfig.sileroDefault
        config.minSpeechDuration = Float(minSpeech)
        config.minSilenceDuration = Float(minSilence)
        let segments = vad.detectSpeech(audio: samples, sampleRate: 16000, config: config)
        let n = segments.count
        guard n > 0 else { return 0 }
        let starts = malloc(MemoryLayout<Double>.stride * n)!.assumingMemoryBound(to: Double.self)
        let ends = malloc(MemoryLayout<Double>.stride * n)!.assumingMemoryBound(to: Double.self)
        for (i, s) in segments.enumerated() {
            starts[i] = Double(s.startTime)
            ends[i] = Double(s.endTime)
        }
        outStarts.pointee = starts
        outEnds.pointee = ends
        outN.pointee = Int32(n)
        return 0
    } catch {
        setErr("语音检测失败 / Speech detection failed: \(error)")
        return -1
    }
}

@_cdecl("c2m_free_doubles")
public func c2mFreeDoubles(_ p: UnsafeMutablePointer<Double>?) {
    if let p = p { free(p) }
}

@_silgen_name("c2m_model_progress")
private func reportModelProgress(
    _ progress: Double, _ phase: UnsafePointer<CChar>, _ message: UnsafePointer<CChar>)

private func progressLog(_ p: Double, _ phase: String, _ msg: String) {
    phase.withCString { phasePtr in
        msg.withCString { reportModelProgress(p, phasePtr, $0) }
    }
}

// Rust prepares missing files before loading, and loaders run offline, so every
// progress report here is a local load whatever the library labels it.
private func cachedLoadProgress(_ p: Double, _ msg: String) {
    progressLog(p, "load", "正在加载本机模型… / Loading local model…")
}

// MARK: - Byte-accurate download progress
//
// `downloadWeights` only reports a byte-weighted fraction of one repository.
// To show bytes, speed and remaining time for the whole model, the files each
// call will fetch are resolved here first, from the same public manifest and
// with the same selection, and Rust turns each fraction back into bytes. The
// transfer itself stays with `downloadWeights`, so its cache fallback, retry
// ladder and redundant-weights cleanup are unchanged.

@_silgen_name("c2m_model_transfer_plan")
private func reportTransferPlan(_ plan: UnsafePointer<CChar>) -> Int32

@_silgen_name("c2m_model_transfer_progress")
private func reportTransferProgress(_ repo: Int32, _ fraction: Double)

/// The globs `downloadWeights` builds from `additionalFiles`.
private func weightGlobs(_ additionalFiles: [String]) -> [String] {
    var patterns = ["config.json"]
    if !additionalFiles.contains(where: { $0.hasSuffix(".safetensors") }) {
        patterns.append("*.safetensors")
        patterns.append(shardIndexName)
    }
    for file in additionalFiles where !patterns.contains(file) {
        patterns.append(file)
    }
    return patterns
}

private let shardIndexName = "model.safetensors.index.json"

private func hubEndpoint() -> String {
    (HuggingFaceDownloader.resolvedEndpoint() ?? "https://huggingface.co")
        .trimmingCharacters(in: CharacterSet(charactersIn: "/"))
}

/// The shard index, fetched the way the library does; `nil` leaves the
/// selection alone, as it does there.
private func shardIndex(modelId: String, manifest: RepoManifest) async -> Data? {
    guard manifest.file(at: shardIndexName) != nil,
          let url = URL(string: "\(hubEndpoint())/\(modelId)/resolve/main/\(shardIndexName)")
    else { return nil }
    var request = URLRequest(url: url, timeoutInterval: 30)
    let env = ProcessInfo.processInfo.environment
    if let token = env["HF_TOKEN"] ?? env["HUGGING_FACE_HUB_TOKEN"],
       !token.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
    }
    guard let (data, response) = try? await URLSession.shared.data(for: request),
          let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode)
    else { return nil }
    return data
}

/// When an index names the shards, other `.safetensors` are redundant copies
/// the library does not fetch.
private func narrowedToShards(_ selection: [RepoFile], indexData: Data) -> [RepoFile] {
    guard let json = try? JSONSerialization.jsonObject(with: indexData) as? [String: Any],
          let weightMap = json["weight_map"] as? [String: String]
    else { return selection }
    let shards = Set(weightMap.values)
    guard !shards.isEmpty else { return selection }
    return selection.filter { !$0.path.hasSuffix(".safetensors") || shards.contains($0.path) }
}

private func environmentBytes(_ name: String, default value: Int64) -> Int64 {
    if let raw = ProcessInfo.processInfo.environment[name], let v = Int64(raw), v > 0 {
        return v
    }
    return value
}

private func fileSize(_ url: URL) -> Int64? {
    (try? FileManager.default.attributesOfItem(atPath: url.path)[.size] as? NSNumber)?
        .int64Value
}

/// Bytes of `file` already on disk: the whole file when it is complete, or the
/// chunks a resumable transfer has staged. A staged transfer resumes without
/// the network, so counting it as fresh bytes would inflate the speed.
///
/// The staging layout follows speech-swift's `HuggingFaceTransfer.swift`. If it
/// ever changes, staged bytes read as zero and only a resumed transfer's first
/// speed sample is high.
private func presentBytes(_ file: RepoFile, in directory: URL) -> Int64 {
    guard let local = try? HuggingFaceDownloader.validatedLocalPath(
        directory: directory, relativePath: file.path)
    else { return 0 }
    if fileSize(local) == file.size { return file.size }
    let threshold = environmentBytes("HF_DOWNLOAD_RANGE_THRESHOLD", default: 8 * 1_024 * 1_024)
    guard file.size >= threshold else { return 0 }

    var hash: UInt64 = 0xcbf2_9ce4_8422_2325
    for byte in file.path.utf8 {
        hash ^= UInt64(byte)
        hash = hash &* 0x0000_0100_0000_01b3
    }
    let name = String(format: "%08x", UInt32(truncatingIfNeeded: hash)) + "-"
        + file.path.replacingOccurrences(of: "/", with: "_")
    let staging = directory.appendingPathComponent(".incomplete", isDirectory: true)
        .appendingPathComponent(name)
    guard fileSize(staging) == file.size,
          let data = try? Data(contentsOf: staging.appendingPathExtension("chunks")),
          let indices = try? JSONSerialization.jsonObject(with: data) as? [Int]
    else { return 0 }
    let chunk = environmentBytes("HF_DOWNLOAD_RANGE_CHUNK", default: 8 * 1_024 * 1_024)
    let count = Int((file.size + chunk - 1) / chunk)
    return Set(indices).filter { $0 >= 0 && $0 < count }.reduce(Int64(0)) { sum, index in
        sum + min(chunk, file.size - Int64(index) * chunk)
    }
}

/// Every repository's files in transfer order, as `[size, bytes on disk]`, plus
/// the host they come from, as JSON for `c2m_model_transfer_plan`.
private func transferPlan(_ downloads: [(String, [String])], directories: [URL]) async throws
    -> String
{
    var repos: [[String: Any]] = []
    for (index, item) in downloads.enumerated() {
        let manifest = try await HuggingFaceDownloader.fetchManifest(modelId: item.0)
        var selection = manifest.matching(globs: weightGlobs(item.1))
        if let indexData = await shardIndex(modelId: item.0, manifest: manifest) {
            selection = narrowedToShards(selection, indexData: indexData)
        }
        repos.append([
            "repo": item.0,
            "files": selection.map { [$0.size, presentBytes($0, in: directories[index])] },
        ])
    }
    let host = URL(string: hubEndpoint())?.host ?? "huggingface.co"
    let data = try JSONSerialization.data(withJSONObject: ["host": host, "repos": repos])
    return String(decoding: data, as: UTF8.self)
}

// MARK: - ASR：qwen3（CoreML 0.6B/ANE）| qwen3-1.7b（MLX/GPU）| whisper

enum AsrKind {
    case qwen(CoreMLASRModel)
    case qwenMlx(Qwen3ASRModel)
    case whisper(WhisperASRModel)
}

final class AsrBox {
    let model: AsrKind
    init(_ m: AsrKind) { model = m }
}

// Cache-only preparation follows the same repositories and required files as the
// actual loaders. It deliberately avoids constructing a second copy of ASR weights.
@_cdecl("c2m_asr_prepare_cache")
public func c2mAsrPrepareCache(
    _ model: UnsafePointer<CChar>,
    _ manifest: UnsafePointer<CChar>
) -> Int32 {
    let name = String(cString: model)
    let manifestText = String(cString: manifest)
    do {
        try runSync {
            let offline = ProcessInfo.processInfo.environment["HF_HUB_OFFLINE"]
                .map { ["1", "true", "yes", "on"].contains($0.lowercased()) } ?? false
            let data = manifestText.data(using: .utf8)!
            guard let items = try JSONSerialization.jsonObject(with: data) as? [[String: Any]] else {
                throw NSError(domain: "course2md", code: 1, userInfo: [
                    NSLocalizedDescriptionKey: "模型清单无效 / Invalid model manifest",
                ])
            }
            let downloads: [(String, [String])] = try items.map { item in
                guard let repo = item["repo"] as? String,
                      let files = item["files"] as? [String] else {
                    throw NSError(domain: "course2md", code: 1, userInfo: [
                        NSLocalizedDescriptionKey: "模型清单无效 / Invalid model manifest",
                    ])
                }
                return (repo, files)
            }
            let directories = try downloads.map {
                try HuggingFaceDownloader.getCacheDirectory(for: $0.0)
            }
            // Byte progress needs every repository's size before the first
            // transfer. Without it (offline, or the Hub unreachable now) the
            // download still runs and falls back to the cache as before; it
            // just reports a fraction rather than invented byte counts.
            let planned: Bool
            if !offline,
               let plan = try? await transferPlan(downloads, directories: directories) {
                planned = plan.withCString { reportTransferPlan($0) } == 0
            } else {
                planned = false
            }
            for (index, item) in downloads.enumerated() {
                try await HuggingFaceDownloader.downloadWeights(
                    modelId: item.0,
                    to: directories[index],
                    additionalFiles: item.1,
                    offlineMode: offline,
                    progressHandler: { fraction in
                        if planned {
                            reportTransferProgress(Int32(index), fraction)
                        } else {
                            progressLog(
                                (Double(index) + fraction) / Double(downloads.count),
                                "download", item.0)
                        }
                    })
            }
        }
        return 0
    } catch {
        setErr("Apple 模型下载失败 / Apple model download failed (\(name)): \(error)")
        return -1
    }
}

@_cdecl("c2m_asr_create")
public func c2mAsrCreate(_ model: UnsafePointer<CChar>) -> UnsafeMutableRawPointer? {
    let name = String(cString: model)
    do {
        let kind: AsrKind
        if name == "whisper" {
            let m = try runSync {
                try await WhisperASRModel.fromPretrained(offlineMode: true, progressHandler: cachedLoadProgress)
            }
            kind = .whisper(m)
        } else if name == "qwen3-1.7b" {
            // MLX/GPU 路径（1.7B 无 CoreML/ANE 导出）：WER 约为 0.6B 一半、
            // 速度约 3 倍，代价是 ~2.7GB RSS 且放弃 ANE 低功耗。
            let m = try runSync {
                try await Qwen3ASRModel.fromPretrained(
                    modelId: "aufklarer/Qwen3-ASR-1.7B-MLX-8bit",
                    offlineMode: true,
                    progressHandler: cachedLoadProgress)
            }
            kind = .qwenMlx(m)
        } else if name == "qwen3-0.6b" {
            let m = try runSync {
                try await CoreMLASRModel.fromPretrained(offlineMode: true, progressHandler: cachedLoadProgress)
            }
            try m.warmUp()
            kind = .qwen(m)
        } else {
            throw NSError(domain: "course2md", code: 1, userInfo: [NSLocalizedDescriptionKey: "不支持的 Apple 模型 / Unsupported Apple model: \(name)"])
        }
        return Unmanaged.passRetained(AsrBox(kind)).toOpaque()
    } catch {
        setErr("识别模型加载失败 / Speech model failed to load (\(name)): \(error)")
        return nil
    }
}

@_cdecl("c2m_asr_transcribe")
public func c2mAsrTranscribe(
    _ handle: UnsafeMutableRawPointer,
    wavPath: UnsafePointer<CChar>,
    outText: UnsafeMutablePointer<CChar>,
    outLen: Int
) -> Int32 {
    let box = Unmanaged<AsrBox>.fromOpaque(handle).takeUnretainedValue()
    do {
        let samples = try loadWav(String(cString: wavPath))
        let text: String
        switch box.model {
        case .qwen(let m):
            text = try m.transcribe(audio: samples, sampleRate: 16000, language: nil, maxTokens: 448)
        case .qwenMlx(let m):
            text = m.transcribe(audio: samples, sampleRate: 16000, language: nil, maxTokens: 448)
        case .whisper(let m):
            text = try runSync {
                try await m.transcribeAudio(samples, sampleRate: 16000, language: nil)
            }
        }
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        if trimmed.isEmpty {
            outText.pointee = 0
            return 1 // 1 = 无文本（非错误）
        }
        let fits = trimmed.utf8CString.count <= outLen // utf8CString 含结尾 NUL
        trimmed.withCString { cStr in
            _ = strncpy(outText, cStr, outLen - 1)
        }
        // strncpy leaves the last byte alone when the text fills the buffer
        // exactly or is truncated; terminate here so the caller need not
        // pre-zero its buffer.
        outText[outLen - 1] = 0
        if !fits {
            return 2 // 2 = 成功但被截断（Rust 侧据此 warn，不再静默截断）
        }
        return 0
    } catch {
        setErr("转写失败 / Transcription failed: \(error)")
        return -1
    }
}

@_cdecl("c2m_asr_destroy")
public func c2mAsrDestroy(_ handle: UnsafeMutableRawPointer) {
    Unmanaged<AsrBox>.fromOpaque(handle).release()
}
