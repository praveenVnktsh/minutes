import Foundation
import XCTest

@testable import NemotronCore

final class ProtocolTests: XCTestCase {
    func testRequestsRoundTripThroughTheHeader() {
        let request = Request(op: .audio, stream: 0x0102_0304, payload: Data([9, 8, 7]))
        let bytes = request.encoded()
        XCTAssertEqual([UInt8](bytes), [2, 4, 3, 2, 1, 3, 0, 0, 0, 9, 8, 7])
        let header = Request.header(bytes)
        XCTAssertEqual(header.op, .audio)
        XCTAssertEqual(header.stream, 0x0102_0304)
        XCTAssertEqual(header.length, 3)
        XCTAssertNil(Request.header(Data([99, 0, 0, 0, 0, 0, 0, 0, 0])).op)
    }

    func testRequestsOnAStreamThatIsNotOpenAreReportedOnTheNextDecode() throws {
        let server = Server { _ in throw HelperError.missing("model") }
        XCTAssertNil(server.handle(Request(op: .audio, stream: 7, payload: Data(count: 8))))
        let reply = try XCTUnwrap(server.handle(Request(op: .decode, stream: 0)))
        let json = try JSONSerialization.jsonObject(with: Data(reply.utf8)) as? [String: Any]
        XCTAssertTrue((json?["error"] as? String)?.contains("stream 7") ?? false, reply)
        // The error is reported once; a decode with nothing open is empty.
        let next = try XCTUnwrap(server.handle(Request(op: .decode, stream: 0)))
        let streams = try JSONSerialization.jsonObject(with: Data(next.utf8)) as? [String: Any]
        XCTAssertEqual((streams?["streams"] as? [Any])?.count, 0)
    }

    func testAFailedOpenIsReported() throws {
        let server = Server { _ in throw HelperError.missing("model") }
        var rate = UInt32(48_000).littleEndian
        _ = server.handle(Request(op: .open, stream: 1, payload: Data(bytes: &rate, count: 4)))
        XCTAssertEqual(server.openStreams, 0)
        let reply = try XCTUnwrap(server.handle(Request(op: .decode, stream: 0)))
        XCTAssertTrue(reply.contains("error"), reply)
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

final class VocabularyTests: XCTestCase {
    func testWordMarkersBecomeSpacesAndSpecialTokensAreDropped() {
        let vocabulary = Vocabulary(pieces: ["<unk>", "▁the", "re", ","])
        XCTAssertEqual(vocabulary.text(0), "")
        XCTAssertEqual(vocabulary.text(1), " the")
        XCTAssertEqual(vocabulary.text(2), "re")
        XCTAssertEqual(vocabulary.text(3), ",")
        XCTAssertEqual(vocabulary.text(1024), "")
    }
}
