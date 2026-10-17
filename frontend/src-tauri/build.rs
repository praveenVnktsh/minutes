#[path = "build/ffmpeg.rs"]
mod ffmpeg;

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        panic!("Minutes builds for macOS (Apple Silicon) only");
    }

    #[cfg(feature = "coreml")]
    println!("cargo:warning=CoreML acceleration ENABLED");

    println!("cargo:rustc-link-lib=framework=AVFoundation");
    println!("cargo:rustc-link-lib=framework=Cocoa");
    println!("cargo:rustc-link-lib=framework=Foundation");

    // Download and bundle FFmpeg binary at build-time to eliminate runtime download delays
    ffmpeg::ensure_ffmpeg_binary();

    tauri_build::build()
}
