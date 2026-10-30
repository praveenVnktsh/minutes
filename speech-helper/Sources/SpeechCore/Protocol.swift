import Foundation

/// The stdin/stdout protocol between the app and the helper.
///
///     speech-helper live    --models DIR --compute gpu|ane|cpu|all
///     speech-helper batch   --models DIR [--compute ...]
///     speech-helper prepare --role live|batch --models DIR [--compute ...]
///
/// Requests (stdin) are binary frames, little-endian:
/// `op: u8, id: u32, length: u32, payload: [u8; length]` (length <= 64 MiB).
/// Replies (stdout) are JSON lines. On start the helper prints
/// `{"ready":true,"role":...,"compute":...,"load_ms":...,"warm_ms":...}` once
/// it can take requests (or `{"error":...}` and exits 1).
///
/// Live role (`id` is a stream id chosen by the app):
///
/// | op | name     | payload            | reply |
/// |----|----------|--------------------|-------|
/// | 1  | open     | u32 sample rate    | none  |
/// | 2  | audio    | f32 samples        | none  |
/// | 3  | finish   | (input finished)   | none  |
/// | 4  | reset    | (clear hypothesis) | none  |
/// | 5  | close    |                    | none  |
/// | 6  | decode   |                    | one JSON line |
/// | 7  | shutdown |                    | none, exits |
///
/// A decode decodes every ready stream and replies with the hypothesis of
/// every open stream:
/// `{"streams":[{"id":1,"start":0.0,"tokens":[" Hello"],"times":[0.48]}],"ms":3.1}`
/// (`tokens[i]` was emitted `start + times[i]` seconds after the stream
/// opened). A failure in a request without a reply is reported on the next
/// decode as `{"error":"..."}`.
///
/// Batch role (`id` is a request id; one request in flight at a time):
///
/// | op | name       | payload                                      |
/// |----|------------|----------------------------------------------|
/// | 8  | transcribe | f32 samples, 16 kHz mono                     |
/// | 9  | vad        | JSON `{"path":"/abs/16k-mono.wav"}`          |
/// | 10 | diarize    | JSON `{"path":"...","max_speakers":0}`       |
/// | 7  | shutdown   | none, exits                                  |
///
/// Replies: `{"id":N,"text":...,"words":[{"text","start","end"}],"confidence":0.93,"ms":120}`,
/// `{"id":N,"segments":[{"start","end"}],"ms":...}`, and for diarize any
/// number of `{"id":N,"progress":0.42}` lines then
/// `{"id":N,"segments":[{"speaker":0,"start","end"}],"ms":...}`. Errors are
/// `{"id":N,"error":"..."}`.
public enum Op: UInt8 {
    case open = 1
    case audio = 2
    case finish = 3
    case reset = 4
    case close = 5
    case decode = 6
    case shutdown = 7
    case transcribe = 8
    case vad = 9
    case diarize = 10
}

public struct Request {
    public let op: Op
    public let id: UInt32
    public let payload: Data

    public init(op: Op, id: UInt32, payload: Data = Data()) {
        self.op = op
        self.id = id
        self.payload = payload
    }

    public static let headerSize = 9
    public static let maxPayload = 64 << 20

    /// Parse a header. `op` is the raw code; `Op(rawValue:)` is nil for an
    /// unknown one.
    public static func header(_ bytes: Data) -> (op: UInt8, id: UInt32, length: Int) {
        let b = [UInt8](bytes.prefix(headerSize))
        let id = UInt32(b[1]) | UInt32(b[2]) << 8 | UInt32(b[3]) << 16 | UInt32(b[4]) << 24
        let length = Int(UInt32(b[5]) | UInt32(b[6]) << 8 | UInt32(b[7]) << 16 | UInt32(b[8]) << 24)
        return (b[0], id, length)
    }

    public func encoded() -> Data {
        var data = Data([op.rawValue])
        withUnsafeBytes(of: id.littleEndian) { data.append(contentsOf: $0) }
        withUnsafeBytes(of: UInt32(payload.count).littleEndian) { data.append(contentsOf: $0) }
        data.append(payload)
        return data
    }

    /// The payload as f32 samples (little-endian, the host's order; a
    /// trailing partial sample is ignored).
    public var samples: [Float] {
        let count = payload.count / MemoryLayout<Float>.size
        var samples = [Float](repeating: 0, count: count)
        samples.withUnsafeMutableBytes { _ = payload.copyBytes(to: $0, count: count * MemoryLayout<Float>.size) }
        return samples
    }
}

/// The JSON payload of `vad` and `diarize`.
public struct FileRequest: Equatable {
    public let path: String
    /// `nil` when the app gives no hint (`max_speakers` absent or 0).
    public let maxSpeakers: Int?

    public static func parse(_ payload: Data) throws -> FileRequest {
        guard let object = try? JSONSerialization.jsonObject(with: payload) as? [String: Any] else {
            throw HelperError.badRequest("the payload is not a JSON object")
        }
        guard let path = object["path"] as? String, !path.isEmpty else {
            throw HelperError.badRequest("no \"path\"")
        }
        let max = (object["max_speakers"] as? NSNumber)?.intValue ?? 0
        return FileRequest(path: path, maxSpeakers: max > 0 ? max : nil)
    }
}

/// One live stream, as the protocol drives it.
public protocol LiveSession: AnyObject {
    /// Samples at the rate the stream was opened with.
    func accept(_ samples: [Float])
    /// No more audio; the next decode decodes the rest.
    func inputFinished()
    func decodeReady() async throws
    /// Clear the hypothesis; the stream carries on, and `startTime` moves to
    /// the audio decoded so far.
    func reset()
    /// Seconds after the stream opened where the hypothesis starts.
    var startTime: Double { get }
    /// Tokens since the last reset, and when each was emitted in seconds
    /// after `startTime`.
    func hypothesis() -> (tokens: [String], times: [Double])
    /// Release what the stream holds (its model goes back to the pool).
    func close() async
}

public struct Word: Equatable, Sendable {
    public let text: String
    public let start: Double
    public let end: Double

    public init(text: String, start: Double, end: Double) {
        self.text = text
        self.start = start
        self.end = end
    }
}

public struct Transcription: Equatable, Sendable {
    public let text: String
    public let words: [Word]
    public let confidence: Double?

    public init(text: String, words: [Word], confidence: Double?) {
        self.text = text
        self.words = words
        self.confidence = confidence
    }
}

/// A span of the file in seconds.
public struct Span: Equatable, Sendable {
    public let start: Double
    public let end: Double

    public init(start: Double, end: Double) {
        self.start = start
        self.end = end
    }
}

/// A diarizer segment with the engine's own speaker label.
public struct LabeledSpan: Equatable, Sendable {
    public let label: String
    public let start: Double
    public let end: Double

    public init(label: String, start: Double, end: Double) {
        self.label = label
        self.start = start
        self.end = end
    }
}

/// A diarizer segment as the app gets it.
public struct SpeakerSpan: Equatable, Sendable {
    public let speaker: Int
    public let start: Double
    public let end: Double

    public init(speaker: Int, start: Double, end: Double) {
        self.speaker = speaker
        self.start = start
        self.end = end
    }

    /// Segments in time order with speakers numbered 0, 1, ... in order of
    /// first appearance.
    public static func numbered(_ spans: [LabeledSpan]) -> [SpeakerSpan] {
        var numbers: [String: Int] = [:]
        return spans.sorted { ($0.start, $0.end) < ($1.start, $1.end) }.map { span in
            let speaker = numbers[span.label] ?? numbers.count
            numbers[span.label] = speaker
            return SpeakerSpan(speaker: speaker, start: span.start, end: span.end)
        }
    }
}

/// The models behind the batch role.
public protocol BatchEngine: AnyObject {
    func transcribe(_ samples: [Float]) async throws -> Transcription
    func vadSegments(path: String) async throws -> [Span]
    /// `progress` gets the fraction done, 0...1.
    func diarize(path: String, maxSpeakers: Int?, progress: @escaping @Sendable (Double) -> Void)
        async throws -> [LabeledSpan]
}

/// Holds the open streams (live) or the engines (batch) and answers requests.
public final class Server {
    /// Writes one line to stdout; called from any thread.
    public typealias Emit = @Sendable (String) -> Void

    private let makeSession: ((Int) async throws -> LiveSession)?
    private let batch: BatchEngine?
    private var sessions: [UInt32: LiveSession] = [:]
    private var pendingError: String?

    /// The live role.
    public init(makeSession: @escaping (Int) async throws -> LiveSession) {
        self.makeSession = makeSession
        self.batch = nil
    }

    /// The batch role.
    public init(batch: BatchEngine) {
        self.makeSession = nil
        self.batch = batch
    }

    public var openStreams: Int { sessions.count }

    /// Handle one request. Returns the reply line for requests that have
    /// one; `emit` carries lines sent before it (diarize progress).
    public func handle(_ request: Request, emit: @escaping Emit = { _ in }) async -> String? {
        switch request.op {
        case .transcribe, .vad, .diarize:
            return await handleBatch(request, emit: emit)
        default:
            return await handleLive(request)
        }
    }

    /// A request whose op is not in the protocol. The batch role answers it
    /// (the app waits for a reply); the live role reports it on the next
    /// decode, where the app expects its only replies.
    public func handleUnknown(op: UInt8, id: UInt32) -> String? {
        let message = "unknown op \(op)"
        if batch != nil { return Self.json(["id": id, "error": message]) }
        pendingError = pendingError ?? message
        return nil
    }

    /// Close every stream (shutdown).
    public func closeAll() async {
        let open = sessions.values
        sessions.removeAll()
        for session in open { await session.close() }
    }

    // MARK: - Live

    private func handleLive(_ request: Request) async -> String? {
        do {
            switch request.op {
            case .open:
                guard let makeSession else {
                    throw HelperError.wrongRole("the batch helper has no live streams")
                }
                var rate: UInt32 = 16_000
                if request.payload.count >= 4 {
                    rate = UInt32(littleEndian: request.payload.withUnsafeBytes { $0.loadUnaligned(as: UInt32.self) })
                }
                if let previous = sessions.removeValue(forKey: request.id) { await previous.close() }
                sessions[request.id] = try await makeSession(Int(rate))
            case .audio:
                try session(request.id).accept(request.samples)
            case .finish:
                try session(request.id).inputFinished()
            case .reset:
                try session(request.id).reset()
            case .close:
                await sessions.removeValue(forKey: request.id)?.close()
            case .decode:
                guard makeSession != nil else {
                    throw HelperError.wrongRole("the batch helper has no live streams")
                }
                return await decode()
            case .shutdown:
                await closeAll()
            case .transcribe, .vad, .diarize:
                break
            }
        } catch {
            pendingError = pendingError ?? "\(error)"
            if request.op == .decode { return errorLine() }
        }
        return nil
    }

    private func session(_ id: UInt32) throws -> LiveSession {
        guard let session = sessions[id] else { throw HelperError.missing("stream \(id)") }
        return session
    }

    private func errorLine() -> String {
        let message = pendingError ?? "unknown error"
        pendingError = nil
        return Self.json(["error": message])
    }

    private func decode() async -> String {
        if pendingError != nil { return errorLine() }
        let started = Date()
        var streams: [[String: Any]] = []
        do {
            for id in sessions.keys.sorted() {
                guard let session = sessions[id] else { continue }
                try await session.decodeReady()
                let (tokens, times) = session.hypothesis()
                streams.append([
                    "id": id,
                    "start": Self.rounded(session.startTime),
                    "tokens": tokens,
                    "times": times.map(Self.rounded),
                ])
            }
        } catch {
            pendingError = "\(error)"
            return errorLine()
        }
        let ms = Date().timeIntervalSince(started) * 1000
        return Self.json(["streams": streams, "ms": (ms * 10).rounded() / 10])
    }

    // MARK: - Batch

    private func handleBatch(_ request: Request, emit: @escaping Emit) async -> String? {
        let id = request.id
        let started = Date()
        do {
            guard let batch else {
                throw HelperError.wrongRole("the live helper does not run batch requests")
            }
            switch request.op {
            case .transcribe:
                let result = try await batch.transcribe(request.samples)
                return Self.json([
                    "id": id,
                    "text": result.text,
                    "words": result.words.map {
                        ["text": $0.text, "start": Self.rounded($0.start), "end": Self.rounded($0.end)]
                    },
                    "confidence": result.confidence.map { Self.rounded($0) as Any } ?? NSNull(),
                    "ms": millis(since: started),
                ])
            case .vad:
                let file = try FileRequest.parse(request.payload)
                let spans = try await batch.vadSegments(path: file.path)
                return Self.json([
                    "id": id,
                    "segments": spans.map { ["start": Self.rounded($0.start), "end": Self.rounded($0.end)] },
                    "ms": millis(since: started),
                ])
            case .diarize:
                let file = try FileRequest.parse(request.payload)
                let throttle = ProgressThrottle()
                let spans = try await batch.diarize(path: file.path, maxSpeakers: file.maxSpeakers) { fraction in
                    if let step = throttle.step(fraction) {
                        emit(Self.json(["id": id, "progress": step]))
                    }
                }
                return Self.json([
                    "id": id,
                    "segments": SpeakerSpan.numbered(spans).map {
                        ["speaker": $0.speaker, "start": Self.rounded($0.start), "end": Self.rounded($0.end)]
                    },
                    "ms": millis(since: started),
                ])
            default:
                return nil
            }
        } catch {
            return Self.json(["id": id, "error": "\(error)"])
        }
    }

    // MARK: - JSON

    /// Seconds to the millisecond, so replies stay short.
    static func rounded(_ value: Double) -> Double {
        value.isFinite ? (value * 1000).rounded() / 1000 : 0
    }

    public static func json(_ object: [String: Any]) -> String {
        guard let data = try? JSONSerialization.data(withJSONObject: object, options: [.withoutEscapingSlashes]),
              let line = String(data: data, encoding: .utf8)
        else { return "{\"error\":\"could not encode the reply\"}" }
        return line
    }
}

/// Progress in whole-percent steps, so a long file sends at most ~100 lines.
final class ProgressThrottle: @unchecked Sendable {
    private let lock = NSLock()
    private var last = -1

    /// The fraction rounded to 0.01 if it moved on since the last step.
    func step(_ fraction: Double) -> Double? {
        guard fraction.isFinite else { return nil }
        let percent = Int((min(max(fraction, 0), 1) * 100).rounded(.down))
        lock.lock()
        defer { lock.unlock() }
        guard percent > last else { return nil }
        last = percent
        return Double(percent) / 100
    }
}
