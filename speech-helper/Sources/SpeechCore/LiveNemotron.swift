// Live transcription: Nemotron Speech Streaming 0.6B, 560 ms chunks, on Core
// ML through FluidAudio's StreamingNemotronAsrManager
// (https://github.com/FluidInference/FluidAudio, Apache-2.0).

import AVFoundation
import CoreML
import FluidAudio
import Foundation

/// The text a SentencePiece piece stands for: the word marker `▁` becomes a
/// space (the form the app joins tokens in), special markers (`<unk>`, ...)
/// become "".
public enum TokenText {
    public static func text(_ piece: String) -> String {
        if piece.count >= 3, piece.first == "<", piece.last == ">", !piece.contains(" ") {
            return ""
        }
        return piece.replacingOccurrences(of: "\u{2581}", with: " ")
    }
}

/// Loaded `StreamingNemotronAsrManager`s, one per open stream.
///
/// A manager holds its own Core ML models next to one stream's state, and
/// FluidAudio 0.18.0 has no way to hand it models another manager loaded, so
/// two streams at once (microphone and system audio) hold two copies of the
/// models. The app opens a stream per utterance, so managers are reset and
/// reused rather than loaded per stream.
public final class NemotronPool {
    public let directory: URL
    public let compute: ComputeChoice
    private var idle: [StreamingNemotronAsrManager] = []
    public private(set) var loaded = 0

    /// Fails at once (with the missing path) if the model files are not there.
    public init(directory: URL, compute: ComputeChoice) throws {
        try ModelLayout.require(ModelLayout.nemotronFiles, in: directory)
        self.directory = directory
        self.compute = compute
    }

    private func load() async throws -> StreamingNemotronAsrManager {
        let started = Date()
        let manager = StreamingNemotronAsrManager(
            configuration: compute.configuration(), requestedChunkSize: .ms560)
        try await manager.loadModels(from: directory)
        let chunkMs = await manager.config.chunkMs
        guard chunkMs == NemotronChunkSize.ms560.rawValue else {
            throw HelperError.missing("the 560 ms Nemotron model (metadata.json says \(chunkMs) ms)")
        }
        loaded += 1
        logLine("loaded Nemotron streaming model \(loaded) (\(compute.rawValue)) in \(Int(millis(since: started))) ms")
        return manager
    }

    /// Load managers until `count` are loaded. The first load compiles the
    /// models for this compute choice (Core ML caches that); the rest are
    /// cache hits.
    public func preload(_ count: Int) async throws {
        while loaded < count {
            idle.append(try await load())
        }
    }

    /// One silent chunk through every idle manager, so the first real chunk
    /// does not pay for the device's first-prediction setup.
    public func warmUp() async throws {
        for manager in idle {
            let samples = await manager.config.chunkSamples
            _ = try await manager.process(audioBuffer: try pcmBuffer([Float](repeating: 0, count: samples)))
            await manager.reset()
        }
    }

    func take() async throws -> StreamingNemotronAsrManager {
        if let manager = idle.popLast() { return manager }
        return try await load()
    }

    func give(_ manager: StreamingNemotronAsrManager) async {
        await manager.reset()
        idle.append(manager)
    }
}

/// One live stream: resampled to 16 kHz, fed to its manager a 560 ms chunk at
/// a time.
///
/// Token times come from FluidAudio (`getTokenTimings()`: the encoder frame
/// each token was emitted on, 80 ms resolution, from the start of the
/// manager's session, which is the stream's start because managers are reset
/// between streams), so the app's segmentation gets real per-token times.
public final class NemotronSession: LiveSession {
    private let pool: NemotronPool
    private var manager: StreamingNemotronAsrManager?
    private let resampler: Resampler
    private let chunkSamples: Int
    private let chunkSeconds: Double

    /// 16 kHz audio not yet decoded.
    private var pending: [Float] = []
    private var finished = false
    private var chunksDecoded = 0
    /// Every token of the manager's session, as of the last decode.
    private var timings: [TokenTiming] = []
    /// Tokens before this index were cleared by `reset`.
    private var cleared = 0
    public private(set) var startTime = 0.0

    private init(pool: NemotronPool, manager: StreamingNemotronAsrManager, inputRate: Int, chunkSamples: Int) {
        self.pool = pool
        self.manager = manager
        self.resampler = Resampler(inputRate: inputRate)
        self.chunkSamples = chunkSamples
        self.chunkSeconds = Double(chunkSamples) / Double(Resampler.outputRate)
    }

    public static func open(pool: NemotronPool, inputRate: Int) async throws -> NemotronSession {
        let manager = try await pool.take()
        let chunkSamples = await manager.config.chunkSamples
        return NemotronSession(pool: pool, manager: manager, inputRate: inputRate, chunkSamples: chunkSamples)
    }

    public func accept(_ samples: [Float]) {
        pending.append(contentsOf: resampler.process(samples))
    }

    public func inputFinished() {
        finished = true
    }

    public func decodeReady() async throws {
        guard let manager else { throw HelperError.missing("the stream's model (closed)") }
        var decoded = false
        // Whole chunks; once the input is finished, the rest padded with
        // silence to a chunk.
        while pending.count >= chunkSamples || (finished && !pending.isEmpty) {
            let count = min(chunkSamples, pending.count)
            var chunk = Array(pending[..<count])
            pending.removeFirst(count)
            if count < chunkSamples {
                chunk.append(contentsOf: [Float](repeating: 0, count: chunkSamples - count))
            }
            _ = try await manager.process(audioBuffer: try pcmBuffer(chunk))
            chunksDecoded += 1
            decoded = true
        }
        if decoded {
            timings = await manager.getTokenTimings()
        }
    }

    public func reset() {
        cleared = timings.count
        startTime = Double(chunksDecoded) * chunkSeconds
    }

    public func hypothesis() -> (tokens: [String], times: [Double]) {
        var tokens: [String] = []
        var times: [Double] = []
        for timing in timings.dropFirst(cleared) {
            let text = TokenText.text(timing.token)
            if text.isEmpty { continue }
            tokens.append(text)
            times.append(max(0, timing.startTime - startTime))
        }
        return (tokens, times)
    }

    public func close() async {
        guard let manager else { return }
        self.manager = nil
        await pool.give(manager)
    }
}

private let monoFormat16k = AVAudioFormat(
    commonFormat: .pcmFormatFloat32, sampleRate: Double(Resampler.outputRate), channels: 1, interleaved: false)!

/// 16 kHz mono f32 samples as the buffer FluidAudio's streaming API takes
/// (already its target format, so it is read without conversion).
func pcmBuffer(_ samples: [Float]) throws -> AVAudioPCMBuffer {
    guard let buffer = AVAudioPCMBuffer(pcmFormat: monoFormat16k, frameCapacity: AVAudioFrameCount(max(1, samples.count)))
    else { throw HelperError.badRequest("could not allocate an audio buffer") }
    buffer.frameLength = AVAudioFrameCount(samples.count)
    if let channel = buffer.floatChannelData?[0] {
        samples.withUnsafeBufferPointer { source in
            if let base = source.baseAddress { channel.update(from: base, count: samples.count) }
        }
    }
    return buffer
}
