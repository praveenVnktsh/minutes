#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

fn main() {
    // First, so a panic anywhere after this (including inside the macOS launch
    // callback, which aborts) leaves a report in the app's log directory.
    app_lib::app_logging::init();

    log::info!("Starting application v{}...", env!("CARGO_PKG_VERSION"));
    app_lib::run();
}
