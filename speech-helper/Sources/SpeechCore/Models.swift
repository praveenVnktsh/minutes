import CoreML
import FluidAudio
import Foundation

/// Where the models run. Named after what the user picks in Settings.
public enum ComputeChoice: String, CaseIterable, Sendable {
    /// Core ML on the GPU (and CPU for what the GPU cannot run).
    case gpu
    /// Core ML on the Neural Engine (and CPU).
    case ane
    /// Core ML on the CPU only.
    case cpu
    /// Core ML decides (all units).
    case all

    public var units: MLComputeUnits {
        switch self {
        case .gpu: return .cpuAndGPU
        case .ane: return .cpuAndNeuralEngine
        case .cpu: return .cpuOnly
        case .all: return .all
        }
    }

    /// A fresh Core ML configuration for this choice. fp16 accumulation is
    /// what makes the GPU worth using (the Neural Engine is fp16 anyway).
    public func configuration() -> MLModelConfiguration {
        let configuration = MLModelConfiguration()
        configuration.computeUnits = units
        configuration.allowLowPrecisionAccumulationOnGPU = true
        return configuration
    }
}

public enum HelperError: Error, CustomStringConvertible {
    case missing(String)
    case badRequest(String)
    case wrongRole(String)

    public var description: String {
        switch self {
        case .missing(let what): return "missing \(what)"
        case .badRequest(let what): return "bad request: \(what)"
        case .wrongRole(let what): return what
        }
    }
}

/// The app's FluidAudio models directory. The app downloads every model
/// (pinned Hugging Face revisions) into `<root>/<repo name>/`, keeping the
/// repo's relative paths, and the helper loads them from there with Core ML
/// directly: it never downloads (`ModelHub.offlineMode`) and never goes
/// through FluidAudio's own cache layout or revision markers.
public struct ModelLayout: Sendable {
    public let root: URL

    public init(root: URL) {
        self.root = root
    }

    public static let nemotronRepo = "nemotron-speech-streaming-en-0.6b-coreml"
    public static let parakeetRepo = "parakeet-tdt-0.6b-v2-coreml"
    public static let vadRepo = "silero-vad-coreml"
    public static let diarizerRepo = "speaker-diarization-coreml"

    /// Live ASR: the 560 ms variant's folder inside the Nemotron repo.
    public var nemotron: URL {
        root.appendingPathComponent(Self.nemotronRepo, isDirectory: true)
            .appendingPathComponent(NemotronChunkSize.ms560.subdirectory, isDirectory: true)
    }
    /// What `StreamingNemotronAsrManager.loadModels(from:)` opens.
    /// `metadata.json` is required here: without it FluidAudio assumes the
    /// 2240 ms chunking. `decoder_joint.mlmodelc` is optional (fused fast path).
    public static let nemotronFiles = [
        "encoder/encoder_int8.mlmodelc", "decoder.mlmodelc", "joint.mlmodelc",
        "tokenizer.json", "metadata.json",
    ]

    /// Final-pass ASR: Parakeet TDT 0.6B v2 (English).
    public var parakeet: URL { root.appendingPathComponent(Self.parakeetRepo, isDirectory: true) }
    /// What `AsrModels.loadLocal(from:version: .v2)` opens.
    public static let parakeetFiles = [
        "Preprocessor.mlmodelc", "Encoder.mlmodelc", "Decoder.mlmodelc", "JointDecision.mlmodelc",
        "parakeet_vocab.json",
    ]

    /// Silero VAD (FluidAudio's unified 256 ms export).
    public var vad: URL { root.appendingPathComponent(Self.vadRepo, isDirectory: true) }
    public static let vadFiles = ["silero-vad-unified-256ms-v6.2.1.mlmodelc"]

    /// Offline diarizer: pyannote community-1 segmentation, WeSpeaker
    /// embeddings, PLDA.
    public var diarizer: URL { root.appendingPathComponent(Self.diarizerRepo, isDirectory: true) }
    public static let diarizerFiles = [
        "Segmentation.mlmodelc", "FBank.mlmodelc", "Embedding.mlmodelc", "PldaRho.mlmodelc",
        "plda-parameters.json",
    ]

    /// Throws `missing <path>` for the first of `files` absent from `directory`.
    public static func require(_ files: [String], in directory: URL) throws {
        for file in files {
            let url = directory.appendingPathComponent(file)
            guard FileManager.default.fileExists(atPath: url.path) else {
                throw HelperError.missing(url.path)
            }
        }
    }

    public static func hasAll(_ files: [String], in directory: URL) -> Bool {
        (try? require(files, in: directory)) != nil
    }
}

/// The PLDA whitening vector shipped next to the diarizer models
/// (`plda-parameters.json`: `tensors.psi.data_base64`, little-endian f32).
/// FluidAudio reads it in its own loader, which also goes through its cache
/// layout; this is the same parse for loading from the app's directory.
public enum PldaParameters {
    public static func psi(from data: Data) throws -> [Double] {
        guard
            let root = try JSONSerialization.jsonObject(with: data) as? [String: Any],
            let tensors = root["tensors"] as? [String: Any],
            let psi = tensors["psi"] as? [String: Any],
            let base64 = psi["data_base64"] as? String,
            let decoded = Data(base64Encoded: base64, options: [.ignoreUnknownCharacters]),
            decoded.count >= MemoryLayout<Float>.size
        else { throw HelperError.missing("PLDA psi in plda-parameters.json") }
        var floats = [Float](repeating: 0, count: decoded.count / MemoryLayout<Float>.size)
        _ = floats.withUnsafeMutableBytes { decoded.copyBytes(to: $0) }
        return floats.map { Double(Float(bitPattern: $0.bitPattern.littleEndian)) }
    }
}

/// Lines on stderr, which the app logs.
public func logLine(_ message: String) {
    FileHandle.standardError.write(Data((message + "\n").utf8))
}

/// Milliseconds since `start`, rounded.
func millis(since start: Date) -> Double {
    (Date().timeIntervalSince(start) * 1000).rounded()
}
