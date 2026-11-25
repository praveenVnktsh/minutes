//! The loaded Nemotron streaming engine, shared by recordings.
//!
//! The engine is Nemotron Speech Streaming 0.6B on Core ML, run by FluidAudio
//! in the `speech-helper` sidecar's live role. Settings → Advanced → Live
//! transcription engine (`LiveEngineChoice`) picks the GPU or the Neural
//! Engine. Without the downloaded model or the helper there is no live
//! engine, and meetings are recorded without live text (the final pass
//! transcribes them).
//!
//! Loading takes a few seconds and ~1 GB, so the engine is created once per
//! recording (or reused if still loaded) and unloaded when the recording
//! stops, which frees the memory before the final pass loads its engine.

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use log::{info, warn};
use tauri::{AppHandle, Runtime};

use super::native::NativeRecognizer;
use super::recognizer::Recognizer;
use crate::audio::speech::helper::{self, Compute, Role};
use crate::audio::speech::models::{self, ModelKind};
use crate::audio::transcription_settings::{load_settings, LiveEngineChoice};

/// What Auto runs: the Neural Engine. Measured on an M5 with two real
/// meeting clips (two streams): the same words as the GPU, ~5% CPU against
/// ~9% for the GPU, ready in ~0.5 s against ~13–15 s for the GPU (its
/// shaders compile on every launch), and it leaves the GPU to the final pass
/// and the built-in LLM. It also kept up in Low Power Mode on a loaded
/// machine. See the design doc's "Engines" section.
pub const AUTO_COMPUTE: Compute = Compute::NeuralEngine;

/// Provenance id of the live model: its directory in the FluidAudio models
/// store (`audio/speech/models.rs`).
pub const MODEL_ID: &str = "nemotron-speech-streaming-en-0.6b-coreml";

/// Where the live model runs for `choice`.
pub fn compute_for(choice: LiveEngineChoice) -> Compute {
    match choice {
        LiveEngineChoice::Auto => AUTO_COMPUTE,
        LiveEngineChoice::Gpu => Compute::Gpu,
        LiveEngineChoice::NeuralEngine => Compute::NeuralEngine,
    }
}

pub struct StreamingEngine {
    pub recognizer: Box<dyn Recognizer>,
    pub compute: Compute,
}

impl StreamingEngine {
    /// Provenance id of the model this engine runs.
    pub fn model_id(&self) -> &'static str {
        MODEL_ID
    }

    pub fn device(&self) -> &'static str {
        self.recognizer.device()
    }
}

static ENGINE: Mutex<Option<Arc<StreamingEngine>>> = Mutex::new(None);

/// The helper's live role on `models_dir` (the FluidAudio models directory,
/// holding [`MODEL_ID`]), on `compute`. Returns at once; the models load in
/// the helper.
pub fn create_native(models_dir: &Path, compute: Compute) -> Result<StreamingEngine> {
    if helper::resolve_helper().is_none() {
        return Err(anyhow!("speech-helper is missing"));
    }
    Ok(StreamingEngine {
        recognizer: Box::new(NativeRecognizer::spawn(models_dir, compute)?),
        compute,
    })
}

/// The Auto engine on `models_dir`, for tests and developer tools that point
/// at a models directory themselves.
pub fn create(models_dir: &Path) -> Result<StreamingEngine> {
    create_native(models_dir, AUTO_COMPUTE)
}

/// Start the engine the setting asks for, or reuse the loaded one if it runs
/// there. Spawns the helper (the model loads in the background, and the
/// first decode waits for it): call it from `spawn_blocking` or before
/// capture starts.
pub fn get_or_load<R: Runtime>(app: &AppHandle<R>) -> Result<Arc<StreamingEngine>> {
    let compute = compute_for(load_settings(app).live_engine);
    let mut guard = ENGINE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(engine) = guard.as_ref() {
        if engine.compute.arg() == compute.arg() {
            return Ok(engine.clone());
        }
    }
    *guard = None;
    if !models::is_downloaded(app, ModelKind::LiveAsr) {
        return Err(anyhow!("The Nemotron live model is not downloaded"));
    }
    let dir = models::dir(app)?;
    let engine = Arc::new(create_native(&dir, compute)?);
    info!("Nemotron streaming model starting on {}", engine.device());
    *guard = Some(engine.clone());
    Ok(engine)
}

pub fn is_loaded() -> bool {
    ENGINE.lock().map(|g| g.is_some()).unwrap_or(false)
}

/// Drop the cached engine. A worker still holding an `Arc` keeps it alive
/// until it finishes (the helper process exits when its engine is dropped).
pub fn unload() {
    let engine = ENGINE.lock().unwrap_or_else(|e| e.into_inner()).take();
    if engine.is_some() {
        info!("Nemotron streaming model unloaded");
        drop_in_background(engine);
    }
}

/// Drop an engine handle on its own thread: dropping the last one waits up
/// to 2 s for the helper to exit, which must not stall an async worker.
/// Callers may be sync or async, so a plain thread rather than tokio's pool.
pub fn drop_in_background<T: Send + 'static>(value: T) {
    let spawned = std::thread::Builder::new()
        .name("live-engine-drop".into())
        .spawn(move || drop(value));
    if let Err(error) = spawned {
        warn!("Could not release the live engine in the background: {error}");
    }
}

/// Compile the Core ML models for the device the setting uses, in the
/// background, so the first recording does not wait for it. Core ML caches
/// the compiled model (a cold Neural Engine compile took over a minute, a
/// cached load under a second). The GPU's shader compile is not cached
/// between launches (~10–20 s on every load), so at app start (`startup`)
/// there is nothing to gain for it and it is skipped.
pub fn spawn_prepare<R: Runtime>(app: AppHandle<R>, startup: bool) {
    let compute = match load_settings(&app).live_engine {
        LiveEngineChoice::Gpu if startup => return,
        choice => compute_for(choice),
    };
    if helper::resolve_helper().is_none() || !models::is_downloaded(&app, ModelKind::LiveAsr) {
        return;
    }
    let Ok(dir) = models::dir(&app) else {
        return;
    };
    std::thread::spawn(move || match helper::prepare(Role::Live, &dir, compute) {
        Ok(seconds) => info!(
            "Nemotron Core ML ({}) prepared in {seconds:.1}s",
            compute.arg()
        ),
        Err(error) => warn!("Nemotron Core ML preparation failed: {error:#}"),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_choice_runs_on_its_device_and_auto_on_the_neural_engine() {
        assert_eq!(compute_for(LiveEngineChoice::Auto).arg(), "ane");
        assert_eq!(compute_for(LiveEngineChoice::NeuralEngine).arg(), "ane");
        assert_eq!(compute_for(LiveEngineChoice::Gpu).arg(), "gpu");
    }
}
