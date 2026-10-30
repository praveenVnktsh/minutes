// speech-helper: the Minutes app's speech models on Core ML, through FluidAudio.
//
//   speech-helper live --models DIR [--compute gpu|ane|cpu|all]
//       Nemotron streaming (live text). Loads and warms the models, prints
//       the ready line, then serves the live protocol.
//   speech-helper batch --models DIR [--compute ...]
//       Parakeet TDT v2, Silero VAD and the offline diarizer. Prints the
//       ready line at once; each model loads on its first request.
//   speech-helper prepare --role live|batch --models DIR [--compute ...]
//       Load (and so compile and cache) a role's models for that compute
//       choice, print one JSON line with the timings, and exit.
//
// DIR is the app's FluidAudio models directory (see SpeechCore/Models.swift);
// the helper never downloads. The protocol is in SpeechCore/Protocol.swift.

import Darwin
import FluidAudio
import Foundation
import SpeechCore

// Replies go to the original stdout; anything else written to fd 1 (Core ML,
// Espresso and FluidAudio print there) is sent to stderr so it cannot
// corrupt the protocol.
let replyFd = dup(STDOUT_FILENO)
dup2(STDERR_FILENO, STDOUT_FILENO)

/// Lines from the request loop and from FluidAudio's progress callbacks.
final class ReplyWriter: @unchecked Sendable {
    private let lock = NSLock()

    func write(_ line: String) {
        let bytes = Array((line + "\n").utf8)
        lock.lock()
        defer { lock.unlock() }
        var written = 0
        while written < bytes.count {
            let n = bytes[written...].withUnsafeBytes { Darwin.write(replyFd, $0.baseAddress, $0.count) }
            if n < 0, errno == EINTR { continue }
            if n <= 0 { exit(0) }  // the app is gone
            written += n
        }
    }
}

let writer = ReplyWriter()

func emit(_ object: [String: Any]) {
    writer.write(Server.json(object))
}

func fail(_ message: String) -> Never {
    emit(["error": message])
    exit(1)
}

enum Role: String {
    case live
    case batch
}

// MARK: - Arguments

var arguments = CommandLine.arguments.dropFirst()
guard let command = arguments.popFirst(), ["live", "batch", "prepare"].contains(command) else {
    fail("usage: speech-helper live|batch|prepare --models DIR [--compute gpu|ane|cpu|all] [--role live|batch]")
}
var options = [String: String]()
while let arg = arguments.popFirst() {
    guard arg.hasPrefix("--"), let value = arguments.popFirst() else { fail("bad argument \(arg)") }
    options[String(arg.dropFirst(2))] = value
}

let roleName = command == "prepare" ? options["role"] ?? "" : command
guard let role = Role(rawValue: roleName) else { fail("--role must be live or batch") }
guard let modelsPath = options["models"] else { fail("--models is required") }
// Live defaults to the Neural Engine (the app's Auto); batch lets Core ML
// choose per model.
guard let compute = ComputeChoice(rawValue: options["compute"] ?? (role == .live ? "ane" : "all")) else {
    fail("--compute must be one of gpu, ane, cpu, all")
}
var isDirectory: ObjCBool = false
guard FileManager.default.fileExists(atPath: modelsPath, isDirectory: &isDirectory), isDirectory.boolValue else {
    fail("missing the models directory \(modelsPath)")
}

// Every model comes from DIR; nothing may fall back to a download.
ModelHub.offlineMode = true
let layout = ModelLayout(root: URL(fileURLWithPath: modelsPath, isDirectory: true))

// MARK: - Load

let server: Server
var loadMs = 0.0
var warmMs = 0.0
switch role {
case .live:
    let pool: NemotronPool
    do {
        pool = try NemotronPool(directory: layout.nemotron, compute: compute)
    } catch {
        fail("\(error)")
    }
    // `prepare` compiles and caches the models; serving loads a model per
    // stream the app keeps open at once (microphone and system audio).
    let loadStarted = Date()
    do {
        try await pool.preload(command == "prepare" ? 1 : 2)
    } catch {
        fail("could not load the Core ML models: \(error)")
    }
    loadMs = (Date().timeIntervalSince(loadStarted) * 1000).rounded()
    let warmStarted = Date()
    do { try await pool.warmUp() } catch { fail("warm-up failed: \(error)") }
    warmMs = (Date().timeIntervalSince(warmStarted) * 1000).rounded()
    server = Server { rate in try await NemotronSession.open(pool: pool, inputRate: rate) }
case .batch:
    let engine = FluidBatchEngine(layout: layout, compute: compute)
    if command == "prepare" {
        let loadStarted = Date()
        do { try engine.loadAll() } catch { fail("could not load the Core ML models: \(error)") }
        loadMs = (Date().timeIntervalSince(loadStarted) * 1000).rounded()
        let warmStarted = Date()
        do { try await engine.warmUp() } catch { fail("warm-up failed: \(error)") }
        warmMs = (Date().timeIntervalSince(warmStarted) * 1000).rounded()
    }
    server = Server(batch: engine)
}

if command == "prepare" {
    emit(["prepared": true, "role": role.rawValue, "compute": compute.rawValue,
          "load_ms": loadMs, "warm_ms": warmMs])
    exit(0)
}

emit(["ready": true, "role": role.rawValue, "compute": compute.rawValue,
      "load_ms": loadMs, "warm_ms": warmMs])

// MARK: - Requests

func readExactly(_ count: Int) -> Data? {
    var data = Data(count: count)
    var got = 0
    while got < count {
        let n = data.withUnsafeMutableBytes { read(STDIN_FILENO, $0.baseAddress! + got, count - got) }
        if n < 0, errno == EINTR { continue }
        if n <= 0 { return nil }
        got += n
    }
    return data
}

while let header = readExactly(Request.headerSize) {
    let (code, id, length) = Request.header(header)
    guard length <= Request.maxPayload, let payload = length == 0 ? Data() : readExactly(length) else { break }
    guard let op = Op(rawValue: code) else {
        if let reply = server.handleUnknown(op: code, id: id) { writer.write(reply) }
        continue
    }
    if op == .shutdown { break }
    if let reply = await server.handle(Request(op: op, id: id, payload: payload), emit: { writer.write($0) }) {
        writer.write(reply)
    }
}
await server.closeAll()
exit(0)
