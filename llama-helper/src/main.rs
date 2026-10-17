use std::io::{self, BufRead, Write};
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::pin::pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use llama_cpp_2::context::params::{KvCacheType, LlamaContextParams};
use llama_cpp_2::context::session::LlamaStateSeqFlags;
use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::LlamaModel;
use llama_cpp_2::sampling::LlamaSampler;
use llama_cpp_2::token::data::LlamaTokenData;
use llama_cpp_2::token::data_array::LlamaTokenDataArray;
use llama_cpp_2::token::LlamaToken;
use serde::{Deserialize, Serialize};

mod gpu;
mod prefix;

use prefix::{plan_prefix, safe_emit_end, CancelFlag};

// ============================================================================
// Protocol Messages (JSON over stdin/stdout)
// ============================================================================
//
// One request per line in, and exactly one final reply line out per request,
// in order. A streaming `generate` also writes `token` lines before its final
// `response`. `cancel` is the only request that never gets a reply: it is
// handled by the stdin reader thread while a generation runs, and a cancel for
// an id that is not running is ignored.

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Request {
    Generate {
        prompt: String,
        max_tokens: Option<i32>,
        context_size: Option<u32>,
        model_path: Option<String>,
        // Sampling parameters
        temperature: Option<f32>,
        top_k: Option<i32>,
        top_p: Option<f32>,
        presence_penalty: Option<f32>,
        frequency_penalty: Option<f32>,
        repeat_penalty: Option<f32>,
        penalty_last_n: Option<i32>,
        stop_tokens: Option<Vec<String>>,
        /// Sampling seed. Defaults to `DEFAULT_SEED` so identical requests give identical output.
        seed: Option<u32>,
        /// Request id, echoed on every line this request produces. Needed to cancel it.
        id: Option<u64>,
        /// Write `token` lines while generating.
        stream: Option<bool>,
        /// Reuse the saved state of the prompt's stable prefix (see `prefix.rs`).
        cache_prompt: Option<bool>,
        /// Byte length of the prompt's stable prefix.
        cache_prefix_chars: Option<usize>,
        /// Byte length of an earlier, more stable part of that prefix (the
        /// system prompt and live summary), checkpointed too.
        cache_anchor_chars: Option<usize>,
        /// Context window for the persistent cached context (defaults to `context_size`).
        cache_context_size: Option<u32>,
        /// CPU threads for this request. The app lowers it while a recording
        /// runs, so live transcription (on the CPU) keeps up.
        n_threads: Option<i32>,
        /// Flash attention: `None` lets llama.cpp decide (on for Metal).
        flash_attn: Option<bool>,
        /// KV cache element type: "f16" (default), "q8_0" or "q4_0".
        /// Quantized V needs flash attention.
        kv_cache_type: Option<String>,
        /// Physical batch size for prompt evaluation (llama.cpp default 512).
        n_ubatch: Option<u32>,
        /// GBNF grammar the output must follow (root rule `root`).
        grammar: Option<String>,
        /// Stop once the output has this many complete non-empty lines (a
        /// live notes answer: its topic line and the notes the app keeps),
        /// instead of generating lines the app would drop.
        max_lines: Option<u32>,
    },
    /// Counts the tokens `text` produces with the loaded model's tokenizer,
    /// using exactly the tokenization `generate` applies to a prompt.
    Tokenize {
        text: String,
        context_size: Option<u32>,
        model_path: Option<String>,
    },
    /// Stop generation `id` early. Never answered.
    Cancel {
        id: u64,
    },
    Ping,
    Shutdown,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
// The variant name preserves the established `{"type":"response"}` wire protocol.
#[allow(clippy::enum_variant_names)]
enum Response {
    Response {
        text: String,
        error: Option<String>,
        /// "stop" when the model ended its turn, "length" when it hit
        /// max_tokens, "cancelled" when a cancel request stopped it.
        #[serde(skip_serializing_if = "Option::is_none")]
        finish_reason: Option<&'static str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        prompt_tokens: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        completion_tokens: Option<u32>,
        /// Machine-readable error class, e.g. "context_overflow".
        #[serde(skip_serializing_if = "Option::is_none")]
        error_kind: Option<&'static str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<u64>,
        /// Prompt tokens restored from the prefix checkpoint instead of evaluated.
        #[serde(skip_serializing_if = "Option::is_none")]
        reused_tokens: Option<u32>,
        /// Time from receiving the request to the first generated token.
        #[serde(skip_serializing_if = "Option::is_none")]
        first_token_ms: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tokens_per_second: Option<f64>,
    },
    Token {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<u64>,
        text: String,
    },
    TokenCount {
        count: u32,
    },
    Pong,
    Goodbye,
    Error {
        message: String,
    },
}

impl Response {
    fn failure(message: String, error_kind: Option<&'static str>, id: Option<u64>) -> Self {
        Response::Response {
            text: String::new(),
            error: Some(message),
            finish_reason: None,
            prompt_tokens: None,
            completion_tokens: None,
            error_kind,
            id,
            reused_tokens: None,
            first_token_ms: None,
            tokens_per_second: None,
        }
    }

    fn from_generation(
        result: std::result::Result<GenerationOutput, GenerateError>,
        id: Option<u64>,
    ) -> Self {
        match result {
            Ok(output) => Response::Response {
                text: output.text,
                error: None,
                finish_reason: Some(output.finish_reason.as_str()),
                prompt_tokens: Some(output.prompt_tokens),
                completion_tokens: Some(output.completion_tokens),
                error_kind: None,
                id,
                reused_tokens: output.reused_tokens,
                first_token_ms: output.first_token_ms,
                tokens_per_second: Some(output.tokens_per_second),
            },
            Err(error) => {
                let kind = matches!(error, GenerateError::ContextOverflow { .. })
                    .then_some(ERROR_KIND_CONTEXT_OVERFLOW);
                Response::failure(format!("Generation failed: {error}"), kind, id)
            }
        }
    }
}

/// Fixed default seed: summaries must be reproducible for the same input.
const DEFAULT_SEED: u32 = 42;

const ERROR_KIND_CONTEXT_OVERFLOW: &str = "context_overflow";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FinishReason {
    Stop,
    Length,
    Cancelled,
}

impl FinishReason {
    fn as_str(self) -> &'static str {
        match self {
            FinishReason::Stop => "stop",
            FinishReason::Length => "length",
            FinishReason::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct GenerationOutput {
    text: String,
    finish_reason: FinishReason,
    prompt_tokens: u32,
    completion_tokens: u32,
    /// Only reported for cached requests.
    reused_tokens: Option<u32>,
    first_token_ms: Option<u64>,
    tokens_per_second: f64,
}

#[derive(Debug)]
enum GenerateError {
    /// The prompt plus the requested output does not fit in the context window.
    ContextOverflow {
        prompt_tokens: usize,
        max_tokens: i32,
        context_size: u32,
    },
    Other(anyhow::Error),
}

impl std::fmt::Display for GenerateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GenerateError::ContextOverflow {
                prompt_tokens,
                max_tokens,
                context_size,
            } => write!(
                f,
                "context overflow: prompt has {prompt_tokens} tokens and max_tokens is {max_tokens}, which exceeds the context window of {context_size} tokens"
            ),
            GenerateError::Other(error) => write!(f, "{error:#}"),
        }
    }
}

impl From<anyhow::Error> for GenerateError {
    fn from(error: anyhow::Error) -> Self {
        GenerateError::Other(error)
    }
}

/// Rejects a request whose prompt plus output budget cannot fit in the context,
/// instead of letting llama_decode fail part-way through.
fn check_context_budget(
    prompt_tokens: usize,
    max_tokens: i32,
    context_size: u32,
) -> std::result::Result<(), GenerateError> {
    if prompt_tokens == 0 {
        return Err(GenerateError::Other(anyhow::anyhow!("prompt is empty")));
    }
    let needed = prompt_tokens as u64 + max_tokens.max(0) as u64;
    if needed > u64::from(context_size) {
        return Err(GenerateError::ContextOverflow {
            prompt_tokens,
            max_tokens,
            context_size,
        });
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct SamplingConfig {
    temperature: f32,
    top_k: i32,
    top_p: f32,
    presence_penalty: f32,
    frequency_penalty: f32,
    repeat_penalty: f32,
    penalty_last_n: i32,
}

impl SamplingConfig {
    fn from_request(
        temperature: Option<f32>,
        top_k: Option<i32>,
        top_p: Option<f32>,
        presence_penalty: Option<f32>,
        frequency_penalty: Option<f32>,
        repeat_penalty: Option<f32>,
        penalty_last_n: Option<i32>,
    ) -> Self {
        let temperature = temperature.unwrap_or(1.0);
        let temperature = if temperature.is_finite() {
            temperature.max(0.0)
        } else {
            0.0
        };
        let top_k = top_k.unwrap_or(64).max(1);
        let top_p = top_p.unwrap_or(0.95);
        let top_p = if top_p.is_finite() && top_p > 0.0 && top_p <= 1.0 {
            top_p
        } else {
            1.0
        };
        let presence_penalty = presence_penalty.unwrap_or(0.0);
        let presence_penalty = if presence_penalty.is_finite() {
            presence_penalty.max(0.0)
        } else {
            0.0
        };
        let frequency_penalty = frequency_penalty.unwrap_or(0.0);
        let frequency_penalty = if frequency_penalty.is_finite() {
            frequency_penalty.max(0.0)
        } else {
            0.0
        };
        let repeat_penalty = repeat_penalty.unwrap_or(1.0);
        let repeat_penalty = if repeat_penalty.is_finite() && repeat_penalty > 0.0 {
            repeat_penalty
        } else {
            1.0
        };
        let penalty_last_n = penalty_last_n.unwrap_or(0).max(0);

        Self {
            temperature,
            top_k,
            top_p,
            presence_penalty,
            frequency_penalty,
            repeat_penalty,
            penalty_last_n,
        }
    }

    fn uses_penalties(&self) -> bool {
        self.penalty_last_n > 0
            && (self.presence_penalty > 0.0
                || self.frequency_penalty > 0.0
                || (self.repeat_penalty - 1.0).abs() > f32::EPSILON)
    }
}

/// How a context is built, beyond its size and threads. Part of the cached
/// context's key: a request with other options gets a new context.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ContextOptions {
    flash_attn: Option<bool>,
    kv_cache_type: Option<KvType>,
    n_ubatch: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KvType {
    F16,
    Q8_0,
    Q4_0,
}

impl KvType {
    fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "f16" => Some(Self::F16),
            "q8_0" | "q8" => Some(Self::Q8_0),
            "q4_0" | "q4" => Some(Self::Q4_0),
            _ => None,
        }
    }

    fn cache_type(self) -> KvCacheType {
        match self {
            Self::F16 => KvCacheType::F16,
            Self::Q8_0 => KvCacheType::Q8_0,
            Self::Q4_0 => KvCacheType::Q4_0,
        }
    }
}

impl ContextOptions {
    fn apply(self, mut params: LlamaContextParams) -> LlamaContextParams {
        if let Some(enabled) = self.flash_attn {
            params = params.with_flash_attention_policy(if enabled {
                llama_cpp_sys_2::LLAMA_FLASH_ATTN_TYPE_ENABLED
            } else {
                llama_cpp_sys_2::LLAMA_FLASH_ATTN_TYPE_DISABLED
            });
        }
        if let Some(kv) = self.kv_cache_type {
            params = params
                .with_type_k(kv.cache_type())
                .with_type_v(kv.cache_type());
        }
        if let Some(n_ubatch) = self.n_ubatch.filter(|n| *n > 0) {
            params = params.with_n_ubatch(n_ubatch);
        }
        params
    }
}

/// One generation's parameters.
struct GenerateParams {
    prompt: String,
    max_tokens: i32,
    sampling: SamplingConfig,
    stop_tokens: Vec<String>,
    seed: u32,
    id: Option<u64>,
    /// Set for `cache_prompt` requests.
    cache: Option<CacheParams>,
    threads: i32,
    context: ContextOptions,
    grammar: Option<String>,
    max_lines: Option<u32>,
}

#[derive(Debug, Clone, Copy)]
struct CacheParams {
    prefix_chars: usize,
    /// An earlier, more stable boundary inside the prefix.
    anchor_chars: Option<usize>,
    context_size: u32,
}

/// Prefix states kept in host memory (each is roughly 70 MB per 1k tokens
/// on Qwen 3.5 4B): the anchor and the stable prefix, plus one spare.
const MAX_CHECKPOINTS: usize = 3;
/// On machines with 8 GB of RAM or less: one checkpoint and a smaller
/// cached context, so Copilot's resident memory stays small next to the model
/// and live transcription.
const LOW_MEMORY_CHECKPOINTS: usize = 1;
const LOW_MEMORY_CACHE_CONTEXT: u32 = 4096;
const LOW_MEMORY_BYTES: u64 = 8 * 1024 * 1024 * 1024;

/// Total physical memory, from macOS's `hw.memsize`.
fn total_memory_bytes() -> Option<u64> {
    let output = std::process::Command::new("sysctl")
        .args(["-n", "hw.memsize"])
        .output()
        .ok()?;
    String::from_utf8(output.stdout).ok()?.trim().parse().ok()
}

/// Checkpoint count and cached-context cap for this machine.
fn cache_limits(total_memory: Option<u64>) -> (usize, Option<u32>) {
    match total_memory {
        Some(bytes) if bytes <= LOW_MEMORY_BYTES => {
            (LOW_MEMORY_CHECKPOINTS, Some(LOW_MEMORY_CACHE_CONTEXT))
        }
        _ => (MAX_CHECKPOINTS, None),
    }
}

/// What generation reports while it runs.
trait GenerationSink {
    fn is_cancelled(&self) -> bool;
    fn on_text(&mut self, text: &str);
}

// ============================================================================
// Model State Management
// ============================================================================

/// The saved state of a prompt's stable prefix, in host memory, so it outlives
/// the context it came from.
struct Checkpoint {
    tokens: Vec<LlamaToken>,
    state: Vec<u8>,
}

struct ModelState {
    // Field order is drop order: the cached context borrows the model, so it
    // must go first.
    /// Persistent context for `cache_prompt` requests, with its n_ctx and threads.
    cache_ctx: Option<((u32, i32, ContextOptions), LlamaContext<'static>)>,
    /// Saved prefix states, most recently used first (at most `max_checkpoints`).
    checkpoints: Vec<Checkpoint>,
    max_checkpoints: usize,
    /// Smaller cached context on low-memory machines.
    cache_context_cap: Option<u32>,
    /// Boxed so its address stays put while `cache_ctx` borrows it.
    model: Option<Box<LlamaModel>>,
    model_path: Option<PathBuf>,
    context_size: u32,
    last_activity: Arc<AtomicU64>,
    backend: LlamaBackend,
}

fn thread_count() -> i32 {
    // Conservative default: max(1, (Cores / 2) + 2), so the UI thread is never starved.
    std::thread::available_parallelism()
        .map(|n| {
            let cores = n.get() as i32;
            ((cores / 2) + 2).max(1)
        })
        .unwrap_or(2)
}

impl ModelState {
    fn new() -> Result<Self> {
        let backend = LlamaBackend::init().context("Failed to init LlamaBackend")?;
        let (max_checkpoints, cache_context_cap) = cache_limits(total_memory_bytes());
        if cache_context_cap.is_some() {
            eprintln!("💡 8 GB machine: one prefix checkpoint, smaller cached context");
        }
        Ok(Self {
            cache_ctx: None,
            checkpoints: Vec::new(),
            max_checkpoints,
            cache_context_cap,
            model: None,
            model_path: None,
            context_size: 2048,
            last_activity: Arc::new(AtomicU64::new(Self::current_timestamp())),
            backend,
        })
    }

    fn current_timestamp() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    fn update_activity(&self) {
        self.last_activity
            .store(Self::current_timestamp(), Ordering::SeqCst);
    }

    fn seconds_since_activity(&self) -> u64 {
        Self::current_timestamp().saturating_sub(self.last_activity.load(Ordering::SeqCst))
    }

    fn load_model_if_needed(&mut self, model_path: PathBuf, context_size: u32) -> Result<()> {
        // Check if model is already loaded
        if let Some(ref loaded_path) = self.model_path {
            if loaded_path == &model_path && self.context_size == context_size {
                eprintln!("✓ Model already loaded");
                self.update_activity();
                return Ok(());
            }
        }

        eprintln!("📥 Loading model: {}", model_path.display());
        // Anything tied to the old model goes before it does.
        self.cache_ctx = None;
        self.checkpoints.clear();
        self.model = None;
        self.model_path = None;

        // Detect GPU layers
        let gpu_layers = gpu::get_default_gpu_layers(&model_path, context_size);

        // Configure model parameters with GPU offload
        let model_params = LlamaModelParams::default().with_n_gpu_layers(gpu_layers);
        let model_params = pin!(model_params);

        let model = LlamaModel::load_from_file(&self.backend, model_path.clone(), &model_params)
            .with_context(|| format!("unable to load model at {:?}", model_path))?;

        self.model = Some(Box::new(model));
        self.model_path = Some(model_path);
        self.context_size = context_size;
        self.update_activity();

        eprintln!("✅ Model loaded successfully");
        Ok(())
    }

    /// Tokenizes `text` exactly as `generate` tokenizes a prompt. `add_special`
    /// maps to llama.cpp's `add_special`, which only prepends BOS when the GGUF's
    /// `add_bos_token` says so (Gemma: yes, Qwen: no), so templates never double it.
    fn tokenize(&self, text: &str) -> Result<Vec<LlamaToken>> {
        let model = self.model.as_deref().context("Model not loaded")?;
        // `add_special` only prepends BOS when the GGUF's `add_bos_token`
        // says so; `parse_special` reads the template's control tokens.
        Ok(model.vocab().tokenize(text.as_bytes(), true, true))
    }

    /// The loaded model with the lifetime the cached context needs.
    ///
    /// SAFETY: the model is boxed (its address never moves) and is only
    /// dropped in `load_model_if_needed` or with `ModelState`, both of which
    /// drop `cache_ctx` first.
    fn model_static(&self) -> Result<&'static LlamaModel> {
        let model = self.model.as_deref().context("Model not loaded")?;
        Ok(unsafe { &*(model as *const LlamaModel) })
    }

    fn generate(
        &mut self,
        params: GenerateParams,
        sink: &mut dyn GenerationSink,
    ) -> std::result::Result<GenerationOutput, GenerateError> {
        match params.cache {
            Some(cache) => self.generate_cached(params, cache, sink),
            None => self.generate_fresh(params, sink),
        }
    }

    /// The original path: a fresh context per request.
    fn generate_fresh(
        &mut self,
        params: GenerateParams,
        sink: &mut dyn GenerationSink,
    ) -> std::result::Result<GenerationOutput, GenerateError> {
        let start_time = Instant::now();
        let tokens_list = self.tokenize(&params.prompt)?;
        eprintln!("📝 Tokenized prompt: {} tokens", tokens_list.len());
        check_context_budget(tokens_list.len(), params.max_tokens, self.context_size)?;

        let model = self.model.as_deref().context("Model not loaded")?;
        let threads = params.threads;
        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(Some(
                NonZeroU32::new(self.context_size).context("Invalid ctx size")?,
            ))
            .with_n_batch(self.context_size)
            .with_n_threads(threads)
            .with_n_threads_batch(thread_count());
        let ctx_params = params.context.apply(ctx_params);

        let mut ctx = model
            .new_context(&self.backend, ctx_params)
            .context("unable to create the llama_context")?;

        // Use context size for batch capacity to handle long prompts
        let mut batch = LlamaBatch::new(self.context_size as usize, 1);
        let last_index: i32 = (tokens_list.len() - 1) as i32;
        for (i, token) in (0_i32..).zip(tokens_list.iter().copied()) {
            batch
                .add(token, i, &[0], i == last_index)
                .context("Failed to add token to batch")?;
        }
        ctx.decode(&mut batch).context("llama_decode() failed")?;
        let prompt_time = start_time.elapsed();

        let n_prompt_tokens = batch.n_tokens();
        let output = run_sampling(
            &mut ctx,
            model,
            &mut batch,
            n_prompt_tokens,
            &params,
            sink,
            start_time,
        )?;
        self.update_activity();
        Ok(finish_output(
            output,
            n_prompt_tokens as u32,
            None,
            start_time,
            prompt_time,
        ))
    }

    /// A `cache_prompt` request: restore the stable prefix's checkpoint when it
    /// still matches, evaluate only what is new, and save the new prefix.
    fn generate_cached(
        &mut self,
        params: GenerateParams,
        cache: CacheParams,
        sink: &mut dyn GenerationSink,
    ) -> std::result::Result<GenerationOutput, GenerateError> {
        let start_time = Instant::now();
        let mut n_ctx = cache.context_size.clamp(512, self.context_size.max(512));
        if let Some(cap) = self.cache_context_cap {
            n_ctx = n_ctx.min(cap);
        }
        let tokens = self.tokenize(&params.prompt)?;
        check_context_budget(tokens.len(), params.max_tokens, n_ctx)?;
        // The stable prefix and, when given, an earlier anchor inside it.
        let mut boundaries = Vec::new();
        for chars in [cache.anchor_chars, Some(cache.prefix_chars)]
            .into_iter()
            .flatten()
        {
            let end = floor_char_boundary(&params.prompt, chars);
            if end > 0 {
                boundaries.push(self.tokenize(&params.prompt[..end])?);
            }
        }
        let boundary_refs: Vec<&[LlamaToken]> = boundaries.iter().map(Vec::as_slice).collect();
        let model = self.model_static()?;

        let threads = params.threads;
        let key = (n_ctx, threads, params.context);
        let mut ctx = match self.cache_ctx.take() {
            Some((cached, ctx)) if cached == key => ctx,
            _ => {
                let ctx_params = LlamaContextParams::default()
                    .with_n_ctx(Some(NonZeroU32::new(n_ctx).context("Invalid ctx size")?))
                    .with_n_batch(n_ctx)
                    .with_n_threads(threads)
                    // Prompt evaluation keeps every thread: on hybrid models
                    // part of it runs on the CPU, and two threads made a cold
                    // prompt ten times slower.
                    .with_n_threads_batch(thread_count());
                let ctx_params = params.context.apply(ctx_params);
                model
                    .new_context(&self.backend, ctx_params)
                    .context("unable to create the cached llama_context")?
            }
        };
        ctx.clear_kv_cache();

        let saved: Vec<&[LlamaToken]> = self
            .checkpoints
            .iter()
            .map(|c| c.tokens.as_slice())
            .collect();
        let mut plan = plan_prefix(&tokens, &boundary_refs, &saved);
        if let Some(index) = plan.restore {
            // SAFETY: the bytes came from `state_seq_get_data_ext` on a context
            // of this same model.
            let restored = unsafe {
                ctx.state_seq_set_data_ext(
                    &self.checkpoints[index].state,
                    0,
                    LlamaStateSeqFlags::empty(),
                )
            };
            if restored {
                // Most recently used first.
                let used = self.checkpoints.remove(index);
                self.checkpoints.insert(0, used);
            } else {
                eprintln!("⚠️ Prefix checkpoint could not be restored; evaluating in full");
                ctx.clear_kv_cache();
                self.checkpoints.clear();
                plan = plan_prefix(&tokens, &boundary_refs, &[]);
            }
        }
        let reused = plan.start;

        let mut batch = LlamaBatch::new(n_ctx as usize, 1);
        let mut position = plan.start;
        for end in plan.save_at.clone() {
            decode_range(&mut ctx, &mut batch, &tokens, position, end, false)?;
            position = end;
            if let Some(checkpoint) = save_checkpoint(&ctx, &tokens[..end]) {
                self.checkpoints.retain(|c| c.tokens != checkpoint.tokens);
                self.checkpoints.insert(0, checkpoint);
                self.checkpoints.truncate(self.max_checkpoints);
            }
        }
        if sink.is_cancelled() {
            self.cache_ctx = Some((key, ctx));
            return Ok(cancelled_output(tokens.len(), reused));
        }
        decode_range(&mut ctx, &mut batch, &tokens, position, tokens.len(), true)?;
        let prompt_time = start_time.elapsed();
        eprintln!(
            "📝 Cached prompt: {} tokens, {} reused, {} evaluated in {:.2}s",
            tokens.len(),
            reused,
            tokens.len() - reused,
            prompt_time.as_secs_f64()
        );

        let output = run_sampling(
            &mut ctx,
            model,
            &mut batch,
            tokens.len() as i32,
            &params,
            sink,
            start_time,
        );
        self.cache_ctx = Some((key, ctx));
        let output = output?;
        self.update_activity();
        Ok(finish_output(
            output,
            tokens.len() as u32,
            Some(reused as u32),
            start_time,
            prompt_time,
        ))
    }
}

fn floor_char_boundary(text: &str, index: usize) -> usize {
    let mut index = index.min(text.len());
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// Evaluates `tokens[from..to]` at their own positions; only the last one gets
/// logits, and only when `logits_at_end`.
fn decode_range(
    ctx: &mut LlamaContext<'_>,
    batch: &mut LlamaBatch,
    tokens: &[LlamaToken],
    from: usize,
    to: usize,
    logits_at_end: bool,
) -> Result<()> {
    if from >= to {
        return Ok(());
    }
    batch.clear();
    for (position, token) in tokens.iter().enumerate().take(to).skip(from) {
        batch
            .add(
                *token,
                position as i32,
                &[0],
                logits_at_end && position + 1 == to,
            )
            .context("Failed to add token to batch")?;
    }
    ctx.decode(batch).context("llama_decode() failed")?;
    Ok(())
}

fn save_checkpoint(ctx: &LlamaContext<'_>, tokens: &[LlamaToken]) -> Option<Checkpoint> {
    let flags = LlamaStateSeqFlags::empty();
    let size = ctx.state_seq_get_size_ext(0, flags);
    if size == 0 {
        return None;
    }
    let mut state = vec![0_u8; size];
    // SAFETY: `state` holds `size` bytes, the size llama.cpp asked for.
    let written = unsafe { ctx.state_seq_get_data_ext(state.as_mut_ptr(), 0, flags) };
    if written == 0 || written > size {
        eprintln!("⚠️ Could not save the prefix checkpoint");
        return None;
    }
    state.truncate(written);
    eprintln!(
        "💾 Prefix checkpoint: {} tokens, {:.1} MB",
        tokens.len(),
        written as f64 / (1024.0 * 1024.0)
    );
    Some(Checkpoint {
        tokens: tokens.to_vec(),
        state,
    })
}

fn cancelled_output(prompt_tokens: usize, reused: usize) -> GenerationOutput {
    GenerationOutput {
        text: String::new(),
        finish_reason: FinishReason::Cancelled,
        prompt_tokens: prompt_tokens as u32,
        completion_tokens: 0,
        reused_tokens: Some(reused as u32),
        first_token_ms: None,
        tokens_per_second: 0.0,
    }
}

struct SampledOutput {
    text: String,
    finish_reason: FinishReason,
    completion_tokens: u32,
    first_token_at: Option<Duration>,
}

fn finish_output(
    output: SampledOutput,
    prompt_tokens: u32,
    reused_tokens: Option<u32>,
    start_time: Instant,
    prompt_time: Duration,
) -> GenerationOutput {
    let total_time = start_time.elapsed();
    let gen_time = total_time.saturating_sub(prompt_time);
    let tokens_per_second = if gen_time.as_secs_f64() > 0.0 {
        f64::from(output.completion_tokens) / gen_time.as_secs_f64()
    } else {
        0.0
    };

    eprintln!("📊 Generation Statistics:");
    eprintln!("   • Prompt tokens: {}", prompt_tokens);
    if let Some(reused) = reused_tokens {
        eprintln!("   • Reused prompt tokens: {}", reused);
    }
    eprintln!("   • Output tokens: {}", output.completion_tokens);
    eprintln!("   • Prompt processing: {:.2}s", prompt_time.as_secs_f64());
    eprintln!("   • Generation time: {:.2}s", gen_time.as_secs_f64());
    eprintln!("   • Total time: {:.2}s", total_time.as_secs_f64());
    eprintln!("   • Speed: {:.2} tokens/sec", tokens_per_second);

    GenerationOutput {
        text: output.text,
        finish_reason: output.finish_reason,
        prompt_tokens,
        completion_tokens: output.completion_tokens,
        reused_tokens,
        first_token_ms: output
            .first_token_at
            .map(|at| u64::try_from(at.as_millis()).unwrap_or(u64::MAX)),
        tokens_per_second,
    }
}

/// Samples up to `max_tokens` after the prompt. The batch's last token must
/// hold logits.
fn run_sampling(
    ctx: &mut LlamaContext<'_>,
    model: &LlamaModel,
    batch: &mut LlamaBatch,
    n_prompt_tokens: i32,
    params: &GenerateParams,
    sink: &mut dyn GenerationSink,
    start_time: Instant,
) -> Result<SampledOutput> {
    let sampling = params.sampling;
    let max_tokens = params.max_tokens;
    let seed = params.seed;
    let mut n_cur = n_prompt_tokens;
    let mut decoder = encoding_rs::UTF_8.new_decoder();
    let mut output = String::new();
    let mut emitted = 0_usize;
    let mut first_token_at = None;

    eprintln!(
        "🔄 Starting generation (max_tokens: {}, seed: {})",
        max_tokens, seed
    );

    let sampler = if sampling.temperature <= 0.0 {
        if sampling.uses_penalties() {
            LlamaSampler::chain_simple([
                LlamaSampler::penalties(
                    model.vocab().n_tokens(),
                    sampling.penalty_last_n,
                    sampling.repeat_penalty,
                    sampling.frequency_penalty,
                    sampling.presence_penalty,
                ),
                LlamaSampler::greedy(),
            ])
        } else {
            LlamaSampler::chain_simple([LlamaSampler::greedy()])
        }
    } else if sampling.uses_penalties() {
        LlamaSampler::chain_simple([
            LlamaSampler::penalties(
                model.vocab().n_tokens(),
                sampling.penalty_last_n,
                sampling.repeat_penalty,
                sampling.frequency_penalty,
                sampling.presence_penalty,
            ),
            LlamaSampler::top_k(sampling.top_k),
            LlamaSampler::top_p(sampling.top_p, 1),
            LlamaSampler::temp(sampling.temperature),
            LlamaSampler::dist(seed),
        ])
    } else {
        LlamaSampler::chain_simple([
            LlamaSampler::top_k(sampling.top_k),
            LlamaSampler::top_p(sampling.top_p, 1),
            LlamaSampler::temp(sampling.temperature),
            LlamaSampler::dist(seed),
        ])
    };
    let mut grammar = match params.grammar.as_deref().filter(|g| !g.trim().is_empty()) {
        Some(grammar) => {
            Some(LlamaSampler::grammar(model, grammar, "root").context("invalid grammar")?)
        }
        None => None,
    };
    let mut sampler = pin!(sampler);
    let mut finish_reason = FinishReason::Stop;

    loop {
        if sink.is_cancelled() {
            eprintln!("✋ Generation cancelled");
            finish_reason = FinishReason::Cancelled;
            break;
        }
        // Check if we've generated enough tokens
        if (n_cur - n_prompt_tokens) >= max_tokens {
            eprintln!("⚠️ Reached max_tokens limit; output is truncated");
            finish_reason = FinishReason::Length;
            break;
        }

        let index = batch.n_tokens() - 1;
        let token = match grammar.as_mut() {
            // `sample` also accepts the token into every sampler of the chain
            // (llama_sampler_sample does): accepting it again counted each
            // token twice in the repeat penalty.
            None => sampler.as_mut().sample(ctx, index),
            Some(grammar) => sample_with_grammar(ctx, index, &mut sampler, grammar)?,
        };
        if first_token_at.is_none() {
            first_token_at = Some(start_time.elapsed());
        }

        if model.vocab().is_eog(token) {
            eprintln!(
                "✓ End-of-generation token reached (generated {} chars)",
                output.len()
            );
            break;
        }

        let output_bytes = model.vocab().token_to_piece(token, true, None);

        let mut token_text = String::with_capacity(32);
        let _ = decoder.decode_to_string(&output_bytes, &mut token_text, false);
        output.push_str(&token_text);

        // Check for model-specific stop tokens
        let mut should_stop = false;
        for stop_token in &params.stop_tokens {
            if output.contains(stop_token) {
                eprintln!(
                    "✓ Stop token '{}' detected (generated {} chars)",
                    stop_token,
                    output.len()
                );
                // Remove the stop token from output
                output = output.replace(stop_token, "").trim_end().to_string();
                should_stop = true;
                break;
            }
        }
        if should_stop {
            break;
        }
        if params
            .max_lines
            .is_some_and(|max| complete_lines(&output) >= max as usize)
        {
            eprintln!("✓ Line limit reached (generated {} chars)", output.len());
            output.truncate(output.trim_end().len());
            break;
        }

        let safe_end = safe_emit_end(&output, &params.stop_tokens);
        if safe_end > emitted {
            sink.on_text(&output[emitted..safe_end]);
            emitted = safe_end;
        }

        batch.clear();
        batch
            .add(token, n_cur, &[0], true)
            .context("Failed to add generated token to batch")?;
        n_cur += 1;
        ctx.decode(batch).context("failed to eval")?;
    }

    if output.len() > emitted && output.is_char_boundary(emitted) {
        sink.on_text(&output[emitted..]);
    }

    Ok(SampledOutput {
        text: output,
        finish_reason,
        completion_tokens: u32::try_from(n_cur - n_prompt_tokens).unwrap_or(0),
        first_token_at,
    })
}

/// Complete (newline-terminated) lines of `output` with text on them.
fn complete_lines(output: &str) -> usize {
    match output.rsplit_once('\n') {
        Some((done, _)) => done.lines().filter(|line| !line.trim().is_empty()).count(),
        None => 0,
    }
}

/// Samples one token that `grammar` allows, the way llama.cpp's common
/// sampler does: sample freely, and only when the grammar rejects that token
/// run the grammar over the whole vocabulary (slow on 250k-token vocabularies)
/// and sample again. Accepts the token into both samplers.
fn sample_with_grammar(
    ctx: &LlamaContext<'_>,
    index: i32,
    sampler: &mut LlamaSampler,
    grammar: &mut LlamaSampler,
) -> Result<LlamaToken> {
    let mut candidates = ctx.token_data_array_ith(index);
    candidates.apply_sampler(sampler);
    let mut token = candidates
        .selected_token()
        .context("the sampler selected no token")?;
    let mut single = LlamaTokenDataArray::new(vec![LlamaTokenData::new(token, 1.0, 0.0)], false);
    single.apply_sampler(grammar);
    if !single.data[0].logit().is_finite() {
        let mut candidates = ctx.token_data_array_ith(index);
        candidates.apply_sampler(grammar);
        candidates.apply_sampler(sampler);
        token = candidates
            .selected_token()
            .context("the grammar allows no token")?;
    }
    grammar.accept(token);
    sampler.accept(token);
    Ok(token)
}

// ============================================================================
// Main Loop with Keep-Alive Protocol
// ============================================================================

fn send_response(response: &Response) -> Result<()> {
    let json = serde_json::to_string(response)?;
    let stdout = io::stdout();
    let mut out = stdout.lock();
    writeln!(out, "{}", json)?;
    out.flush()?;
    Ok(())
}

/// Streams token lines for one request and watches its cancel flag.
struct StdoutSink<'a> {
    id: Option<u64>,
    stream: bool,
    cancel: &'a CancelFlag,
}

impl GenerationSink for StdoutSink<'_> {
    fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled(self.id)
    }

    fn on_text(&mut self, text: &str) {
        if self.stream && !text.is_empty() {
            let _ = send_response(&Response::Token {
                id: self.id,
                text: text.to_string(),
            });
        }
    }
}

/// Reads stdin on its own thread so a `cancel` can arrive mid-generation.
/// Every other line is passed to the main loop in order.
fn spawn_stdin_reader(
    cancel: Arc<CancelFlag>,
) -> mpsc::Receiver<std::result::Result<Request, String>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let stdin = io::stdin();
        for line in stdin.lock().lines() {
            let Ok(line) = line else {
                eprintln!("❌ Error reading stdin");
                break;
            };
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let message = match serde_json::from_str::<Request>(line) {
                Ok(Request::Cancel { id }) => {
                    cancel.cancel(id);
                    continue;
                }
                Ok(request) => Ok(request),
                Err(e) => Err(e.to_string()),
            };
            if sender.send(message).is_err() {
                break;
            }
        }
        // Dropping the sender tells the main loop stdin reached EOF.
    });
    receiver
}

fn main() -> Result<()> {
    // Get idle timeout from environment variable (default 5 minutes)
    let idle_timeout_secs = std::env::var("LLAMA_IDLE_TIMEOUT")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(300); // 5 minutes default

    eprintln!(
        "🦙 llama-helper starting (idle timeout: {}s)",
        idle_timeout_secs
    );

    let mut state = ModelState::new()?;
    let cancel = Arc::new(CancelFlag::default());
    let requests = spawn_stdin_reader(cancel.clone());

    loop {
        // Check idle timeout
        if state.seconds_since_activity() > idle_timeout_secs {
            eprintln!("💤 Idle timeout reached, shutting down");
            send_response(&Response::Goodbye)?;
            break;
        }

        let Ok(message) = requests.recv() else {
            eprintln!("📪 EOF received, shutting down");
            break;
        };

        match message {
            Ok(Request::Generate {
                prompt,
                max_tokens,
                context_size,
                model_path,
                temperature,
                top_k,
                top_p,
                presence_penalty,
                frequency_penalty,
                repeat_penalty,
                penalty_last_n,
                stop_tokens,
                seed,
                id,
                stream,
                cache_prompt,
                cache_prefix_chars,
                cache_anchor_chars,
                cache_context_size,
                n_threads,
                flash_attn,
                kv_cache_type,
                n_ubatch,
                grammar,
                max_lines,
            }) => {
                let max_tokens = max_tokens.unwrap_or(512);
                let context_size = context_size.unwrap_or(2048);
                let seed = seed.unwrap_or(DEFAULT_SEED);

                let sampling = SamplingConfig::from_request(
                    temperature,
                    top_k,
                    top_p,
                    presence_penalty,
                    frequency_penalty,
                    repeat_penalty,
                    penalty_last_n,
                );
                let stop_tokens = stop_tokens.unwrap_or_default();

                // Load model if path provided
                if let Some(path_str) = model_path {
                    let path = PathBuf::from(path_str);
                    if let Err(e) = state.load_model_if_needed(path, context_size) {
                        send_response(&Response::failure(
                            format!("Failed to load model: {}", e),
                            None,
                            id,
                        ))?;
                        continue;
                    }
                }

                let cache = cache_prompt.unwrap_or(false).then(|| CacheParams {
                    prefix_chars: cache_prefix_chars.unwrap_or(0),
                    anchor_chars: cache_anchor_chars,
                    context_size: cache_context_size.unwrap_or(context_size),
                });
                let params = GenerateParams {
                    prompt,
                    max_tokens,
                    sampling,
                    stop_tokens,
                    seed,
                    id,
                    cache,
                    threads: n_threads
                        .filter(|n| *n > 0)
                        .map_or_else(thread_count, |n| n.min(thread_count())),
                    context: ContextOptions {
                        flash_attn,
                        kv_cache_type: kv_cache_type.as_deref().and_then(KvType::parse),
                        n_ubatch,
                    },
                    grammar,
                    max_lines: max_lines.filter(|n| *n > 0),
                };
                let mut sink = StdoutSink {
                    id: params.id,
                    stream: stream.unwrap_or(false),
                    cancel: &cancel,
                };
                let result = state.generate(params, &mut sink);
                cancel.finished(id);
                send_response(&Response::from_generation(result, id))?;
            }
            Ok(Request::Tokenize {
                text,
                context_size,
                model_path,
            }) => {
                if let Some(path_str) = model_path {
                    let context_size = context_size.unwrap_or(state.context_size);
                    if let Err(e) =
                        state.load_model_if_needed(PathBuf::from(path_str), context_size)
                    {
                        send_response(&Response::Error {
                            message: format!("Failed to load model: {}", e),
                        })?;
                        continue;
                    }
                }
                match state.tokenize(&text) {
                    Ok(tokens) => {
                        state.update_activity();
                        send_response(&Response::TokenCount {
                            count: u32::try_from(tokens.len()).unwrap_or(u32::MAX),
                        })?;
                    }
                    Err(e) => send_response(&Response::Error {
                        message: format!("Tokenization failed: {:#}", e),
                    })?,
                }
            }
            // Handled by the reader thread; never reaches here.
            Ok(Request::Cancel { .. }) => {}
            Ok(Request::Ping) => {
                state.update_activity();
                send_response(&Response::Pong)?;
            }
            Ok(Request::Shutdown) => {
                eprintln!("🛑 Shutdown requested");
                send_response(&Response::Goodbye)?;
                break;
            }
            Err(e) => {
                eprintln!("❌ Failed to parse request: {}", e);
                send_response(&Response::Error {
                    message: format!("Invalid request: {}", e),
                })?;
            }
        }
    }

    eprintln!("👋 llama-helper exiting");
    Ok(())
}

#[cfg(test)]
mod tests;
