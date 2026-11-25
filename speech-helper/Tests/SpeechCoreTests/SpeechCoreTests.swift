import Foundation
import XCTest

@testable import SpeechCore

private func object(_ line: String?) throws -> [String: Any] {
    let line = try XCTUnwrap(line)
    return try XCTUnwrap(JSONSerialization.jsonObject(with: Data(line.utf8)) as? [String: Any], line)
}

private func floats(_ samples: [Float]) -> Data {
    samples.withUnsafeBufferPointer { Data(buffer: $0) }
}

private func rate(_ value: UInt32) -> Data {
    withUnsafeBytes(of: value.littleEndian) { Data($0) }
}

/// A stream that reports one token per accepted call.
private final class FakeSession: LiveSession {
    var accepted: [[Float]] = []
    var finished = false
    var closed = false
    var startTime = 0.0
    private var decoded = 0
    private var tokens: [String] = []
    private var times: [Double] = []

    func accept(_ samples: [Float]) { accepted.append(samples) }
    func inputFinished() { finished = true }
    func decodeReady() async throws {
        while decoded < accepted.count {
            tokens.append(" w\(decoded)")
            decoded += 1
            times.append(Double(decoded) * 0.08)
        }
    }
    func reset() {
        startTime = times.last ?? 0
        tokens.removeAll()
        times = []
    }
    func hypothesis() -> (tokens: [String], times: [Double]) { (tokens, times.map { $0 - startTime }) }
    func close() async { closed = true }
}

private final class FakeBatch: BatchEngine {
    var transcription = Transcription(text: "", words: [], confidence: nil)
    var spans: [Span] = []
    var speakers: [LabeledSpan] = []
    var progressSteps: [Double] = []
    var error: Error?
    var lastSamples: [Float] = []
    var lastPath = ""
    var lastMaxSpeakers: Int?

    func transcribe(_ samples: [Float]) async throws -> Transcription {
        if let error { throw error }
        lastSamples = samples
        return transcription
    }
    func vadSegments(path: String) async throws -> [Span] {
        if let error { throw error }
        lastPath = path
        return spans
    }
    func diarize(path: String, maxSpeakers: Int?, progress: @escaping @Sendable (Double) -> Void)
        async throws -> [LabeledSpan]
    {
        if let error { throw error }
        lastPath = path
        lastMaxSpeakers = maxSpeakers
        progressSteps.forEach(progress)
        return speakers
    }
}

/// Lines a request emitted before its reply.
private final class Lines: @unchecked Sendable {
    private let lock = NSLock()
    private var lines: [String] = []
    func append(_ line: String) {
        lock.lock()
        lines.append(line)
        lock.unlock()
    }
    var all: [String] {
        lock.lock()
        defer { lock.unlock() }
        return lines
    }
}

final class ProtocolTests: XCTestCase {
    func testRequestsRoundTripThroughTheHeader() {
        let request = Request(op: .audio, id: 0x0102_0304, payload: Data([9, 8, 7]))
        let bytes = request.encoded()
        XCTAssertEqual([UInt8](bytes), [2, 4, 3, 2, 1, 3, 0, 0, 0, 9, 8, 7])
        let header = Request.header(bytes)
        XCTAssertEqual(header.op, 2)
        XCTAssertEqual(header.id, 0x0102_0304)
        XCTAssertEqual(header.length, 3)
        XCTAssertEqual(Request.header(Request(op: .diarize, id: 1).encoded()).op, 10)
        XCTAssertNil(Op(rawValue: Request.header(Data([99, 0, 0, 0, 0, 0, 0, 0, 0])).op))
    }

    func testAudioPayloadsAreLittleEndianFloats() {
        let request = Request(op: .transcribe, id: 1, payload: floats([0.5, -1, 0.25]) + Data([1, 2]))
        XCTAssertEqual(request.samples, [0.5, -1, 0.25])
        XCTAssertEqual(Request(op: .transcribe, id: 1).samples, [])
    }

    func testFileRequestsParse() throws {
        let plain = try FileRequest.parse(Data(#"{"path":"/tmp/a.wav"}"#.utf8))
        XCTAssertEqual(plain, FileRequest(path: "/tmp/a.wav", maxSpeakers: nil))
        let noHint = try FileRequest.parse(Data(#"{"path":"/tmp/a.wav","max_speakers":0}"#.utf8))
        XCTAssertNil(noHint.maxSpeakers)
        let hint = try FileRequest.parse(Data(#"{"path":"/tmp/a.wav","max_speakers":3}"#.utf8))
        XCTAssertEqual(hint.maxSpeakers, 3)
        XCTAssertThrowsError(try FileRequest.parse(Data("not json".utf8)))
        XCTAssertThrowsError(try FileRequest.parse(Data(#"{"max_speakers":2}"#.utf8)))
        XCTAssertThrowsError(try FileRequest.parse(Data(#"{"path":""}"#.utf8)))
    }

    func testSpeakersAreNumberedByFirstAppearance() {
        let numbered = SpeakerSpan.numbered([
            LabeledSpan(label: "S3", start: 5, end: 6),
            LabeledSpan(label: "S7", start: 1, end: 2),
            LabeledSpan(label: "S3", start: 2.5, end: 4),
            LabeledSpan(label: "S1", start: 7, end: 8),
            LabeledSpan(label: "S7", start: 9, end: 9.5),
        ])
        XCTAssertEqual(numbered.map(\.speaker), [0, 1, 1, 2, 0])
        XCTAssertEqual(numbered.map(\.start), [1, 2.5, 5, 7, 9])
    }

    func testProgressIsSentInWholePercentSteps() {
        let throttle = ProgressThrottle()
        XCTAssertEqual(throttle.step(0), 0)
        XCTAssertNil(throttle.step(0.004))
        XCTAssertEqual(throttle.step(0.42), 0.42)
        XCTAssertNil(throttle.step(0.425))
        XCTAssertNil(throttle.step(0.3))
        XCTAssertEqual(throttle.step(1.5), 1)
        XCTAssertNil(throttle.step(.nan))
    }
}

final class LiveServerTests: XCTestCase {
    func testRequestsOnAStreamThatIsNotOpenAreReportedOnTheNextDecode() async throws {
        let server = Server { _ in throw HelperError.missing("model") }
        let none = await server.handle(Request(op: .audio, id: 7, payload: Data(count: 8)))
        XCTAssertNil(none)
        let reply = try object(await server.handle(Request(op: .decode, id: 0)))
        XCTAssertTrue((reply["error"] as? String)?.contains("stream 7") ?? false, "\(reply)")
        // The error is reported once; a decode with nothing open is empty.
        let next = try object(await server.handle(Request(op: .decode, id: 0)))
        XCTAssertEqual((next["streams"] as? [Any])?.count, 0)
    }

    func testAFailedOpenIsReported() async throws {
        let server = Server { _ in throw HelperError.missing("model") }
        _ = await server.handle(Request(op: .open, id: 1, payload: rate(48_000)))
        XCTAssertEqual(server.openStreams, 0)
        let reply = try object(await server.handle(Request(op: .decode, id: 0)))
        XCTAssertNotNil(reply["error"])
    }

    func testAStreamIsOpenedFedDecodedResetAndClosed() async throws {
        var opened: [(Int, FakeSession)] = []
        let server = Server { rate in
            let session = FakeSession()
            opened.append((rate, session))
            return session
        }
        _ = await server.handle(Request(op: .open, id: 3, payload: rate(48_000)))
        _ = await server.handle(Request(op: .audio, id: 3, payload: floats([0.1, 0.2])))
        _ = await server.handle(Request(op: .audio, id: 3, payload: floats([0.3])))
        XCTAssertEqual(opened.count, 1)
        XCTAssertEqual(opened[0].0, 48_000)
        XCTAssertEqual(opened[0].1.accepted, [[0.1, 0.2], [0.3]])

        let reply = try object(await server.handle(Request(op: .decode, id: 0)))
        let streams = try XCTUnwrap(reply["streams"] as? [[String: Any]])
        XCTAssertEqual(streams.count, 1)
        XCTAssertEqual(streams[0]["id"] as? Int, 3)
        XCTAssertEqual(streams[0]["start"] as? Double, 0)
        XCTAssertEqual(streams[0]["tokens"] as? [String], [" w0", " w1"])
        XCTAssertEqual(streams[0]["times"] as? [Double], [0.08, 0.16])
        XCTAssertNotNil(reply["ms"] as? Double)

        _ = await server.handle(Request(op: .reset, id: 3))
        _ = await server.handle(Request(op: .finish, id: 3))
        XCTAssertTrue(opened[0].1.finished)
        let afterReset = try object(await server.handle(Request(op: .decode, id: 0)))
        let stream = try XCTUnwrap((afterReset["streams"] as? [[String: Any]])?.first)
        XCTAssertEqual(stream["start"] as? Double, 0.16)
        XCTAssertEqual(stream["tokens"] as? [String], [])

        _ = await server.handle(Request(op: .close, id: 3))
        XCTAssertTrue(opened[0].1.closed)
        XCTAssertEqual(server.openStreams, 0)
    }

    func testReopeningAStreamIdClosesTheOldStream() async throws {
        var opened: [FakeSession] = []
        let server = Server { _ in
            let session = FakeSession()
            opened.append(session)
            return session
        }
        _ = await server.handle(Request(op: .open, id: 1, payload: rate(16_000)))
        _ = await server.handle(Request(op: .open, id: 1, payload: rate(16_000)))
        XCTAssertEqual(opened.count, 2)
        XCTAssertTrue(opened[0].closed)
        XCTAssertFalse(opened[1].closed)
        XCTAssertEqual(server.openStreams, 1)
        await server.closeAll()
        XCTAssertTrue(opened[1].closed)
    }

    func testBatchRequestsToTheLiveHelperGetAnErrorReply() async throws {
        let server = Server { _ in FakeSession() }
        let reply = try object(await server.handle(Request(op: .transcribe, id: 9, payload: floats([0]))))
        XCTAssertEqual(reply["id"] as? Int, 9)
        XCTAssertNotNil(reply["error"])
    }

    func testAnUnknownOpIsReportedOnTheNextDecode() async throws {
        let server = Server { _ in FakeSession() }
        XCTAssertNil(server.handleUnknown(op: 42, id: 1))
        let reply = try object(await server.handle(Request(op: .decode, id: 0)))
        XCTAssertEqual(reply["error"] as? String, "unknown op 42")
    }
}

final class BatchServerTests: XCTestCase {
    func testTranscribeRepliesWithTextWordsAndConfidence() async throws {
        let engine = FakeBatch()
        engine.transcription = Transcription(
            text: "hello world",
            words: [Word(text: "hello", start: 0.12, end: 0.4), Word(text: "world", start: 0.48, end: 0.8)],
            confidence: 0.93123)
        let server = Server(batch: engine)
        let reply = try object(await server.handle(Request(op: .transcribe, id: 5, payload: floats([0.5, 0.25]))))
        XCTAssertEqual(engine.lastSamples, [0.5, 0.25])
        XCTAssertEqual(reply["id"] as? Int, 5)
        XCTAssertEqual(reply["text"] as? String, "hello world")
        XCTAssertEqual(reply["confidence"] as? Double, 0.931)
        let words = try XCTUnwrap(reply["words"] as? [[String: Any]])
        XCTAssertEqual(words.map { $0["text"] as? String }, ["hello", "world"])
        XCTAssertEqual(words.map { $0["start"] as? Double }, [0.12, 0.48])
        XCTAssertEqual(words.map { $0["end"] as? Double }, [0.4, 0.8])
        XCTAssertNotNil(reply["ms"] as? Double)
    }

    func testAnUnknownConfidenceIsNull() async throws {
        let server = Server(batch: FakeBatch())
        let reply0 = await server.handle(Request(op: .transcribe, id: 1))
        let line = try XCTUnwrap(reply0)
        XCTAssertTrue(line.contains("\"confidence\":null"), line)
        let reply = try object(line)
        XCTAssertEqual(reply["text"] as? String, "")
        XCTAssertEqual((reply["words"] as? [Any])?.count, 0)
    }

    func testVadRepliesWithSegments() async throws {
        let engine = FakeBatch()
        engine.spans = [Span(start: 0.52, end: 4.1), Span(start: 5.0004, end: 7.25)]
        let server = Server(batch: engine)
        let payload = Data(#"{"path":"/tmp/meeting.wav"}"#.utf8)
        let reply = try object(await server.handle(Request(op: .vad, id: 2, payload: payload)))
        XCTAssertEqual(engine.lastPath, "/tmp/meeting.wav")
        XCTAssertEqual(reply["id"] as? Int, 2)
        let segments = try XCTUnwrap(reply["segments"] as? [[String: Double]])
        XCTAssertEqual(segments, [["start": 0.52, "end": 4.1], ["start": 5.0, "end": 7.25]])
    }

    func testDiarizeSendsProgressThenNumberedSegments() async throws {
        let engine = FakeBatch()
        engine.progressSteps = [0, 0.001, 0.5, 0.5, 1]
        engine.speakers = [
            LabeledSpan(label: "B", start: 3, end: 4),
            LabeledSpan(label: "A", start: 1.2, end: 3.4),
        ]
        let server = Server(batch: engine)
        let lines = Lines()
        let payload = Data(#"{"path":"/tmp/m.wav","max_speakers":4}"#.utf8)
        let reply = try object(await server.handle(Request(op: .diarize, id: 11, payload: payload)) {
            lines.append($0)
        })
        XCTAssertEqual(engine.lastMaxSpeakers, 4)
        let progress = try lines.all.map { try object($0) }
        XCTAssertEqual(progress.map { $0["id"] as? Int }, [11, 11, 11])
        XCTAssertEqual(progress.map { $0["progress"] as? Double }, [0, 0.5, 1])
        XCTAssertEqual(reply["id"] as? Int, 11)
        let segments = try XCTUnwrap(reply["segments"] as? [[String: Any]])
        XCTAssertEqual(segments.map { $0["speaker"] as? Int }, [0, 1])
        XCTAssertEqual(segments.map { $0["start"] as? Double }, [1.2, 3])
        XCTAssertEqual(segments.map { $0["end"] as? Double }, [3.4, 4])
    }

    func testFailuresAreReportedWithTheRequestId() async throws {
        let engine = FakeBatch()
        let server = Server(batch: engine)
        let bad = try object(await server.handle(Request(op: .vad, id: 4, payload: Data("{}".utf8))))
        XCTAssertEqual(bad["id"] as? Int, 4)
        XCTAssertTrue((bad["error"] as? String)?.contains("path") ?? false, "\(bad)")

        engine.error = HelperError.missing("/models/parakeet-tdt-0.6b-v2-coreml/Encoder.mlmodelc")
        let failed = try object(await server.handle(Request(op: .transcribe, id: 6, payload: floats([0]))))
        XCTAssertEqual(failed["id"] as? Int, 6)
        XCTAssertEqual(failed["error"] as? String, "missing /models/parakeet-tdt-0.6b-v2-coreml/Encoder.mlmodelc")
    }

    func testLiveRequestsToTheBatchHelperFail() async throws {
        let server = Server(batch: FakeBatch())
        _ = await server.handle(Request(op: .open, id: 1, payload: rate(16_000)))
        XCTAssertEqual(server.openStreams, 0)
        let reply = try object(await server.handle(Request(op: .decode, id: 0)))
        XCTAssertNotNil(reply["error"])
        let unknown = try object(server.handleUnknown(op: 77, id: 8))
        XCTAssertEqual(unknown["id"] as? Int, 8)
        XCTAssertEqual(unknown["error"] as? String, "unknown op 77")
    }
}

final class ModelLayoutTests: XCTestCase {
    func testModelsLiveUnderTheirRepoNames() {
        let layout = ModelLayout(root: URL(fileURLWithPath: "/m", isDirectory: true))
        XCTAssertEqual(layout.nemotron.path, "/m/nemotron-speech-streaming-en-0.6b-coreml/nemotron_coreml_560ms")
        XCTAssertEqual(layout.parakeet.path, "/m/parakeet-tdt-0.6b-v2-coreml")
        XCTAssertEqual(layout.vad.path, "/m/silero-vad-coreml")
        XCTAssertEqual(layout.diarizer.path, "/m/speaker-diarization-coreml")
    }

    func testAMissingFileIsNamed() throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("speech-helper-tests-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(
            at: directory.appendingPathComponent("Encoder.mlmodelc"), withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        XCTAssertNoThrow(try ModelLayout.require(["Encoder.mlmodelc"], in: directory))
        XCTAssertThrowsError(try ModelLayout.require(["Encoder.mlmodelc", "Decoder.mlmodelc"], in: directory)) {
            XCTAssertTrue("\($0)".hasSuffix("Decoder.mlmodelc"), "\($0)")
        }
        XCTAssertFalse(ModelLayout.hasAll(ModelLayout.diarizerFiles, in: directory))
    }

    func testPldaPsiIsDecoded() throws {
        let psi = floats([1.5, -2, 0.25]).base64EncodedString()
        let json = Data(#"{"tensors":{"psi":{"dtype":"float32","data_base64":"\#(psi)"}}}"#.utf8)
        XCTAssertEqual(try PldaParameters.psi(from: json), [1.5, -2, 0.25])
        XCTAssertThrowsError(try PldaParameters.psi(from: Data(#"{"tensors":{}}"#.utf8)))
    }

    func testComputeChoicesMapToCoreMLUnits() {
        XCTAssertEqual(ComputeChoice.gpu.units, .cpuAndGPU)
        XCTAssertEqual(ComputeChoice.ane.units, .cpuAndNeuralEngine)
        XCTAssertEqual(ComputeChoice.cpu.units, .cpuOnly)
        XCTAssertEqual(ComputeChoice.all.units, .all)
        XCTAssertEqual(ComputeChoice.gpu.configuration().computeUnits, .cpuAndGPU)
    }
}

final class TokenTextTests: XCTestCase {
    func testWordMarkersBecomeSpacesAndSpecialTokensAreDropped() {
        XCTAssertEqual(TokenText.text("<unk>"), "")
        XCTAssertEqual(TokenText.text("\u{2581}the"), " the")
        XCTAssertEqual(TokenText.text("re"), "re")
        XCTAssertEqual(TokenText.text(","), ",")
        XCTAssertEqual(TokenText.text("<"), "<")
    }
}

final class ResamplerTests: XCTestCase {
    func test48kHzIsDecimatedToAThirdAndKeepsATone() {
        let resampler = Resampler(inputRate: 48_000)
        let tone = (0..<48_000).map { Float(sin(2 * Double.pi * 440 * Double($0) / 48_000)) }
        var out: [Float] = []
        // In 50 ms windows, like the capture pipeline.
        for start in stride(from: 0, to: tone.count, by: 2_400) {
            out += resampler.process(Array(tone[start..<min(start + 2_400, tone.count)]))
        }
        XCTAssertEqual(Double(out.count), 16_000, accuracy: 80)
        let middle = out[4_000..<12_000]
        let rms = sqrt(middle.map { $0 * $0 }.reduce(0, +) / Float(middle.count))
        XCTAssertEqual(rms, Float(1 / 2.0.squareRoot()), accuracy: 0.02)
    }

    func testAToneAboveTheNewNyquistIsRemoved() {
        let resampler = Resampler(inputRate: 48_000)
        let tone = (0..<48_000).map { Float(sin(2 * Double.pi * 12_000 * Double($0) / 48_000)) }
        let out = resampler.process(tone)
        let middle = out[4_000..<12_000]
        let rms = sqrt(middle.map { $0 * $0 }.reduce(0, +) / Float(middle.count))
        XCTAssertLessThan(rms, 0.02)
    }

    func test16kHzPassesThrough() {
        let resampler = Resampler(inputRate: 16_000)
        XCTAssertEqual(resampler.process([0.1, 0.2, 0.3]), [0.1, 0.2, 0.3])
    }
}
