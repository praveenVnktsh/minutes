// nemotron-helper: Nemotron Speech Streaming 0.6B on Core ML for Minutes.
//
//   nemotron-helper --model-dir DIR [--compute gpu|ane|cpu|all]
//       Serve the stdin/stdout protocol (see NemotronCore/Protocol.swift).
//   nemotron-helper --model-dir DIR --compute gpu --prepare
//       Load (and so compile and cache) the models for that compute choice,
//       print one JSON line with the load time, and exit.
//   nemotron-helper --model-dir DIR --compute gpu --transcribe FILE.f32 [--rate 16000]
//       Benchmark: stream a raw little-endian f32 mono file through one
//       session in 160 ms steps and print the hypothesis and timings.

import Foundation
import NemotronCore


func emit(_ line: String) {
    FileHandle.standardOutput.write((line + "\n").data(using: .utf8)!)
}

func fail(_ message: String) -> Never {
    emit(Server.json(["error": message]))
    exit(1)
}

var arguments = [String: String]()
var flags = Set<String>()
do {
    var iterator = CommandLine.arguments.dropFirst().makeIterator()
    while let arg = iterator.next() {
        switch arg {
        case "--prepare", "--small-on-device":
            flags.insert(arg)
        default:
            guard arg.hasPrefix("--"), let value = iterator.next() else { fail("bad argument \(arg)") }
            arguments[String(arg.dropFirst(2))] = value
        }
    }
}

guard let dir = arguments["model-dir"] else { fail("--model-dir is required") }
guard let compute = ComputeChoice(rawValue: arguments["compute"] ?? "gpu") else {
    fail("--compute must be one of gpu, ane, cpu, all")
}

let loadStarted = Date()
let makeSession: (Int) throws -> RecognizerSession
let warmUp: () throws -> Void
do {
    let directory = URL(fileURLWithPath: dir)
    let smallOnCPU = !flags.contains("--small-on-device")
    let model = try NemotronModel(directory: directory, compute: compute, smallOnCPU: smallOnCPU)
    makeSession = { rate in try StreamSession(model: model, inputRate: rate) }
    warmUp = model.warmUp
} catch {
    fail("could not load the Core ML models: \(error)")
}
let loadMs = Date().timeIntervalSince(loadStarted) * 1000
let warmStarted = Date()
do { try warmUp() } catch { fail("warm-up failed: \(error)") }
let warmMs = Date().timeIntervalSince(warmStarted) * 1000

if flags.contains("--prepare") {
    emit(Server.json(["prepared": true, "compute": compute.rawValue,
                      "load_ms": loadMs.rounded(), "warm_ms": warmMs.rounded()]))
    exit(0)
}

if let file = arguments["transcribe"] {
    let rate = Int(arguments["rate"] ?? "16000") ?? 16_000
    guard let data = FileManager.default.contents(atPath: file) else { fail("cannot read \(file)") }
    let samples: [Float] = data.withUnsafeBytes { Array($0.bindMemory(to: Float.self)) }
    do {
        let session = try makeSession(rate)
        let step = rate * 160 / 1000
        var offset = 0
        var chunkTimes: [Double] = []
        var firstTokenAudio: Double? = nil
        let started = Date()
        while offset < samples.count {
            let end = min(offset + step, samples.count)
            session.accept(Array(samples[offset..<end]))
            let t = Date()
            try session.decodeReady()
            chunkTimes.append(Date().timeIntervalSince(t) * 1000)
            if firstTokenAudio == nil, !session.tokenIds.isEmpty {
                firstTokenAudio = Double(end) / Double(rate)
            }
            offset = end
        }
        session.accept([Float](repeating: 0, count: rate * 8 / 10))
        session.inputFinished()
        try session.decodeReady()
        let total = Date().timeIntervalSince(started)
        let (tokens, times) = session.hypothesis()
        let audioSeconds = Double(samples.count) / Double(rate)
        let sorted = chunkTimes.sorted()
        emit(Server.json([
            "compute": compute.rawValue,
            "load_ms": loadMs.rounded(), "warm_ms": warmMs.rounded(),
            "audio_s": audioSeconds, "decode_s": total, "rtf": total / audioSeconds,
            "chunk_ms_p50": sorted.isEmpty ? 0 : sorted[sorted.count / 2],
            "chunk_ms_p95": sorted.isEmpty ? 0 : sorted[sorted.count * 95 / 100],
            "first_token_audio_s": firstTokenAudio ?? -1,
            "encoder_s": Timing.encoderSeconds, "encoder_calls": Timing.encoderCalls,
            "decoder_s": Timing.decoderSeconds, "decoder_calls": Timing.decoderCalls,
            "text": tokens.joined().trimmingCharacters(in: .whitespaces),
            "times": times,
        ]))
    } catch {
        fail("transcribe failed: \(error)")
    }
    exit(0)
}

emit(Server.json(["ready": true, "compute": compute.rawValue,
                  "load_ms": loadMs.rounded(), "warm_ms": warmMs.rounded()]))

let server = Server(makeSession: makeSession)
let input = FileHandle.standardInput

func readExactly(_ count: Int) -> Data? {
    var data = Data()
    while data.count < count {
        let chunk = input.readData(ofLength: count - data.count)
        if chunk.isEmpty { return nil }
        data.append(chunk)
    }
    return data
}

while let header = readExactly(Request.headerSize) {
    let (op, stream, length) = Request.header(header)
    guard length <= 64 << 20, let payload = length == 0 ? Data() : readExactly(length) else { break }
    guard let op else { continue }
    if op == .shutdown { break }
    if let reply = server.handle(Request(op: op, stream: stream, payload: payload)) {
        emit(reply)
    }
}
exit(0)
