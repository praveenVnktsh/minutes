// The 560 ms Core ML export of Nemotron Speech Streaming 0.6B by FluidInference
// (`FluidInference/nemotron-speech-streaming-en-0.6b-coreml`,
// `nemotron_coreml_560ms`): the same chunking as the sherpa-onnx export the
// app runs on the CPU (65 mel frames in = 9 pre-encode cache + 56 new, 7
// encoder frames out). Model I/O follows FluidAudio's
// StreamingNemotronAsrManager (https://github.com/FluidInference/FluidAudio,
// Apache-2.0).

import CoreML
import Foundation

public struct ModelConfig: Sendable {
    public let melFeatures: Int
    public let chunkMelFrames: Int
    public let preEncodeCache: Int
    public let totalMelFrames: Int
    public let blankIdx: Int
    public let vocabSize: Int
    public let decoderHidden: Int
    public let decoderLayers: Int
    public let cacheChannelShape: [Int]
    public let cacheTimeShape: [Int]
    public let hop = 160
    public let nFFT = 512
    public let winLength = 400
    public let sampleRate = 16_000
    public let preEmphasis: Float = 0.97
    /// Seconds per encoder frame (8x subsampling of 10 ms mel frames).
    public let frameSeconds = 0.08

    public init(metadata url: URL) throws {
        let data = try Data(contentsOf: url)
        guard let json = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            throw HelperError.badOutput("metadata.json")
        }
        melFeatures = json["mel_features"] as? Int ?? 128
        chunkMelFrames = json["chunk_mel_frames"] as? Int ?? 56
        preEncodeCache = json["pre_encode_cache"] as? Int ?? 9
        totalMelFrames = json["total_mel_frames"] as? Int ?? 65
        blankIdx = json["blank_idx"] as? Int ?? 1024
        vocabSize = json["vocab_size"] as? Int ?? 1024
        decoderHidden = json["decoder_hidden"] as? Int ?? 640
        decoderLayers = json["decoder_layers"] as? Int ?? 2
        cacheChannelShape = json["cache_channel_shape"] as? [Int] ?? [1, 24, 70, 1024]
        cacheTimeShape = json["cache_time_shape"] as? [Int] ?? [1, 24, 1024, 8]
    }
}

public final class NemotronModel {
    public let config: ModelConfig
    public let vocabulary: Vocabulary
    let encoder: MLModel
    let decoder: MLModel
    let joint: MLModel
    public let compute: ComputeChoice
    let mel: MelExtractor

    public init(directory: URL, compute: ComputeChoice, smallOnCPU: Bool = true) throws {
        self.compute = compute
        config = try ModelConfig(metadata: directory.appendingPathComponent("metadata.json"))
        vocabulary = try Vocabulary.load(from: directory.appendingPathComponent("tokenizer.json"))
        func load(_ path: String, _ units: MLComputeUnits) throws -> MLModel {
            let url = directory.appendingPathComponent(path, isDirectory: true)
            guard FileManager.default.fileExists(atPath: url.path) else {
                throw HelperError.missing(url.path)
            }
            let configuration = MLModelConfiguration()
            configuration.computeUnits = units
            // The export computes in fp32; fp16 accumulation is what makes
            // the GPU worth using (the Neural Engine is fp16 anyway).
            configuration.allowLowPrecisionAccumulationOnGPU =
                ProcessInfo.processInfo.environment["NEMOTRON_GPU_FP32"] == nil
            return try MLModel(contentsOf: url, configuration: configuration)
        }
        encoder = try load("encoder/encoder_int8.mlmodelc", compute.units)
        let small: MLComputeUnits = smallOnCPU ? .cpuOnly : compute.units
        decoder = try load("decoder.mlmodelc", small)
        joint = try load("joint.mlmodelc", small)
        mel = MelExtractor(
            numMelBins: config.melFeatures, sampleRate: config.sampleRate, hop: config.hop,
            winLength: config.winLength, preEmphasis: config.preEmphasis, periodicWindow: false)
    }

    /// One throwaway chunk, so the first real one does not pay for the
    /// device's first-prediction setup.
    public func warmUp() throws {
        let session = try StreamSession(model: self, inputRate: config.sampleRate)
        session.accept([Float](repeating: 0, count: config.chunkMelFrames * config.hop * 2))
        try session.decodeReady()
    }
}

/// Time spent in each model since the process started (for benchmarks).
public enum Timing {
    nonisolated(unsafe) public static var encoderSeconds = 0.0
    nonisolated(unsafe) public static var decoderSeconds = 0.0
    nonisolated(unsafe) public static var encoderCalls = 0
    nonisolated(unsafe) public static var decoderCalls = 0
    nonisolated(unsafe) public static var jointSeconds = 0.0
    nonisolated(unsafe) public static var jointCalls = 0
}

/// One audio stream: continuous log-mel, the cache-aware encoder, and greedy
/// RNN-T (prediction network on each emitted token, joint network per frame). Its API mirrors
/// sherpa-onnx's OnlineStream, so the app's segmentation runs unchanged on
/// either engine.
public final class StreamSession: RecognizerSession {
    let model: NemotronModel
    let config: ModelConfig
    let resampler: Resampler

    /// Pre-emphasized 16 kHz signal from absolute sample `base` on.
    private var emphasized: [Float] = []
    private var base = 0
    private var samplesIn = 0
    private var lastSample: Float = 0
    private var finished = false
    /// First new mel frame of the next chunk.
    private var nextFrame = 0

    private var cacheChannel: MLMultiArray
    private var cacheTime: MLMultiArray
    private var cacheLen: MLMultiArray
    private let melLength: MLMultiArray
    private var h: MLMultiArray
    private var c: MLMultiArray
    private var lastToken: Int32
    /// Prediction network output for `lastToken`, and the state after it.
    private var decoderOut: MLMultiArray?
    private var nextH: MLMultiArray?
    private var nextC: MLMultiArray?
    private let tokenLength: MLMultiArray

    public private(set) var tokenIds: [Int] = []
    private var tokenFrames: [Int] = []
    private var framesDecoded = 0
    private var startFrame = 0

    public init(model: NemotronModel, inputRate: Int) throws {
        self.model = model
        config = model.config
        resampler = Resampler(inputRate: inputRate)
        cacheChannel = try .zeros(config.cacheChannelShape, .float32)
        cacheTime = try .zeros(config.cacheTimeShape, .float32)
        cacheLen = try .zeros([1], .int32)
        melLength = try .zeros([1], .int32)
        melLength[0] = NSNumber(value: Int32(config.totalMelFrames))
        h = try .zeros([config.decoderLayers, 1, config.decoderHidden], .float32)
        c = try .zeros([config.decoderLayers, 1, config.decoderHidden], .float32)
        tokenLength = try .zeros([1], .int32)
        tokenLength[0] = 1
        lastToken = Int32(config.blankIdx)
    }

    public func accept(_ samples: [Float]) {
        let audio = resampler.process(samples)
        emphasized.reserveCapacity(emphasized.count + audio.count)
        for x in audio {
            // NeMo keeps the very first sample as is.
            emphasized.append(samplesIn == 0 && emphasized.isEmpty ? x : x - config.preEmphasis * lastSample)
            lastSample = x
            samplesIn += 1
        }
    }

    public func inputFinished() {
        finished = true
    }

    public var isReady: Bool {
        let last = nextFrame + config.chunkMelFrames - 1
        if samplesIn >= last * config.hop + config.nFFT / 2 { return true }
        return finished && nextFrame * config.hop < samplesIn
    }

    public func decodeReady() throws {
        while isReady {
            try processChunk()
            nextFrame += config.chunkMelFrames
            let keepFrom = max(0, (nextFrame - config.preEncodeCache) * config.hop - config.nFFT / 2)
            let drop = min(emphasized.count, keepFrom - base)
            if drop > 0 {
                emphasized.removeFirst(drop)
                base += drop
            }
        }
    }

    public func reset() {
        tokenIds.removeAll()
        tokenFrames.removeAll()
        startFrame = framesDecoded
    }

    public var startTime: Double { Double(startFrame) * config.frameSeconds }

    public func hypothesis() -> (tokens: [String], times: [Double]) {
        var tokens: [String] = []
        var times: [Double] = []
        for (id, frame) in zip(tokenIds, tokenFrames) {
            let text = model.vocabulary.text(id)
            if text.isEmpty { continue }
            tokens.append(text)
            times.append(Double(frame - startFrame) * config.frameSeconds)
        }
        return (tokens, times)
    }

    private func processChunk() throws {
        // Mel frames [next - 9, next + 56): the pre-encode cache (zeros before
        // the stream's start) and the new chunk, from the continuous signal.
        let first = nextFrame - config.preEncodeCache
        let total = config.totalMelFrames
        let computedFrom = max(0, first)
        let features = model.mel.frames(
            emphasized, base: base, first: computedFrom, count: total - (computedFrom - first))
        let bins = config.melFeatures
        let input = try MLMultiArray.zeros([1, bins, total], .float32)
        let dst = input.dataPointer.assumingMemoryBound(to: Float.self)
        let offset = computedFrom - first
        let computed = total - offset
        features.withUnsafeBufferPointer { src in
            for b in 0..<bins {
                for f in 0..<computed { dst[b * total + offset + f] = src[b * computed + f] }
            }
        }

        let encoderStarted = DispatchTime.now().uptimeNanoseconds
        let out = try model.encoder.prediction(from: MLDictionaryFeatureProvider(dictionary: [
            "mel": MLFeatureValue(multiArray: input),
            "mel_length": MLFeatureValue(multiArray: melLength),
            "cache_channel": MLFeatureValue(multiArray: cacheChannel),
            "cache_time": MLFeatureValue(multiArray: cacheTime),
            "cache_len": MLFeatureValue(multiArray: cacheLen),
        ]))
        Timing.encoderSeconds += Double(DispatchTime.now().uptimeNanoseconds - encoderStarted) / 1e9
        Timing.encoderCalls += 1
        func array(_ name: String) throws -> MLMultiArray {
            guard let value = out.featureValue(for: name)?.multiArrayValue else {
                throw HelperError.badOutput(name)
            }
            return value
        }
        let encoded = try array("encoded")  // [1, 1024, T]
        cacheChannel = try array("cache_channel_out")
        cacheTime = try array("cache_time_out")
        cacheLen = try array("cache_len_out")
        let length = min(try array("encoded_length")[0].intValue, encoded.shape[2].intValue)

        let dim = encoded.shape[1].intValue
        let channelStride = encoded.strides[1].intValue
        let timeStride = encoded.strides[2].intValue
        let step = try MLMultiArray.zeros([1, dim, 1], .float32)
        let stepPtr = step.dataPointer.assumingMemoryBound(to: Float.self)
        for t in 0..<length {
            for i in 0..<dim { stepPtr[i] = encoded.float(at: i * channelStride + t * timeStride) }
            try decodeFrame(step, frame: framesDecoded + t)
        }
        framesDecoded += length
    }

    /// Run the prediction network on `lastToken` from state (h, c). Its
    /// output only changes when a token is emitted, so it is cached between
    /// frames and the joint network alone runs per frame.
    private func predict() throws {
        let token = try MLMultiArray.zeros([1, 1], .int32)
        token[0] = NSNumber(value: lastToken)
        let started = DispatchTime.now().uptimeNanoseconds
        let out = try model.decoder.prediction(from: MLDictionaryFeatureProvider(dictionary: [
            "token": MLFeatureValue(multiArray: token),
            "token_length": MLFeatureValue(multiArray: tokenLength),
            "h_in": MLFeatureValue(multiArray: h),
            "c_in": MLFeatureValue(multiArray: c),
        ]))
        Timing.decoderSeconds += Double(DispatchTime.now().uptimeNanoseconds - started) / 1e9
        Timing.decoderCalls += 1
        guard let output = out.featureValue(for: "decoder_out")?.multiArrayValue,
              let hOut = out.featureValue(for: "h_out")?.multiArrayValue,
              let cOut = out.featureValue(for: "c_out")?.multiArrayValue
        else { throw HelperError.badOutput("decoder") }
        decoderOut = output
        nextH = hOut
        nextC = cOut
    }

    private func decodeFrame(_ step: MLMultiArray, frame: Int) throws {
        for _ in 0..<10 {
            if decoderOut == nil { try predict() }
            let started = DispatchTime.now().uptimeNanoseconds
            let out = try model.joint.prediction(from: MLDictionaryFeatureProvider(dictionary: [
                "encoder": MLFeatureValue(multiArray: step),
                "decoder": MLFeatureValue(multiArray: decoderOut!),
            ]))
            Timing.jointSeconds += Double(DispatchTime.now().uptimeNanoseconds - started) / 1e9
            Timing.jointCalls += 1
            guard let logits = out.featureValue(for: "logits")?.multiArrayValue else {
                throw HelperError.badOutput("joint")
            }
            let classes = min(config.vocabSize + 1, logits.count)
            let stride = logits.strides.last?.intValue ?? 1
            var best = 0
            var bestValue = -Float.infinity
            for i in 0..<classes {
                let v = logits.float(at: i * stride)
                if v > bestValue { bestValue = v; best = i }
            }
            if best == config.blankIdx { return }
            tokenIds.append(best)
            tokenFrames.append(frame)
            // The emitted token advances the prediction network.
            lastToken = Int32(best)
            h = nextH!
            c = nextC!
            decoderOut = nil
        }
    }
}
