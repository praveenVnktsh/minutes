// swift-tools-version: 5.10
// speech-helper: runs every speech model of the Minutes app (Nemotron
// streaming for live text, Parakeet TDT v2 for the final transcript, Silero
// VAD, pyannote diarization) on Core ML through FluidAudio, behind a small
// stdin/stdout protocol (see Sources/SpeechCore/Protocol.swift).
import PackageDescription

let package = Package(
    name: "speech-helper",
    platforms: [.macOS("15.0")],
    dependencies: [
        .package(url: "https://github.com/FluidInference/FluidAudio.git", exact: "0.18.0"),
    ],
    targets: [
        .target(
            name: "SpeechCore",
            dependencies: [.product(name: "FluidAudio", package: "FluidAudio")]),
        .executableTarget(name: "speech-helper", dependencies: ["SpeechCore"]),
        .testTarget(name: "SpeechCoreTests", dependencies: ["SpeechCore"]),
    ]
)
