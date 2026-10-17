//! Times the post-meeting final pass on a real recording, the way the app
//! runs it (VAD, 25 s splits, the app's Whisper engine and parameters), so
//! Metal-only and Core ML encoder runs can be compared.
//!
//! ```text
//! cargo run --release -p meetily --features dev-replay --example final_pass_bench -- \
//!     <models_dir> <model_name> "/path/to/Meeting …/audio.mp4" <out.txt> [quiet]
//! ```
//!
//! A Core ML run is a `models_dir` holding the ggml model *and*
//! `ggml-<model>-encoder.mlmodelc`; a Metal run is one without the encoder.
//! The transcript goes to `out.txt` for comparison and stays local: never
//! commit or upload it. Prints one JSON line of timings.

use std::path::PathBuf;
use std::time::Instant;

use app_lib::audio::decoder::decode_audio_file;
use app_lib::audio::vad::get_speech_chunks_with_progress;
use app_lib::whisper_engine::WhisperEngine;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 {
        anyhow::bail!("usage: final_pass_bench <models_dir> <model> <audio> <out.txt> [quiet]");
    }
    let models_dir = PathBuf::from(&args[1]);
    let model = &args[2];
    let audio = PathBuf::from(&args[3]);
    let out = PathBuf::from(&args[4]);
    let quiet = args.get(5).is_some_and(|a| a == "quiet");
    let pace_ms: u64 = std::env::var("BENCH_PACE_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    let started = Instant::now();
    let samples = decode_audio_file(&audio)?.to_whisper_format();
    let audio_seconds = samples.len() as f64 / 16_000.0;
    let segments = get_speech_chunks_with_progress(&samples, 2000, |_, _| true)?;
    let mut pieces = Vec::new();
    for segment in &segments {
        if segment.samples.len() > 25 * 16_000 {
            pieces.extend(app_lib::audio::split_segment_at_silence(
                segment,
                25 * 16_000,
            ));
        } else {
            pieces.push(segment.clone());
        }
    }
    let prep_s = started.elapsed().as_secs_f64();

    let engine = WhisperEngine::new_with_models_dir(Some(models_dir))?;
    engine.discover_models().await?;
    let load_started = Instant::now();
    engine.load_model(model).await?;
    let load_s = load_started.elapsed().as_secs_f64();
    let warm_started = Instant::now();
    engine.warm_up().await?;
    let warm_s = warm_started.elapsed().as_secs_f64();

    eprintln!("BENCH_START {}", chrono::Utc::now().timestamp_millis());
    let transcribe_started = Instant::now();
    let mut text = String::new();
    for piece in &pieces {
        if piece.samples.len() < 1600 {
            continue;
        }
        let (line, _, _) = if quiet {
            engine
                .transcribe_audio_with_confidence_quietly(piece.samples.clone(), Some("en".into()))
                .await?
        } else {
            engine
                .transcribe_audio_with_confidence(piece.samples.clone(), Some("en".into()))
                .await?
        };
        if !line.trim().is_empty() {
            text.push_str(line.trim());
            text.push('\n');
        }
        if pace_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(pace_ms)).await;
        }
    }
    let transcribe_s = transcribe_started.elapsed().as_secs_f64();
    eprintln!("BENCH_END {}", chrono::Utc::now().timestamp_millis());

    // Optional: diarization of the same recording (BENCH_DIAR_MODEL is the
    // Sortformer .onnx), quiet or not like the transcription.
    let mut diar_s = None;
    if let Ok(model_path) = std::env::var("BENCH_DIAR_MODEL") {
        let started = Instant::now();
        eprintln!("BENCH_DIAR_START {}", chrono::Utc::now().timestamp_millis());
        let run = move || -> anyhow::Result<usize> {
            let mut diarizer = app_lib::audio::sortformer::SortformerDiarizer::new(
                std::path::Path::new(&model_path),
            )?;
            Ok(diarizer.diarize(&samples, |_| {})?.len())
        };
        let turns = if quiet {
            app_lib::quiet_work::spawn_quietly(run).await??
        } else {
            tokio::task::spawn_blocking(run).await??
        };
        eprintln!(
            "BENCH_DIAR_END {} turns={turns}",
            chrono::Utc::now().timestamp_millis()
        );
        diar_s = Some(started.elapsed().as_secs_f64());
    }
    std::fs::write(&out, &text)?;
    println!(
        "{}",
        serde_json::json!({
            "model": model,
            "quiet": quiet,
            "pace_ms": pace_ms,
            "audio_s": audio_seconds,
            "segments": pieces.len(),
            "prep_s": prep_s,
            "load_s": load_s,
            "warm_s": warm_s,
            "transcribe_s": transcribe_s,
            "rtf": transcribe_s / audio_seconds,
            "diar_s": diar_s,
        })
    );
    Ok(())
}
