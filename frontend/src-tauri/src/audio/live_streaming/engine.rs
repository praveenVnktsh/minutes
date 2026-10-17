//! The loaded Nemotron streaming recognizer, shared by recordings.
//!
//! Loading takes ~2 s and ~1 GB, so the recognizer is created once per
//! recording (or reused if still loaded) and unloaded when the recording
//! stops, which frees the memory before the final pass loads its engine.

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use log::info;
use sherpa_onnx::{OnlineRecognizer, OnlineRecognizerConfig};
use tauri::{AppHandle, Runtime};

use super::model;

/// Intra-op threads. Two streams are decoded as one batch, and more threads
/// than this contend with capture and the UI without speeding decode up.
const NUM_THREADS: i32 = 2;

pub struct StreamingEngine {
    pub recognizer: OnlineRecognizer,
}

static ENGINE: Mutex<Option<Arc<StreamingEngine>>> = Mutex::new(None);

fn path_string(dir: &Path, name: &str) -> Option<String> {
    Some(dir.join(name).to_string_lossy().into_owned())
}

pub fn create(dir: &Path) -> Result<StreamingEngine> {
    let mut config = OnlineRecognizerConfig::default();
    config.model_config.transducer.encoder = path_string(dir, model::ENCODER);
    config.model_config.transducer.decoder = path_string(dir, model::DECODER);
    config.model_config.transducer.joiner = path_string(dir, model::JOINER);
    config.model_config.tokens = path_string(dir, model::TOKENS);
    config.model_config.num_threads = NUM_THREADS;
    config.decoding_method = Some("greedy_search".to_string());
    // Segmentation is done on token times (see `gate::next_commit`); sherpa's
    // endpoint + reset was measured to drop words at utterance boundaries.
    config.enable_endpoint = false;
    let recognizer = OnlineRecognizer::create(&config)
        .ok_or_else(|| anyhow!("sherpa-onnx could not load the Nemotron streaming model"))?;
    Ok(StreamingEngine { recognizer })
}

/// Load the recognizer, or reuse the loaded one. Blocking (~2 s): call it
/// from `spawn_blocking` or before capture starts.
pub fn get_or_load<R: Runtime>(app: &AppHandle<R>) -> Result<Arc<StreamingEngine>> {
    let mut guard = ENGINE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(engine) = guard.as_ref() {
        return Ok(engine.clone());
    }
    let dir = model::model_dir(app)?;
    if !model::is_downloaded_in(&dir) {
        return Err(anyhow!("The Nemotron live model is not downloaded"));
    }
    let started = std::time::Instant::now();
    let engine = Arc::new(create(&dir)?);
    info!(
        "Nemotron streaming model loaded in {:.1}s",
        started.elapsed().as_secs_f32()
    );
    *guard = Some(engine.clone());
    Ok(engine)
}

pub fn is_loaded() -> bool {
    ENGINE.lock().map(|g| g.is_some()).unwrap_or(false)
}

/// Drop the cached recognizer. A worker still holding an `Arc` keeps it alive
/// until it finishes.
pub fn unload() {
    if ENGINE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .is_some()
    {
        info!("Nemotron streaming model unloaded");
    }
}
