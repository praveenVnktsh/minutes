//! Speaker diarization with NVIDIA's Nemotron-3 streaming Sortformer.
//!
//! The model is an ONNX export of the offline preset of `nvidia/Nemotron-3-Diarization`.
//! One graph call scores one chunk step: 2720 log-mel frames (27.2 s) plus 320
//! frames (3.2 s) of right context, and returns per-frame logits for up to eight
//! speakers at 10 ms resolution. Overlapping speech comes out natively, as more
//! than one active channel in the same frame.
//!
//! Speakers are numbered in the order they first speak. The model keeps that
//! numbering stable across chunks through its arrival-order speaker cache
//! (AOSC), which lives in the graph's inputs and outputs rather than inside the
//! session, so [`SortformerDiarizer`] carries the seven state tensors from each
//! step into the next.
//!
//! Input audio must be 16 kHz mono f32, e.g. from
//! `decode_audio_file(path)?.to_whisper_format()`; [`mel`] computes the matching
//! features.

pub mod mel;

use std::ops::Range;
use std::path::Path;
use std::time::Instant;

use anyhow::{anyhow, bail, ensure, Context, Result};
use ndarray::{arr0, s, Array2, Array3, ArrayD, ArrayView2, ArrayViewMut2, Axis, IxDyn};
use ort::execution_providers::CPUExecutionProvider;
use ort::inputs;
use ort::session::builder::GraphOptimizationLevel;
use ort::session::{Session, SessionOutputs};
use ort::tensor::TensorElementType;
use ort::value::TensorRef;

/// File name the model is stored under.
pub const MODEL_FILE: &str = "nemotron3-diar-step-offline.onnx";
/// Download URL, pinned to one revision of the Hugging Face repo so the file
/// always matches [`MODEL_SHA256`].
pub const MODEL_URL: &str = "https://huggingface.co/tdavis9/lor-weaver-nemotron3-diarization-onnx/resolve/02bae198c6de4fdf87969723c71667b8841a4ceb/nemotron3-diar-step-offline.onnx";
/// SHA-256 of [`MODEL_FILE`] at the pinned revision.
pub const MODEL_SHA256: &str = "d4489043be6e677a266837b4be1bdd5c31a3b059a0b6ddeb4d531d0e6083c01e";
/// Size of [`MODEL_FILE`] in bytes at the pinned revision.
pub const MODEL_BYTES: u64 = 397_889_899;
/// Speaker channels the model scores.
pub const MAX_SPEAKERS: usize = 8;
/// Duration of one output frame: frame `i` covers `[i, i + 1) * FRAME_SECONDS`.
pub const FRAME_SECONDS: f64 = 0.01;

// Streaming geometry of the offline preset, in 10 ms mel frames.
/// Frames each step scores.
const CHUNK_FRAMES: usize = 2720;
/// Frames of look-ahead each step sees after its chunk but does not score.
const RIGHT_CONTEXT_FRAMES: usize = 320;
/// Frames fed to one graph call.
const STEP_FRAMES: usize = CHUNK_FRAMES + RIGHT_CONTEXT_FRAMES;
/// The encoder's subsampling factor; `num_input_frames` counts encoder frames.
const SUBSAMPLING_FACTOR: usize = 8;
/// Slots in the arrival-order speaker cache.
const SPEAKER_CACHE_FRAMES: usize = 264;
/// Slots in the FIFO of recent encoder frames not yet merged into the cache.
const FIFO_FRAMES: usize = 40;
/// Width of the encoder embeddings held in the cache and FIFO.
const EMBEDDING_DIM: usize = 512;

/// Upper bound on ONNX Runtime's intra-op threads, so a diarization run leaves
/// cores for the rest of the app.
const MAX_INTRA_THREADS: usize = 4;

// Post-processing of per-frame probabilities into segments.
/// A speaker channel is active in a frame when its probability exceeds this.
const ACTIVITY_THRESHOLD: f32 = 0.5;
/// Silences shorter than this (0.3 s) between two runs of the same speaker are
/// bridged, so natural pauses do not split a turn.
const MAX_BRIDGED_GAP_FRAMES: usize = 30;
/// Runs shorter than this (0.15 s) after bridging are dropped as blips.
const MIN_SEGMENT_FRAMES: usize = 15;

/// One stretch of speech attributed to one speaker. Segments of different
/// speakers may overlap.
#[derive(Debug, Clone, PartialEq)]
pub struct SpeakerSegment {
    /// Start time in seconds.
    pub start: f64,
    /// End time in seconds (exclusive).
    pub end: f64,
    /// Model speaker channel, `0..MAX_SPEAKERS`, numbered in order of first speech.
    pub speaker: usize,
    /// Mean speaker probability over the segment, bridged gaps included.
    pub confidence: f32,
}

/// A tensor the graph must expose: its name, element type and dimensions,
/// where `-1` in either the expected or the declared shape matches any size.
struct TensorSpec {
    name: &'static str,
    ty: TensorElementType,
    dims: &'static [i64],
}

const INPUT_FEATURES: &str = "input_features";
const ATTENTION_MASK: &str = "attention_mask";
const CACHE_EMBEDS: &str = "cache_embeds";
const CACHE_PROBS: &str = "cache_probs";
const FIFO: &str = "fifo";
const NUM_CACHE_FRAMES: &str = "num_cache_frames";
const NUM_FIFO_FRAMES: &str = "num_fifo_frames";
const IS_COMPRESSED: &str = "is_compressed";
const NUM_INPUT_FRAMES: &str = "num_input_frames";

const LOGITS: &str = "logits";
const NEW_CACHE_EMBEDS: &str = "new_cache_embeds";
const NEW_CACHE_PROBS: &str = "new_cache_probs";
const NEW_FIFO: &str = "new_fifo";
const NEW_NUM_CACHE_FRAMES: &str = "new_num_cache_frames";
const NEW_NUM_FIFO_FRAMES: &str = "new_num_fifo_frames";
const NEW_IS_COMPRESSED: &str = "new_is_compressed";

const CACHE_EMBEDS_DIMS: [usize; 3] = [1, SPEAKER_CACHE_FRAMES, EMBEDDING_DIM];
const CACHE_PROBS_DIMS: [usize; 3] = [1, SPEAKER_CACHE_FRAMES, MAX_SPEAKERS];
const FIFO_DIMS: [usize; 3] = [1, FIFO_FRAMES, EMBEDDING_DIM];

const EXPECTED_INPUTS: [TensorSpec; 9] = [
    TensorSpec {
        name: INPUT_FEATURES,
        ty: TensorElementType::Float32,
        dims: &[1, STEP_FRAMES as i64, mel::N_MELS as i64],
    },
    TensorSpec {
        name: ATTENTION_MASK,
        ty: TensorElementType::Float32,
        dims: &[1, STEP_FRAMES as i64],
    },
    TensorSpec {
        name: CACHE_EMBEDS,
        ty: TensorElementType::Float32,
        dims: &[1, SPEAKER_CACHE_FRAMES as i64, EMBEDDING_DIM as i64],
    },
    TensorSpec {
        name: CACHE_PROBS,
        ty: TensorElementType::Float32,
        dims: &[1, SPEAKER_CACHE_FRAMES as i64, MAX_SPEAKERS as i64],
    },
    TensorSpec {
        name: FIFO,
        ty: TensorElementType::Float32,
        dims: &[1, FIFO_FRAMES as i64, EMBEDDING_DIM as i64],
    },
    TensorSpec {
        name: NUM_CACHE_FRAMES,
        ty: TensorElementType::Int64,
        dims: &[],
    },
    TensorSpec {
        name: NUM_FIFO_FRAMES,
        ty: TensorElementType::Int64,
        dims: &[],
    },
    TensorSpec {
        name: IS_COMPRESSED,
        ty: TensorElementType::Int64,
        dims: &[],
    },
    TensorSpec {
        name: NUM_INPUT_FRAMES,
        ty: TensorElementType::Int64,
        dims: &[],
    },
];

const EXPECTED_OUTPUTS: [TensorSpec; 7] = [
    TensorSpec {
        name: LOGITS,
        ty: TensorElementType::Float32,
        dims: &[1, -1, MAX_SPEAKERS as i64],
    },
    TensorSpec {
        name: NEW_CACHE_EMBEDS,
        ty: TensorElementType::Float32,
        dims: &[1, SPEAKER_CACHE_FRAMES as i64, EMBEDDING_DIM as i64],
    },
    TensorSpec {
        name: NEW_CACHE_PROBS,
        ty: TensorElementType::Float32,
        dims: &[1, SPEAKER_CACHE_FRAMES as i64, MAX_SPEAKERS as i64],
    },
    TensorSpec {
        name: NEW_FIFO,
        ty: TensorElementType::Float32,
        dims: &[1, FIFO_FRAMES as i64, EMBEDDING_DIM as i64],
    },
    TensorSpec {
        name: NEW_NUM_CACHE_FRAMES,
        ty: TensorElementType::Int64,
        dims: &[],
    },
    TensorSpec {
        name: NEW_NUM_FIFO_FRAMES,
        ty: TensorElementType::Int64,
        dims: &[],
    },
    TensorSpec {
        name: NEW_IS_COMPRESSED,
        ty: TensorElementType::Int64,
        dims: &[],
    },
];

/// Runs the Nemotron-3 streaming Sortformer over whole clips.
pub struct SortformerDiarizer {
    session: Session,
}

impl SortformerDiarizer {
    /// Loads the model on the CPU execution provider and checks that it exposes
    /// the inputs and outputs this runner feeds and reads.
    pub fn new(model_path: &Path) -> Result<Self> {
        crate::ensure_onnx_runtime_available()
            .context("ONNX Runtime is unavailable, so Sortformer diarization cannot run")?;
        ensure!(
            model_path.is_file(),
            "Sortformer model not found at {}",
            model_path.display()
        );

        let intra_threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .min(MAX_INTRA_THREADS);
        let started = Instant::now();
        let session = Session::builder()?
            .with_optimization_level(GraphOptimizationLevel::Level3)?
            .with_execution_providers([CPUExecutionProvider::default().build()])?
            .with_intra_threads(intra_threads)?
            .commit_from_file(model_path)
            .with_context(|| {
                format!(
                    "Failed to load Sortformer model from {}",
                    model_path.display()
                )
            })?;

        verify_signature(&session).with_context(|| {
            format!(
                "{} is not the expected Nemotron-3 Sortformer step graph",
                model_path.display()
            )
        })?;

        log::info!(
            "Loaded Sortformer diarization model {} in {:.2?} ({} intra-op threads)",
            model_path.display(),
            started.elapsed(),
            intra_threads
        );
        Ok(Self { session })
    }

    /// Per-frame speaker activity for a 16 kHz mono clip, as sigmoid
    /// probabilities of shape `(frames, MAX_SPEAKERS)` with 10 ms frames
    /// (`samples.len() / 160` of them). Column `k` is the `k`-th speaker to
    /// start talking. `on_progress` receives the fraction of frames scored
    /// after each step and ends at 1.0. Audio shorter than one frame yields an
    /// empty array.
    pub fn speaker_probabilities(
        &mut self,
        samples: &[f32],
        mut on_progress: impl FnMut(f32),
    ) -> Result<Array2<f32>> {
        let started = Instant::now();
        let features = mel::log_mel_features(samples);
        let total_frames = features.nrows();
        ensure!(
            features.ncols() == mel::N_MELS,
            "mel front end produced {} bins, expected {}",
            features.ncols(),
            mel::N_MELS
        );
        let mel_elapsed = started.elapsed();

        let mut probabilities = Array2::<f32>::zeros((total_frames, MAX_SPEAKERS));
        let steps = plan_steps(total_frames);
        if steps.is_empty() {
            on_progress(1.0);
            return Ok(probabilities);
        }

        let mut input_features = Array3::<f32>::zeros((1, STEP_FRAMES, mel::N_MELS));
        let mut attention_mask = Array2::<f32>::zeros((1, STEP_FRAMES));
        let mut state = StreamState::initial();

        for (index, step) in steps.iter().enumerate() {
            let real = step.real_frames();
            let window = step.input.clone();
            input_features
                .slice_mut(s![0, ..real, ..])
                .assign(&features.slice(s![window, ..]));
            input_features.slice_mut(s![0, real.., ..]).fill(0.0);
            attention_mask.slice_mut(s![0, ..real]).fill(1.0);
            attention_mask.slice_mut(s![0, real..]).fill(0.0);

            self.run_step(
                &input_features,
                &attention_mask,
                step.num_input_frames(),
                &mut state,
                probabilities.slice_mut(s![step.output.clone(), ..]),
            )
            .with_context(|| format!("Sortformer step {} of {} failed", index + 1, steps.len()))?;

            on_progress(step.output.end as f32 / total_frames as f32);
        }

        let elapsed = started.elapsed();
        let audio_seconds = total_frames as f64 * FRAME_SECONDS;
        log::info!(
            "Sortformer scored {:.1} s of audio in {} step(s) in {:.2?} (mel {:.2?}, {:.1}x real time)",
            audio_seconds,
            steps.len(),
            elapsed,
            mel_elapsed,
            audio_seconds / elapsed.as_secs_f64().max(f64::EPSILON)
        );
        Ok(probabilities)
    }

    /// Speaker segments for a 16 kHz mono clip: [`Self::speaker_probabilities`]
    /// followed by [`segments_from_probabilities`].
    pub fn diarize(
        &mut self,
        samples: &[f32],
        on_progress: impl FnMut(f32),
    ) -> Result<Vec<SpeakerSegment>> {
        let probabilities = self.speaker_probabilities(samples, on_progress)?;
        let segments = segments_from_probabilities(probabilities.view());
        let speakers = segments
            .iter()
            .map(|segment| segment.speaker)
            .collect::<std::collections::BTreeSet<_>>();
        log::info!(
            "Sortformer found {} segment(s) from {} speaker(s)",
            segments.len(),
            speakers.len()
        );
        Ok(segments)
    }

    /// Runs one chunk step, writes the sigmoid of its first `out.nrows()`
    /// logit frames into `out`, and replaces `state` with the graph's new state.
    fn run_step(
        &mut self,
        input_features: &Array3<f32>,
        attention_mask: &Array2<f32>,
        num_input_frames: i64,
        state: &mut StreamState,
        mut out: ArrayViewMut2<f32>,
    ) -> Result<()> {
        let num_cache_frames = arr0(state.num_cache_frames);
        let num_fifo_frames = arr0(state.num_fifo_frames);
        let is_compressed = arr0(state.is_compressed);
        let num_input_frames = arr0(num_input_frames);

        let outputs = self.session.run(inputs![
            INPUT_FEATURES => TensorRef::from_array_view(input_features)?,
            ATTENTION_MASK => TensorRef::from_array_view(attention_mask)?,
            CACHE_EMBEDS => TensorRef::from_array_view(&state.cache_embeds)?,
            CACHE_PROBS => TensorRef::from_array_view(&state.cache_probs)?,
            FIFO => TensorRef::from_array_view(&state.fifo)?,
            NUM_CACHE_FRAMES => TensorRef::from_array_view(&num_cache_frames)?,
            NUM_FIFO_FRAMES => TensorRef::from_array_view(&num_fifo_frames)?,
            IS_COMPRESSED => TensorRef::from_array_view(&is_compressed)?,
            NUM_INPUT_FRAMES => TensorRef::from_array_view(&num_input_frames)?,
        ])?;

        let logits = output(&outputs, LOGITS)?.try_extract_array::<f32>()?;
        let shape = logits.shape();
        ensure!(
            shape.len() == 3
                && shape[0] == 1
                && shape[1] >= out.nrows()
                && shape[2] == MAX_SPEAKERS,
            "logits have shape {:?}, expected [1, >={}, {}]",
            shape,
            out.nrows(),
            MAX_SPEAKERS
        );
        let rows = out.nrows();
        let logits = logits.index_axis(Axis(0), 0);
        out.zip_mut_with(&logits.slice(s![..rows, ..]), |p, &logit| {
            *p = sigmoid(logit)
        });

        *state = StreamState {
            cache_embeds: state_tensor(&outputs, NEW_CACHE_EMBEDS, &CACHE_EMBEDS_DIMS)?,
            cache_probs: state_tensor(&outputs, NEW_CACHE_PROBS, &CACHE_PROBS_DIMS)?,
            fifo: state_tensor(&outputs, NEW_FIFO, &FIFO_DIMS)?,
            num_cache_frames: state_scalar(&outputs, NEW_NUM_CACHE_FRAMES)?,
            num_fifo_frames: state_scalar(&outputs, NEW_NUM_FIFO_FRAMES)?,
            is_compressed: state_scalar(&outputs, NEW_IS_COMPRESSED)?,
        };
        Ok(())
    }
}

/// The streaming state the graph threads from one step to the next: the
/// arrival-order speaker cache, the FIFO of recent frames, and their counters.
struct StreamState {
    cache_embeds: ArrayD<f32>,
    cache_probs: ArrayD<f32>,
    fifo: ArrayD<f32>,
    num_cache_frames: i64,
    num_fifo_frames: i64,
    is_compressed: i64,
}

impl StreamState {
    /// The state before the first step: empty cache and FIFO.
    fn initial() -> Self {
        Self {
            cache_embeds: ArrayD::zeros(IxDyn(&CACHE_EMBEDS_DIMS)),
            cache_probs: ArrayD::zeros(IxDyn(&CACHE_PROBS_DIMS)),
            fifo: ArrayD::zeros(IxDyn(&FIFO_DIMS)),
            num_cache_frames: 0,
            num_fifo_frames: 0,
            is_compressed: 0,
        }
    }
}

fn output<'a>(outputs: &'a SessionOutputs<'_>, name: &str) -> Result<&'a ort::value::DynValue> {
    outputs
        .get(name)
        .ok_or_else(|| anyhow!("Sortformer graph returned no '{name}' output"))
}

/// Copies a state tensor out of the step's outputs, checking it has the shape
/// the next step will feed back.
fn state_tensor(outputs: &SessionOutputs<'_>, name: &str, dims: &[usize]) -> Result<ArrayD<f32>> {
    let tensor = output(outputs, name)?.try_extract_array::<f32>()?;
    ensure!(
        tensor.shape() == dims,
        "'{name}' has shape {:?}, expected {:?}",
        tensor.shape(),
        dims
    );
    Ok(tensor.to_owned())
}

/// Reads a single-element i64 state output.
fn state_scalar(outputs: &SessionOutputs<'_>, name: &str) -> Result<i64> {
    let (_, data) = output(outputs, name)?.try_extract_tensor::<i64>()?;
    match data {
        [value] => Ok(*value),
        _ => bail!("'{name}' has {} elements, expected 1", data.len()),
    }
}

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

/// Fails unless the session has exactly the inputs this runner feeds and at
/// least the outputs it reads, with matching element types and shapes.
fn verify_signature(session: &Session) -> Result<()> {
    let input_names = || {
        session
            .inputs
            .iter()
            .map(|input| input.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    for spec in &EXPECTED_INPUTS {
        let input = session
            .inputs
            .iter()
            .find(|input| input.name == spec.name)
            .ok_or_else(|| {
                anyhow!(
                    "model has no input named '{}' (inputs: {})",
                    spec.name,
                    input_names()
                )
            })?;
        check_tensor(
            spec,
            input.input_type.tensor_type(),
            input.input_type.tensor_shape().map(|shape| &shape[..]),
        )
        .with_context(|| format!("input '{}'", spec.name))?;
    }
    if let Some(extra) = session
        .inputs
        .iter()
        .find(|input| !EXPECTED_INPUTS.iter().any(|spec| spec.name == input.name))
    {
        bail!("model has an unexpected input '{}'", extra.name);
    }

    for spec in &EXPECTED_OUTPUTS {
        let output = session
            .outputs
            .iter()
            .find(|output| output.name == spec.name)
            .ok_or_else(|| {
                anyhow!(
                    "model has no output named '{}' (outputs: {})",
                    spec.name,
                    session
                        .outputs
                        .iter()
                        .map(|output| output.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?;
        check_tensor(
            spec,
            output.output_type.tensor_type(),
            output.output_type.tensor_shape().map(|shape| &shape[..]),
        )
        .with_context(|| format!("output '{}'", spec.name))?;
    }
    Ok(())
}

/// Checks one declared tensor against its spec. `ty` and `dims` are `None`
/// when the declared value is not a tensor.
fn check_tensor(
    spec: &TensorSpec,
    ty: Option<TensorElementType>,
    dims: Option<&[i64]>,
) -> Result<()> {
    let (Some(ty), Some(dims)) = (ty, dims) else {
        bail!("is not a tensor");
    };
    ensure!(
        ty == spec.ty,
        "has element type {ty:?}, expected {:?}",
        spec.ty
    );
    let compatible = dims.len() == spec.dims.len()
        && dims
            .iter()
            .zip(spec.dims)
            .all(|(&declared, &expected)| declared < 0 || expected < 0 || declared == expected);
    ensure!(compatible, "has shape {dims:?}, expected {:?}", spec.dims);
    Ok(())
}

/// One graph call's share of a clip, in mel frames.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Step {
    /// Clip frames fed to the graph; the rest of the step's window is padding.
    input: Range<usize>,
    /// Clip frames this step scores, a prefix of `input`.
    output: Range<usize>,
}

impl Step {
    fn real_frames(&self) -> usize {
        self.input.len()
    }

    /// Encoder frames covering the real input, rounded up.
    fn num_input_frames(&self) -> i64 {
        self.real_frames().div_ceil(SUBSAMPLING_FACTOR) as i64
    }
}

/// Splits a clip of `total_frames` mel frames into chunk steps. Step `k` feeds
/// frames `[k * CHUNK_FRAMES, k * CHUNK_FRAMES + STEP_FRAMES)`, clipped to the
/// clip, and scores its first `CHUNK_FRAMES` of them; together the outputs
/// tile the whole clip.
fn plan_steps(total_frames: usize) -> Vec<Step> {
    (0..total_frames)
        .step_by(CHUNK_FRAMES)
        .map(|start| Step {
            input: start..total_frames.min(start + STEP_FRAMES),
            output: start..total_frames.min(start + CHUNK_FRAMES),
        })
        .collect()
}

/// Turns per-frame speaker probabilities of shape `(frames, speakers)` into
/// segments. Per speaker channel, frames with probability above 0.5 are
/// active; gaps shorter than 0.3 s between active runs are bridged, then runs
/// shorter than 0.15 s are dropped. Each segment's confidence is its mean
/// probability. The result is sorted by start time, then speaker; segments of
/// different speakers may overlap.
pub fn segments_from_probabilities(probs: ArrayView2<f32>) -> Vec<SpeakerSegment> {
    let mut segments = Vec::new();
    for (speaker, channel) in probs.axis_iter(Axis(1)).enumerate() {
        let mut runs: Vec<Range<usize>> = Vec::new();
        let mut run_start = None;
        for (frame, &p) in channel.iter().enumerate() {
            match (p > ACTIVITY_THRESHOLD, run_start) {
                (true, None) => run_start = Some(frame),
                (false, Some(start)) => {
                    push_bridged(&mut runs, start..frame);
                    run_start = None;
                }
                _ => {}
            }
        }
        if let Some(start) = run_start {
            push_bridged(&mut runs, start..channel.len());
        }

        for run in runs {
            if run.len() < MIN_SEGMENT_FRAMES {
                continue;
            }
            let sum: f32 = channel.slice(s![run.clone()]).sum();
            segments.push(SpeakerSegment {
                start: run.start as f64 * FRAME_SECONDS,
                end: run.end as f64 * FRAME_SECONDS,
                speaker,
                confidence: sum / run.len() as f32,
            });
        }
    }
    segments.sort_by(|a, b| {
        a.start
            .total_cmp(&b.start)
            .then_with(|| a.speaker.cmp(&b.speaker))
    });
    segments
}

/// Appends `run`, merging it into the previous run when the gap between them
/// is shorter than `MAX_BRIDGED_GAP_FRAMES`.
fn push_bridged(runs: &mut Vec<Range<usize>>, run: Range<usize>) {
    match runs.last_mut() {
        Some(last) if run.start - last.end < MAX_BRIDGED_GAP_FRAMES => last.end = run.end,
        _ => runs.push(run),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// Probabilities for `frames` frames and `speakers` channels, all 0.0
    /// except the listed `(speaker, frames, value)` spans.
    fn probs(frames: usize, speakers: usize, spans: &[(usize, Range<usize>, f32)]) -> Array2<f32> {
        let mut probs = Array2::zeros((frames, speakers));
        for (speaker, range, value) in spans {
            probs.slice_mut(s![range.clone(), *speaker]).fill(*value);
        }
        probs
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn empty_or_silent_input_has_no_segments() {
        assert!(segments_from_probabilities(Array2::zeros((0, MAX_SPEAKERS)).view()).is_empty());
        assert!(segments_from_probabilities(probs(500, MAX_SPEAKERS, &[]).view()).is_empty());
    }

    #[test]
    fn thresholds_strictly_above_one_half() {
        let at_threshold = probs(200, 2, &[(0, 10..100, 0.5)]);
        assert!(segments_from_probabilities(at_threshold.view()).is_empty());

        let above = probs(200, 2, &[(1, 10..100, 0.51)]);
        let segments = segments_from_probabilities(above.view());
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].speaker, 1);
        assert!(close(segments[0].start, 0.10));
        assert!(close(segments[0].end, 1.00));
    }

    #[test]
    fn run_reaching_the_last_frame_is_closed() {
        let segments = segments_from_probabilities(probs(100, 1, &[(0, 60..100, 0.9)]).view());
        assert_eq!(segments.len(), 1);
        assert!(close(segments[0].start, 0.60));
        assert!(close(segments[0].end, 1.00));
    }

    #[test]
    fn bridges_gaps_shorter_than_the_limit_only() {
        // 29 silent frames between runs: bridged into one segment.
        let short_gap = probs(300, 1, &[(0, 0..50, 0.9), (0, 79..130, 0.9)]);
        let segments = segments_from_probabilities(short_gap.view());
        assert_eq!(segments.len(), 1);
        assert!(close(segments[0].start, 0.0));
        assert!(close(segments[0].end, 1.30));

        // 30 silent frames: kept apart.
        let long_gap = probs(300, 1, &[(0, 0..50, 0.9), (0, 80..130, 0.9)]);
        let segments = segments_from_probabilities(long_gap.view());
        assert_eq!(segments.len(), 2);
        assert!(close(segments[0].end, 0.50));
        assert!(close(segments[1].start, 0.80));
    }

    #[test]
    fn gaps_are_bridged_per_speaker() {
        // Speaker 1 talks in speaker 0's short pause; speaker 0's turn still bridges.
        let spans = [(0, 0..50, 0.9), (1, 50..60, 0.9), (0, 60..120, 0.9)];
        let segments = segments_from_probabilities(probs(200, 2, &spans).view());
        assert_eq!(segments.len(), 1, "speaker 1's 0.1 s blip is dropped");
        assert_eq!(segments[0].speaker, 0);
        assert!(close(segments[0].end, 1.20));
    }

    #[test]
    fn drops_runs_shorter_than_the_minimum_after_bridging() {
        let segments = segments_from_probabilities(probs(300, 1, &[(0, 100..114, 0.9)]).view());
        assert!(segments.is_empty(), "14 frames is below the minimum");

        let segments = segments_from_probabilities(probs(300, 1, &[(0, 100..115, 0.9)]).view());
        assert_eq!(segments.len(), 1, "15 frames is kept");

        // Two 10-frame blips 5 frames apart bridge into a 25-frame run that survives.
        let spans = [(0, 100..110, 0.9), (0, 115..125, 0.9)];
        let segments = segments_from_probabilities(probs(300, 1, &spans).view());
        assert_eq!(segments.len(), 1);
        assert!(close(segments[0].start, 1.00));
        assert!(close(segments[0].end, 1.25));
    }

    #[test]
    fn overlapping_speakers_both_get_segments_sorted_by_start_then_speaker() {
        let spans = [
            (2, 100..300, 0.8),
            (0, 100..200, 0.9),
            (1, 50..150, 0.7),
            (0, 400..500, 0.6),
        ];
        let segments = segments_from_probabilities(probs(600, 3, &spans).view());
        let summary: Vec<(usize, i64, i64)> = segments
            .iter()
            .map(|s| {
                (
                    s.speaker,
                    (s.start * 100.0).round() as i64,
                    (s.end * 100.0).round() as i64,
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![(1, 50, 150), (0, 100, 200), (2, 100, 300), (0, 400, 500)]
        );
    }

    #[test]
    fn confidence_is_the_mean_probability_over_the_run_including_bridged_gaps() {
        let mut p = probs(200, 1, &[(0, 0..20, 0.9), (0, 30..50, 0.7)]);
        // The bridged gap frames keep a sub-threshold probability of 0.2.
        p.slice_mut(s![20..30, 0]).fill(0.2);
        let segments = segments_from_probabilities(p.view());
        assert_eq!(segments.len(), 1);
        let expected = (20.0 * 0.9 + 10.0 * 0.2 + 20.0 * 0.7) / 50.0;
        assert!((segments[0].confidence - expected).abs() < 1e-5);
    }

    #[test]
    fn plans_no_steps_for_an_empty_clip() {
        assert!(plan_steps(0).is_empty());
    }

    #[test]
    fn plans_one_padded_step_for_a_clip_shorter_than_a_chunk() {
        let steps = plan_steps(1);
        assert_eq!(
            steps,
            vec![Step {
                input: 0..1,
                output: 0..1
            }]
        );
        assert_eq!(steps[0].num_input_frames(), 1);

        let steps = plan_steps(CHUNK_FRAMES);
        assert_eq!(
            steps,
            vec![Step {
                input: 0..CHUNK_FRAMES,
                output: 0..CHUNK_FRAMES
            }]
        );
        assert_eq!(steps[0].num_input_frames(), 340);
    }

    #[test]
    fn full_steps_feed_right_context_and_the_last_step_is_padded() {
        // 30 s clip: one full-width step, then a short padded one.
        let steps = plan_steps(3000);
        assert_eq!(
            steps,
            vec![
                Step {
                    input: 0..3000,
                    output: 0..2720
                },
                Step {
                    input: 2720..3000,
                    output: 2720..3000
                },
            ]
        );
        assert_eq!(steps[0].num_input_frames(), 375);
        assert_eq!(steps[1].num_input_frames(), 35);

        // 60 s clip: the first step sees all 3040 frames.
        let steps = plan_steps(6000);
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0].input, 0..STEP_FRAMES);
        assert_eq!(steps[0].num_input_frames(), 380);
        assert_eq!(steps[1].input, 2720..5760);
        assert_eq!(steps[2].input, 5440..6000);
        assert_eq!(steps[2].output, 5440..6000);

        // One frame past a chunk boundary still gets its own step.
        let steps = plan_steps(CHUNK_FRAMES + 1);
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].num_input_frames(), 341);
        assert_eq!(steps[1].input, CHUNK_FRAMES..CHUNK_FRAMES + 1);
        assert_eq!(steps[1].num_input_frames(), 1);
    }

    #[test]
    fn step_outputs_tile_the_clip_exactly() {
        for total in [
            1, 7, 8, 9, 2719, 2720, 2721, 3039, 3040, 3041, 5440, 5441, 100_000,
        ] {
            let steps = plan_steps(total);
            let mut next = 0;
            for (k, step) in steps.iter().enumerate() {
                assert_eq!(step.input.start, k * CHUNK_FRAMES);
                assert_eq!(step.output.start, next, "total {total}");
                assert!(step.output.end <= step.input.end);
                assert!(step.real_frames() <= STEP_FRAMES);
                assert!(step.output.len() <= CHUNK_FRAMES);
                assert!(!step.output.is_empty());
                next = step.output.end;
            }
            assert_eq!(next, total, "total {total}");
        }
    }

    #[test]
    fn check_tensor_accepts_matching_and_dynamic_dims_only() {
        fn accepts(spec: &TensorSpec, ty: TensorElementType, dims: &[i64]) -> bool {
            check_tensor(spec, Some(ty), Some(dims)).is_ok()
        }
        let spec = |name: &str| EXPECTED_INPUTS.iter().find(|s| s.name == name).unwrap();
        let features = spec(INPUT_FEATURES);
        let f32_ty = TensorElementType::Float32;
        assert!(accepts(features, f32_ty, &[1, 3040, 128]));
        assert!(accepts(features, f32_ty, &[-1, -1, 128]));
        assert!(!accepts(features, f32_ty, &[1, 3000, 128]));
        assert!(!accepts(features, f32_ty, &[1, 3040]));
        assert!(!accepts(
            features,
            TensorElementType::Float16,
            &[1, 3040, 128]
        ));
        assert!(check_tensor(features, None, None).is_err());

        // The attention mask is a float mask, not a bool one.
        assert!(!accepts(
            spec(ATTENTION_MASK),
            TensorElementType::Bool,
            &[1, 3040]
        ));

        // Scalars must be rank 0.
        let scalar = spec(NUM_INPUT_FRAMES);
        assert!(accepts(scalar, TensorElementType::Int64, &[]));
        assert!(!accepts(scalar, TensorElementType::Int64, &[1]));
    }

    fn env_path(name: &str) -> std::path::PathBuf {
        std::env::var_os(name)
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| panic!("set {name} to run this test"))
    }

    fn load_fixture() -> Vec<f32> {
        let path = env_path("MEETILY_DIARIZATION_FIXTURE");
        crate::audio::decoder::decode_audio_file(&path)
            .expect("decode fixture")
            .to_whisper_format()
    }

    fn describe(segments: &[SpeakerSegment]) -> String {
        segments
            .iter()
            .map(|s| {
                format!(
                    "  speaker {} {:6.2}-{:6.2} s  conf {:.3}",
                    s.speaker, s.start, s.end, s.confidence
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Needs the model and a 30 s two-speaker fixture whose first speech starts
    /// at 6.69 s:
    /// `MEETILY_SORTFORMER_MODEL=... MEETILY_DIARIZATION_FIXTURE=... cargo test
    /// -p meetily --lib audio::sortformer -- --ignored --nocapture`
    #[test]
    #[ignore = "needs MEETILY_SORTFORMER_MODEL and MEETILY_DIARIZATION_FIXTURE"]
    fn diarizes_two_speaker_fixture() {
        let samples = load_fixture();
        let started = Instant::now();
        let mut diarizer =
            SortformerDiarizer::new(&env_path("MEETILY_SORTFORMER_MODEL")).expect("load model");
        println!("model loaded in {:.2?}", started.elapsed());

        let started = Instant::now();
        let mut progress = Vec::new();
        let segments = diarizer
            .diarize(&samples, |fraction| progress.push(fraction))
            .expect("diarize");
        println!(
            "diarized {:.1} s in {:.2?}; progress {:?}\n{}",
            samples.len() as f64 / 16_000.0,
            started.elapsed(),
            progress,
            describe(&segments)
        );

        assert_eq!(progress.last().copied(), Some(1.0));
        assert!(progress.windows(2).all(|w| w[0] < w[1]));
        let speakers: BTreeSet<usize> = segments.iter().map(|s| s.speaker).collect();
        assert_eq!(speakers.len(), 2, "expected exactly two speakers");
        let first = segments.first().expect("some speech");
        assert_eq!(first.speaker, 0, "speakers are numbered in arrival order");
        assert!(
            (first.start - 6.69).abs() < 0.5,
            "first speech at {:.2} s, expected about 6.69 s",
            first.start
        );
    }

    /// Runs a clip longer than one chunk (the fixture twice over) and a few
    /// shorter than one, to exercise state carrying and the padded last step.
    #[test]
    #[ignore = "needs MEETILY_SORTFORMER_MODEL and MEETILY_DIARIZATION_FIXTURE"]
    fn carries_state_across_steps_and_handles_short_clips() {
        let samples = load_fixture();
        let mut diarizer =
            SortformerDiarizer::new(&env_path("MEETILY_SORTFORMER_MODEL")).expect("load model");

        let doubled = [samples.as_slice(), samples.as_slice()].concat();
        let started = Instant::now();
        let mut steps = 0;
        let probabilities = diarizer
            .speaker_probabilities(&doubled, |_| steps += 1)
            .expect("multi-step run");
        println!(
            "scored {:.1} s in {} steps in {:.2?}",
            doubled.len() as f64 / 16_000.0,
            steps,
            started.elapsed()
        );
        assert_eq!(steps, plan_steps(doubled.len() / mel::HOP_LENGTH).len());
        assert!(steps >= 3);
        assert_eq!(
            probabilities.dim(),
            (doubled.len() / mel::HOP_LENGTH, MAX_SPEAKERS)
        );
        assert!(probabilities.iter().all(|p| (0.0..=1.0).contains(p)));

        let segments = segments_from_probabilities(probabilities.view());
        println!("{}", describe(&segments));
        let speakers: BTreeSet<usize> = segments.iter().map(|s| s.speaker).collect();
        assert_eq!(
            speakers.len(),
            2,
            "the speaker cache should recognise both speakers when they return"
        );
        let second_half: BTreeSet<usize> = segments
            .iter()
            .filter(|s| s.start >= 30.0)
            .map(|s| s.speaker)
            .collect();
        assert_eq!(second_half, speakers);

        for len in [0, 100, 160, 800, 16_000] {
            let probabilities = diarizer
                .speaker_probabilities(&samples[..len], |_| {})
                .unwrap_or_else(|e| panic!("{len} samples: {e:#}"));
            assert_eq!(probabilities.dim(), (len / mel::HOP_LENGTH, MAX_SPEAKERS));
            let segments = diarizer
                .diarize(&samples[..len], |_| {})
                .unwrap_or_else(|e| panic!("{len} samples: {e:#}"));
            assert!(segments.is_empty(), "{len} samples of silence");
        }
    }
}
