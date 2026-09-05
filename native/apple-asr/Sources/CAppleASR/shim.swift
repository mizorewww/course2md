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

// 单线程使用（Rust 侧在同一个阻塞线程内串行调用）。
nonisolated(unsafe) private var lastErrorCStr: UnsafeMutablePointer<CChar>?

private func setErr(_ message: String) {
    message.withCString { c in
        if let old = lastErrorCStr { free(old) }
        lastErrorCStr = strdup(c)
    }
}

/// 最近一次错误的可读描述（指针在下次 setErr 前有效）。
@_cdecl("c2m_last_error")
public func c2mLastError() -> UnsafePointer<CChar> {
    if let p = lastErrorCStr { return UnsafePointer(p) }
    return ("unknown error" as NSString).utf8String!
}

// MARK: - Silero VAD

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
            try await SileroVADModel.fromPretrained(engine: .coreml, progressHandler: progressLog)
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
        setErr("VAD 失败: \(error)")
        return -1
    }
}

@_cdecl("c2m_free_doubles")
public func c2mFreeDoubles(_ p: UnsafeMutablePointer<Double>?) {
    if let p = p { free(p) }
}

@_silgen_name("c2m_model_progress")
private func reportModelProgress(_ progress: Double, _ message: UnsafePointer<CChar>)

private func progressLog(_ p: Double, _ msg: String) {
    msg.withCString { reportModelProgress(p, $0) }
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

@_cdecl("c2m_asr_create")
public func c2mAsrCreate(_ model: UnsafePointer<CChar>, _ errBuf: UnsafeMutablePointer<CChar>, _ errLen: Int) -> UnsafeMutableRawPointer? {
    let name = String(cString: model)
    do {
        let kind: AsrKind
        if name == "whisper" {
            let m = try runSync {
                try await WhisperASRModel.fromPretrained(progressHandler: progressLog)
            }
            kind = .whisper(m)
        } else if name == "qwen3-1.7b" {
            // MLX/GPU 路径（1.7B 无 CoreML/ANE 导出）：WER 约为 0.6B 一半、
            // 速度约 3 倍，代价是 ~2.7GB RSS 且放弃 ANE 低功耗。
            let m = try runSync {
                try await Qwen3ASRModel.fromPretrained(
                    modelId: "aufklarer/Qwen3-ASR-1.7B-MLX-8bit",
                    progressHandler: progressLog)
            }
            kind = .qwenMlx(m)
        } else {
            let m = try runSync {
                try await CoreMLASRModel.fromPretrained(progressHandler: progressLog)
            }
            try m.warmUp()
            kind = .qwen(m)
        }
        return Unmanaged.passRetained(AsrBox(kind)).toOpaque()
    } catch {
        let msg = "ASR init failed (\(name)): \(error)"
        msg.withCString { cStr in
            _ = strncpy(errBuf, cStr, errLen - 1)
        }
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
        if !fits {
            outText[outLen - 1] = 0 // strncpy 截断时不保证补 NUL
            return 2 // 2 = 成功但被截断（Rust 侧据此 warn，不再静默截断）
        }
        return 0
    } catch {
        setErr("转写失败: \(error)")
        return -1
    }
}

@_cdecl("c2m_asr_destroy")
public func c2mAsrDestroy(_ handle: UnsafeMutableRawPointer) {
    Unmanaged<AsrBox>.fromOpaque(handle).release()
}
