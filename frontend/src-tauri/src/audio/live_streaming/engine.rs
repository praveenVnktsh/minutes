//! The loaded Nemotron streaming engine, shared by recordings.
//!
//! Which engine runs follows Settings → Advanced → Live transcription engine
//! (`LiveEngineChoice`): Core ML on the GPU or Neural Engine through the
//! `nemotron-helper` sidecar, or sherpa-onnx on the CPU. A choice whose model
//! or helper is missing, or that fails to load, falls back to the others in
//! [`candidates`] order, so live text keeps working.
//!
//! Loading takes a few seconds and ~1 GB, so the engine is created once per
//! recording (or reused if still loaded) and unloaded when the recording
//! stops, which frees the memory before the final pass loads its engine.

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use log::{info, warn};
use sherpa_onnx::{OnlineRecognizer, OnlineRecognizerConfig};
use tauri::{AppHandle, Runtime};

use super::model::{self, Bundle};
use super::native::{self, Compute, NativeRecognizer};
use super::recognizer::{Recognizer, SherpaRecognizer};
use crate::audio::transcription_settings::{load_settings, LiveEngineChoice};

/// Intra-op threads. Two streams are decoded as one batch, and more threads
/// than this contend with capture and the UI without speeding decode up.
const NUM_THREADS: i32 = 2;

/// What Auto runs: the Neural Engine. Measured on an M5 with two real
/// meeting clips (two streams): the same words as the GPU (both ~3% from the
/// CPU export), ~5% CPU against ~9% for the GPU and ~160% for sherpa, ready
/// in ~0.5 s against ~13–15 s for the GPU (its shaders compile on every
/// launch), and it leaves the GPU to the final pass and the built-in LLM.
/// It also kept up in Low Power Mode on a loaded machine, where the CPU
/// engine fell behind. See the design doc's "Engines" section.
pub const AUTO_COMPUTE: Compute = Compute::NeuralEngine;

/// An engine the live model can run on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Native(Compute),
    Sherpa,
}

impl Backend {
    fn bundle(self) -> &'static Bundle {
        match self {
            Self::Native(_) => &model::COREML,
            Self::Sherpa => &model::SHERPA,
        }
    }
}

/// Engines to try for `choice`, best first, given what is on disk. The chosen
/// one comes first; the rest are fallbacks so live text keeps working.
pub fn candidates(
    choice: LiveEngineChoice,
    coreml_ready: bool,
    sherpa_ready: bool,
) -> Vec<Backend> {
    let preferred = match choice {
        LiveEngineChoice::Auto => Backend::Native(AUTO_COMPUTE),
        LiveEngineChoice::Gpu => Backend::Native(Compute::Gpu),
        LiveEngineChoice::NeuralEngine => Backend::Native(Compute::NeuralEngine),
        LiveEngineChoice::Cpu => Backend::Sherpa,
    };
    let mut order = vec![preferred];
    match preferred {
        Backend::Native(_) => order.push(Backend::Sherpa),
        Backend::Sherpa => order.push(Backend::Native(AUTO_COMPUTE)),
    }
    order
        .into_iter()
        .filter(|backend| match backend {
            Backend::Native(_) => coreml_ready,
            Backend::Sherpa => sherpa_ready,
        })
        .collect()
}

pub struct StreamingEngine {
    pub recognizer: Box<dyn Recognizer>,
    pub backend: Backend,
}

impl StreamingEngine {
    /// Provenance id of the model bundle this engine runs.
    pub fn model_id(&self) -> &'static str {
        self.backend.bundle().id
    }

    pub fn device(&self) -> &'static str {
        self.recognizer.device()
    }
}

static ENGINE: Mutex<Option<Arc<StreamingEngine>>> = Mutex::new(None);

fn path_string(dir: &Path, name: &str) -> Option<String> {
    Some(dir.join(name).to_string_lossy().into_owned())
}

/// sherpa-onnx on the CPU, from the sherpa bundle in `dir`.
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
    Ok(StreamingEngine {
        recognizer: Box::new(SherpaRecognizer::new(recognizer)),
        backend: Backend::Sherpa,
    })
}

/// Core ML in the helper, from the Core ML bundle in `dir`.
pub fn create_native(dir: &Path, compute: Compute) -> Result<StreamingEngine> {
    let helper = native::resolve_helper().ok_or_else(|| anyhow!("nemotron-helper is missing"))?;
    Ok(StreamingEngine {
        recognizer: Box::new(NativeRecognizer::spawn(&helper, dir, compute)?),
        backend: Backend::Native(compute),
    })
}

fn load(backend: Backend, dir: &Path) -> Result<StreamingEngine> {
    match backend {
        Backend::Native(compute) => create_native(dir, compute),
        Backend::Sherpa => create(dir),
    }
}

/// Load the engine the setting asks for (falling back as [`candidates`]
/// says), or reuse the loaded one if it is that engine. Blocking (seconds):
/// call it from `spawn_blocking` or before capture starts.
pub fn get_or_load<R: Runtime>(app: &AppHandle<R>) -> Result<Arc<StreamingEngine>> {
    let choice = load_settings(app).live_engine;
    let helper = native::resolve_helper().is_some();
    let order = candidates(
        choice,
        helper && model::COREML.is_downloaded(app),
        model::SHERPA.is_downloaded(app),
    );
    let mut guard = ENGINE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(engine) = guard.as_ref() {
        if order.first() == Some(&engine.backend) {
            return Ok(engine.clone());
        }
    }
    *guard = None;
    if order.is_empty() {
        return Err(anyhow!("The Nemotron live model is not downloaded"));
    }
    let mut last_error = None;
    for backend in order {
        let dir = backend.bundle().dir(app)?;
        let started = std::time::Instant::now();
        match load(backend, &dir) {
            Ok(engine) => {
                info!(
                    "Nemotron streaming model loaded on {} in {:.1}s",
                    engine.device(),
                    started.elapsed().as_secs_f32()
                );
                let engine = Arc::new(engine);
                *guard = Some(engine.clone());
                return Ok(engine);
            }
            Err(error) => {
                warn!("Nemotron live engine {backend:?} failed to load: {error:#}");
                last_error = Some(error);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow!("no live engine")))
}

/// The CPU engine, for a recording whose Core ML helper failed mid-meeting.
pub fn load_cpu_fallback<R: Runtime>(app: &AppHandle<R>) -> Result<Arc<StreamingEngine>> {
    let dir = model::SHERPA.dir(app)?;
    if !model::SHERPA.is_downloaded_in(&dir) {
        return Err(anyhow!("the CPU live model is not downloaded"));
    }
    let engine = Arc::new(create(&dir)?);
    *ENGINE.lock().unwrap_or_else(|e| e.into_inner()) = Some(engine.clone());
    Ok(engine)
}

pub fn is_loaded() -> bool {
    ENGINE.lock().map(|g| g.is_some()).unwrap_or(false)
}

/// Drop the cached engine. A worker still holding an `Arc` keeps it alive
/// until it finishes (a helper process exits when its engine is dropped).
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

/// Compile the Core ML models for the device the setting uses, in the
/// background, so the first recording does not wait for it. Core ML caches
/// the compiled model (a cold Neural Engine compile took over a minute, a
/// cached load under a second). The GPU's shader compile is not cached
/// between launches (~10–20 s on every load), so at app start (`startup`)
/// there is nothing to gain for it and it is skipped.
pub fn spawn_prepare<R: Runtime>(app: AppHandle<R>, startup: bool) {
    let compute = match load_settings(&app).live_engine {
        LiveEngineChoice::Cpu => return,
        LiveEngineChoice::Gpu if startup => return,
        LiveEngineChoice::Gpu => Compute::Gpu,
        LiveEngineChoice::NeuralEngine => Compute::NeuralEngine,
        LiveEngineChoice::Auto => AUTO_COMPUTE,
    };
    let (Some(helper), Ok(dir)) = (native::resolve_helper(), model::COREML.dir(&app)) else {
        return;
    };
    if !model::COREML.is_downloaded_in(&dir) {
        return;
    }
    std::thread::spawn(move || match native::prepare(&helper, &dir, compute) {
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
    fn the_chosen_engine_comes_first_and_the_other_is_the_fallback() {
        use LiveEngineChoice::*;
        assert_eq!(
            candidates(Gpu, true, true),
            vec![Backend::Native(Compute::Gpu), Backend::Sherpa]
        );
        assert_eq!(
            candidates(NeuralEngine, true, true),
            vec![Backend::Native(Compute::NeuralEngine), Backend::Sherpa]
        );
        assert_eq!(
            candidates(Auto, true, true),
            vec![Backend::Native(AUTO_COMPUTE), Backend::Sherpa]
        );
        assert_eq!(
            candidates(Cpu, true, true),
            vec![Backend::Sherpa, Backend::Native(AUTO_COMPUTE)]
        );
    }

    #[test]
    fn missing_models_drop_out_of_the_order() {
        use LiveEngineChoice::*;
        // An existing user on Auto before the Core ML download finishes.
        assert_eq!(candidates(Auto, false, true), vec![Backend::Sherpa]);
        assert_eq!(
            candidates(Cpu, true, false),
            vec![Backend::Native(AUTO_COMPUTE)]
        );
        assert!(candidates(Gpu, false, false).is_empty());
    }
}
