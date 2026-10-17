// swift-tools-version: 5.10
// nemotron-helper: runs the Nemotron Speech Streaming 0.6B live model on Core ML
// (GPU or Neural Engine) for the Minutes app, behind a small stdin/stdout
// protocol. See README.md in this folder.
import PackageDescription

let package = Package(
    name: "nemotron-helper",
    platforms: [.macOS("15.0")],
    targets: [
        .target(name: "NemotronCore"),
        .executableTarget(name: "nemotron-helper", dependencies: ["NemotronCore"]),
        .testTarget(name: "NemotronCoreTests", dependencies: ["NemotronCore"]),
    ]
)
