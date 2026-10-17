import Foundation

/// The stdin/stdout protocol between the app and the helper.
///
/// Requests (stdin) are binary frames, little-endian:
/// `op: u8, stream: u32, length: u32, payload: [u8; length]`.
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
/// Replies (stdout) are JSON lines. On start the helper prints
/// `{"ready":true,...}` once the models are loaded (or `{"error":...}` and
/// exits). A decode decodes every ready stream and replies with the
/// hypothesis of every open stream:
/// `{"streams":[{"id":1,"start":0.0,"tokens":[" Hello"],"times":[0.48]}],"ms":3.1}`.
/// A failure in a request without a reply is reported on the next decode as
/// `{"error":"..."}`.
public enum Op: UInt8 {
    case open = 1
    case audio = 2
    case finish = 3
    case reset = 4
    case close = 5
    case decode = 6
    case shutdown = 7
}

public struct Request {
    public let op: Op
    public let stream: UInt32
    public let payload: Data

    public init(op: Op, stream: UInt32, payload: Data = Data()) {
        self.op = op
        self.stream = stream
        self.payload = payload
    }

    public static let headerSize = 9

    /// Parse a header; `nil` for an unknown op.
    public static func header(_ bytes: Data) -> (op: Op?, stream: UInt32, length: Int) {
        let b = [UInt8](bytes.prefix(headerSize))
        let stream = UInt32(b[1]) | UInt32(b[2]) << 8 | UInt32(b[3]) << 16 | UInt32(b[4]) << 24
        let length = Int(UInt32(b[5]) | UInt32(b[6]) << 8 | UInt32(b[7]) << 16 | UInt32(b[8]) << 24)
        return (Op(rawValue: b[0]), stream, length)
    }

    public func encoded() -> Data {
        var data = Data([op.rawValue])
        withUnsafeBytes(of: stream.littleEndian) { data.append(contentsOf: $0) }
        withUnsafeBytes(of: UInt32(payload.count).littleEndian) { data.append(contentsOf: $0) }
        data.append(payload)
        return data
    }
}

/// One stream of a loaded model, as the protocol drives it.
public protocol RecognizerSession: AnyObject {
    func accept(_ samples: [Float])
    func inputFinished()
    func decodeReady() throws
    func reset()
    var startTime: Double { get }
    var tokenIds: [Int] { get }
    func hypothesis() -> (tokens: [String], times: [Double])
}

/// Holds the open streams and answers requests.
public final class Server {
    let makeSession: (Int) throws -> RecognizerSession
    private var sessions: [UInt32: RecognizerSession] = [:]
    private var pendingError: String?

    public init(makeSession: @escaping (Int) throws -> RecognizerSession) {
        self.makeSession = makeSession
    }

    public var openStreams: Int { sessions.count }

    /// Handle one request. Returns the reply line for requests that have one.
    public func handle(_ request: Request) -> String? {
        do {
            switch request.op {
            case .open:
                var rate: UInt32 = 16_000
                if request.payload.count >= 4 {
                    rate = request.payload.withUnsafeBytes { $0.loadUnaligned(as: UInt32.self) }
                }
                sessions[request.stream] = try makeSession(Int(UInt32(littleEndian: rate)))
            case .audio:
                let samples: [Float] = request.payload.withUnsafeBytes { raw in
                    Array(raw.bindMemory(to: Float.self))
                }
                try session(request.stream).accept(samples)
            case .finish:
                try session(request.stream).inputFinished()
            case .reset:
                try session(request.stream).reset()
            case .close:
                sessions[request.stream] = nil
            case .decode:
                return decode()
            case .shutdown:
                sessions.removeAll()
            }
        } catch {
            pendingError = pendingError ?? "\(error)"
            if request.op == .decode { return errorLine() }
        }
        return nil
    }

    private func session(_ id: UInt32) throws -> RecognizerSession {
        guard let session = sessions[id] else { throw HelperError.missing("stream \(id)") }
        return session
    }

    private func errorLine() -> String {
        let message = pendingError ?? "unknown error"
        pendingError = nil
        return Self.json(["error": message])
    }

    private func decode() -> String {
        if pendingError != nil { return errorLine() }
        let started = Date()
        var streams: [[String: Any]] = []
        do {
            for id in sessions.keys.sorted() {
                let session = sessions[id]!
                try session.decodeReady()
                let (tokens, times) = session.hypothesis()
                streams.append([
                    "id": id,
                    "start": (session.startTime * 1000).rounded() / 1000,
                    "tokens": tokens,
                    "times": times.map { ($0 * 1000).rounded() / 1000 },
                ])
            }
        } catch {
            pendingError = "\(error)"
            return errorLine()
        }
        let ms = Date().timeIntervalSince(started) * 1000
        return Self.json(["streams": streams, "ms": (ms * 10).rounded() / 10])
    }

    public static func json(_ object: [String: Any]) -> String {
        guard let data = try? JSONSerialization.data(withJSONObject: object, options: []),
              let line = String(data: data, encoding: .utf8)
        else { return "{\"error\":\"could not encode the reply\"}" }
        return line
    }
}
