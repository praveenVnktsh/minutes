//! Live engine benchmark on a real recording, through the app's own
//! two-source transcriber (silence gate, segmentation, the engine trait).
//! Ignored by default; run one engine per process so usage can be sampled:
//!
//! ```text
//! LIVE_BENCH_AUDIO=meeting-48k.f32 LIVE_BENCH_ENGINE=gpu|ane|cpu \
//! NEMOTRON_STREAMING_MODEL_DIR=... NEMOTRON_COREML_MODEL_DIR=... \
//! [LIVE_BENCH_SECONDS=600] [LIVE_BENCH_PACED=1] [LIVE_BENCH_OUT=out.json] \
//! cargo test -p meetily --lib live_streaming::bench -- --ignored --nocapture
//! ```
//!
//! The audio is raw 48 kHz mono f32 (the pipeline's rate). It is fed as the
//! system source and, 3 s later, as the microphone, so both streams carry
//! speech (a worst case for load). `LIVE_BENCH_PACED=1` feeds it in real time,
//! like a meeting, and measures token latency; otherwise it runs flat out and
//! measures the real-time factor. Finals are written to `LIVE_BENCH_OUT` for
//! WER scoring; only aggregates are printed.

use std::sync::Arc;
use std::time::{Duration, Instant};

use super::engine::{self, StreamingEngine};
use super::native::Compute;
use super::transcriber::{LiveEvent, Source, StreamingTranscriber};

fn percentile(values: &mut [f64], p: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap());
    values[((values.len() - 1) as f64 * p).round() as usize]
}

fn load_engine(kind: &str) -> StreamingEngine {
    let dir = |key: &str| std::path::PathBuf::from(std::env::var(key).expect(key));
    match kind {
        "cpu" => engine::create(&dir("NEMOTRON_STREAMING_MODEL_DIR")).expect("sherpa loads"),
        "gpu" => engine::create_native(&dir("NEMOTRON_COREML_MODEL_DIR"), Compute::Gpu)
            .expect("helper loads"),
        "ane" => engine::create_native(&dir("NEMOTRON_COREML_MODEL_DIR"), Compute::NeuralEngine)
            .expect("helper loads"),
        other => panic!("unknown engine {other}"),
    }
}

#[test]
#[ignore]
fn live_engine_on_a_recording() {
    let (Ok(audio_path), Ok(kind)) = (
        std::env::var("LIVE_BENCH_AUDIO"),
        std::env::var("LIVE_BENCH_ENGINE"),
    ) else {
        return;
    };
    let paced = std::env::var("LIVE_BENCH_PACED").is_ok_and(|v| v == "1");
    let bytes = std::fs::read(&audio_path).expect("audio");
    let mut system: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
        .collect();
    let sr = 48_000usize;
    if let Some(seconds) = std::env::var("LIVE_BENCH_SECONDS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
    {
        system.truncate(seconds * sr);
    }
    let mut mic = vec![0.0; 3 * sr];
    mic.extend_from_slice(&system);
    mic.truncate(system.len());
    let audio_seconds = system.len() as f64 / sr as f64;

    let load_started = Instant::now();
    let engine = Arc::new(load_engine(&kind));
    engine.recognizer.wait_ready().expect("engine loads");
    let load_seconds = load_started.elapsed().as_secs_f64();
    eprintln!("BENCH_READY pid={} engine={kind}", std::process::id());

    let mut transcriber = StreamingTranscriber::new(engine.clone(), sr as u32);
    let mut events = Vec::new();
    let window = sr / 20; // the pipeline's 50 ms windows
    let mut decode_ms = Vec::new();
    let mut lags = Vec::new();
    let mut last_token = f64::NEG_INFINITY;
    let started = Instant::now();
    for (i, (sys, mic)) in system.chunks(window).zip(mic.chunks(window)).enumerate() {
        let start = (i * window) as f64 / sr as f64;
        if paced {
            let due = Duration::from_secs_f64(start);
            if let Some(wait) = due.checked_sub(started.elapsed()) {
                std::thread::sleep(wait);
            }
        }
        transcriber.push(Source::System, sys, start, &mut events);
        transcriber.push(Source::Microphone, mic, start, &mut events);
        let t = Instant::now();
        transcriber.decode(&mut events);
        decode_ms.push(t.elapsed().as_secs_f64() * 1000.0);
        if paced {
            if let Some(token) = transcriber.last_token_time(Source::System) {
                if token > last_token {
                    // Audio fed so far is the wall clock in a paced run.
                    let now = start + sys.len() as f64 / sr as f64;
                    lags.push(now - token);
                    last_token = token;
                }
            }
        }
        assert!(transcriber.failure().is_none(), "{:?}", transcriber.failure());
    }
    transcriber.finish(&mut events);
    let wall = started.elapsed().as_secs_f64();

    let finals: Vec<serde_json::Value> = events
        .iter()
        .filter_map(|event| match event {
            LiveEvent::Final {
                source,
                text,
                start,
                end,
            } => Some(serde_json::json!({
                "source": source.label(), "text": text, "start": start, "end": end
            })),
            _ => None,
        })
        .collect();
    let partials = events
        .iter()
        .filter(|e| matches!(e, LiveEvent::Partial { text, .. } if !text.is_empty()))
        .count();
    let summary = serde_json::json!({
        "engine": kind,
        "paced": paced,
        "audio_s": audio_seconds,
        "load_s": load_seconds,
        "wall_s": wall,
        "rtf": wall / audio_seconds,
        "decode_ms_p50": percentile(&mut decode_ms.clone(), 0.5),
        "decode_ms_p95": percentile(&mut decode_ms.clone(), 0.95),
        "decode_ms_max": percentile(&mut decode_ms, 1.0),
        "token_lag_s_p50": percentile(&mut lags.clone(), 0.5),
        "token_lag_s_p95": percentile(&mut lags.clone(), 0.95),
        "first_token_lag_s": lags.first().copied(),
        "finals": finals.len(),
        "partials": partials,
    });
    eprintln!("BENCH_RESULT {summary}");
    if let Ok(out) = std::env::var("LIVE_BENCH_OUT") {
        let mut full = summary;
        full["finals"] = serde_json::Value::Array(finals);
        std::fs::write(out, full.to_string()).unwrap();
    }
}
