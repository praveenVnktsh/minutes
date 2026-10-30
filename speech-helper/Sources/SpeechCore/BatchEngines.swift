// The batch role's models, through FluidAudio: Parakeet TDT 0.6B v2 for the
// final transcript, Silero VAD, and the offline pyannote community-1 +
// WeSpeaker diarizer. Each loads on first use, from the app's directory
// (see ModelLayout), with Core ML directly.

import AVFoundation
import CoreML
import FluidAudio
import Foundation

public final class FluidBatchEngine: BatchEngine {
    public let layout: ModelLayout
    public let compute: ComputeChoice

    private var asr: AsrManager?
    private var vad: VadManager?
    private var diarizerModels: OfflineDiarizerModels?

    /// Silero hysteresis as the app used before FluidAudio: enter at 0.5,
    /// leave at 0.35 (FluidAudio's exit offset is 0.15).
    static let vadThreshold: Float = 0.5
    static let segmentation = VadSegmentationConfig(
        minSpeechDuration: 0.25, minSilenceDuration: 0.5, maxSpeechDuration: 25, speechPadding: 0.1)
    /// VAD runs over the file in blocks of whole 256 ms chunks (~300 s), so
    /// a long meeting is never in memory at once.
    static let vadBlockSamples = VadManager.chunkSize * 1170

    public init(layout: ModelLayout, compute: ComputeChoice) {
        self.layout = layout
        self.compute = compute
    }

    // MARK: - Loading

    func loadAsr() throws -> AsrManager {
        if let asr { return asr }
        let started = Date()
        try ModelLayout.require(ModelLayout.parakeetFiles, in: layout.parakeet)
        let models = try AsrModels.loadLocal(
            from: layout.parakeet, version: .v2, configuration: compute.configuration())
        let manager = AsrManager(config: .default, models: models)
        logLine("loaded Parakeet TDT v2 (\(compute.rawValue)) in \(Int(millis(since: started))) ms")
        asr = manager
        return manager
    }

    func loadVad() throws -> VadManager {
        if let vad { return vad }
        let started = Date()
        try ModelLayout.require(ModelLayout.vadFiles, in: layout.vad)
        let model = try MLModel(
            contentsOf: layout.vad.appendingPathComponent(ModelLayout.vadFiles[0]),
            configuration: compute.configuration())
        let manager = VadManager(
            config: VadConfig(defaultThreshold: Self.vadThreshold, computeUnits: compute.units), vadModel: model)
        logLine("loaded Silero VAD (\(compute.rawValue)) in \(Int(millis(since: started))) ms")
        vad = manager
        return manager
    }

    /// The same split of compute units as FluidAudio's own loader: FBank on
    /// the CPU (fastest there), the rest on the chosen units.
    func loadDiarizer() throws -> OfflineDiarizerModels {
        if let diarizerModels { return diarizerModels }
        let started = Date()
        let directory = layout.diarizer
        try ModelLayout.require(ModelLayout.diarizerFiles, in: directory)
        func model(_ name: String, _ units: MLComputeUnits) throws -> MLModel {
            let configuration = compute.configuration()
            configuration.computeUnits = units
            return try MLModel(contentsOf: directory.appendingPathComponent(name), configuration: configuration)
        }
        let psi = try PldaParameters.psi(
            from: Data(contentsOf: directory.appendingPathComponent("plda-parameters.json")))
        let models = OfflineDiarizerModels(
            segmentationModel: try model("Segmentation.mlmodelc", compute.units),
            fbankModel: try model("FBank.mlmodelc", .cpuOnly),
            embeddingModel: try model("Embedding.mlmodelc", compute.units),
            pldaRhoModel: try model("PldaRho.mlmodelc", compute.units),
            pldaPsi: psi,
            compilationDuration: Date().timeIntervalSince(started))
        logLine("loaded the offline diarizer (\(compute.rawValue)) in \(Int(millis(since: started))) ms")
        diarizerModels = models
        return models
    }

    /// For `prepare --role batch`: load (and so compile and cache) the
    /// transcription models, and the diarizer when it is downloaded.
    public func loadAll() throws {
        _ = try loadAsr()
        _ = try loadVad()
        if ModelLayout.hasAll(ModelLayout.diarizerFiles, in: layout.diarizer) {
            _ = try loadDiarizer()
        }
    }

    /// One second of silence through ASR and VAD.
    public func warmUp() async throws {
        _ = try await transcribe([Float](repeating: 0, count: 16_000))
        _ = try await loadVad().process([Float](repeating: 0, count: VadManager.chunkSize))
    }

    // MARK: - Requests

    public func transcribe(_ samples: [Float]) async throws -> Transcription {
        if samples.isEmpty { return Transcription(text: "", words: [], confidence: nil) }
        let asr = try loadAsr()
        // FluidAudio rejects audio under 0.3 s; trailing silence does not
        // change what is said.
        var audio = samples
        let minimum = ASRConstants.minimumRequiredSamples(forSampleRate: 16_000)
        if audio.count < minimum {
            audio.append(contentsOf: [Float](repeating: 0, count: minimum - audio.count))
        }
        // Every request is its own utterance: a fresh decoder state.
        var state = TdtDecoderState.make(decoderLayers: AsrModelVersion.v2.decoderLayers)
        let result = try await asr.transcribe(audio, decoderState: &state)
        let words = buildWordTimings(from: result.tokenTimings ?? []).map {
            Word(text: $0.word, start: $0.startTime, end: $0.endTime)
        }
        let confidence = Double(result.confidence)
        return Transcription(
            text: result.text.trimmingCharacters(in: .whitespacesAndNewlines),
            words: words,
            confidence: confidence.isFinite ? confidence : nil)
    }

    public func vadSegments(path: String) async throws -> [Span] {
        let vad = try loadVad()
        var results: [VadResult] = []
        var total = 0
        try await readMono16k(try audioFile(path), block: Self.vadBlockSamples) { block in
            total += block.count
            results.append(contentsOf: try await vad.process(block))
        }
        let segments = await vad.segmentSpeech(from: results, totalSamples: total, config: Self.segmentation)
        return segments.map { Span(start: $0.startTime, end: $0.endTime) }
    }

    public func diarize(
        path: String, maxSpeakers: Int?, progress: @escaping @Sendable (Double) -> Void
    ) async throws -> [LabeledSpan] {
        let models = try loadDiarizer()
        var config = OfflineDiarizerConfig.default
        if let maxSpeakers { config = config.withSpeakers(max: maxSpeakers) }
        let manager = OfflineDiarizerManager(config: config)
        manager.initialize(models: models)
        let result: DiarizationResult
        do {
            result = try await manager.process(try audioFile(path)) { done, total in
                progress(Double(done) / Double(max(total, 1)))
            }
        } catch OfflineDiarizationError.noSpeechDetected {
            // A silent recording has no speakers; that is an answer, not a failure.
            progress(1)
            return []
        }
        return result.segments.map {
            LabeledSpan(label: $0.speakerId, start: Double($0.startTimeSeconds), end: Double($0.endTimeSeconds))
        }
    }
}

/// The file a request names, or `missing <path>` (AVAudioFile's own error
/// for a missing file is an opaque OSStatus).
func audioFile(_ path: String) throws -> URL {
    guard FileManager.default.fileExists(atPath: path) else { throw HelperError.missing(path) }
    return URL(fileURLWithPath: path)
}

/// Read an audio file as 16 kHz mono f32 in blocks of `block` samples. The
/// app writes 16 kHz mono WAVs, read as they are; anything else goes through
/// FluidAudio's converter whole.
func readMono16k(_ url: URL, block: Int, _ body: ([Float]) async throws -> Void) async throws {
    let file = try AVAudioFile(forReading: url)
    let format = file.processingFormat
    guard format.sampleRate == 16_000, format.channelCount == 1, format.commonFormat == .pcmFormatFloat32 else {
        let samples = try AudioConverter().resampleAudioFile(url)
        var start = 0
        while start < samples.count {
            let end = min(start + block, samples.count)
            try await body(Array(samples[start..<end]))
            start = end
        }
        return
    }
    guard let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: AVAudioFrameCount(block)) else {
        throw HelperError.badRequest("could not allocate an audio buffer")
    }
    while file.framePosition < file.length {
        try file.read(into: buffer, frameCount: AVAudioFrameCount(block))
        let count = Int(buffer.frameLength)
        guard count > 0, let channel = buffer.floatChannelData?[0] else { break }
        try await body(Array(UnsafeBufferPointer(start: channel, count: count)))
    }
}
