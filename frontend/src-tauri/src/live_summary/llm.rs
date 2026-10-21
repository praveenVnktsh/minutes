//! The model call for a live summary window: the user's summary provider and
//! model, through the shared summary entry point (`generate_summary`), with a
//! short output cap, a low temperature, the fixed seed and a timeout of our
//! own. The same options reach every provider.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::anyhow;
use async_trait::async_trait;
use sqlx::SqlitePool;
use tokio_util::sync::CancellationToken;

use crate::database::repositories::setting::SettingsRepository;
use crate::summary::budget::SUMMARY_SEED;
use crate::summary::llm_client::{generate_summary, Gate, GenerationOptions};
use crate::summary::service::{load_llm_config, LlmConfig};
use crate::summary::summary_engine::sidecar::OnAbandon;

/// Output cap per request: up to three notes of JSON, each with its topic,
/// fit comfortably. A reply cut off at the cap is skipped, so the cap leaves
/// headroom rather than trimming to the typical reply.
pub const MAX_OUTPUT_TOKENS: u32 = 256;
/// Sampling temperature: low, so entries stay factual and stable.
pub const TEMPERATURE: f32 = 0.2;
/// A request taking longer than this is abandoned; the next window retries.
/// Time spent queued behind another built-in request does not count.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// The generation settings every live summary request uses, whatever the
/// provider: the window must never interrupt or slow the meeting's summary,
/// so an abandoned built-in request finishes in the background instead of
/// stopping the shared sidecar. It runs at background priority: Copilot may
/// pre-empt it, and the window is then retried.
pub(crate) fn generation_options() -> GenerationOptions {
    GenerationOptions {
        max_tokens: Some(MAX_OUTPUT_TOKENS),
        output_cap: Some(MAX_OUTPUT_TOKENS),
        temperature: Some(TEMPERATURE),
        sampling_temperature: Some(TEMPERATURE),
        seed: Some(SUMMARY_SEED),
        timeout: Some(REQUEST_TIMEOUT),
        on_abandon: OnAbandon::Yield,
        // Reasoning models get low effort and room to think on top of the
        // cap, or they would spend the 150 tokens thinking.
        brief: true,
        ..Default::default()
    }
}

/// A model reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Generated {
    pub text: String,
    /// The provider stopped at the output cap: the JSON is incomplete.
    pub truncated: bool,
}

/// Why a request failed, as far as the worker's retry policy cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// Worth retrying soon (network blip, timeout, busy model).
    Transient,
    /// The provider refused us (bad key, quota, rate limit): retrying every
    /// minute only adds noise, so the worker backs off for long and says so.
    Unavailable,
    /// Copilot needed the built-in model: not a failure. The window's text
    /// goes back in the queue and is retried shortly, without a notice.
    Preempted,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct GenerateError {
    pub kind: FailureKind,
    pub message: String,
}

impl GenerateError {
    pub fn transient(message: impl Into<String>) -> Self {
        Self {
            kind: FailureKind::Transient,
            message: message.into(),
        }
    }

    /// Classifies a `generate_summary` error message.
    pub fn from_provider_error(message: String) -> Self {
        let kind = if is_preempted_error(&message) {
            FailureKind::Preempted
        } else if is_unavailable_error(&message) {
            FailureKind::Unavailable
        } else {
            FailureKind::Transient
        };
        Self { kind, message }
    }
}

/// The sidecar stopped this request for a Copilot request
/// (`SidecarError::Preempted`).
fn is_preempted_error(message: &str) -> bool {
    message.contains("preempted by an interactive request")
}

/// Auth and quota failures from the providers' HTTP APIs.
fn is_unavailable_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    ["status 401", "status 402", "status 403", "status 429"]
        .iter()
        .any(|status| lower.contains(status))
        || lower.contains("invalid api key")
        || lower.contains("invalid_api_key")
        || lower.contains("rate limit")
}

/// Something that turns a prompt into model text. The worker only sees this,
/// so tests can drive it with a fake.
#[async_trait]
pub trait Generator: Send + Sync {
    /// `provider` and `model` stored with each entry.
    fn provider(&self) -> &str;
    fn model(&self) -> &str;
    async fn generate(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        cancel: &CancellationToken,
    ) -> Result<Generated, GenerateError>;

    /// The notes call: [`Generator::generate`] for an answer of at most
    /// `max_lines` lines (the compact answer's topic line and the notes the
    /// window keeps). A model that can stop there does, rather than write
    /// notes the parser would drop; the default ignores the limit.
    async fn generate_notes(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        _max_lines: usize,
        cancel: &CancellationToken,
    ) -> Result<Generated, GenerateError> {
        self.generate(system_prompt, user_prompt, cancel).await
    }

    /// The live notes LoRA adapter when this is the fine-tuned live notes
    /// model (the built-in Gemma 4 E2B with the adapter on disk): the worker
    /// then speaks its trained format (`finetuned`). `None` for every other
    /// model, and without the adapter.
    fn live_notes_adapter(&self) -> Option<&Path> {
        None
    }

    /// A fine-tuned window (`finetuned`): greedy, "." gated, the adapter
    /// applied. The default is [`Generator::generate`].
    async fn generate_finetuned(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        cancel: &CancellationToken,
    ) -> Result<Generated, GenerateError> {
        self.generate(system_prompt, user_prompt, cancel).await
    }

    /// An Enhanced notes section (or its title): a longer answer than a
    /// window's notes, of at most `max_lines` lines on a model that can stop
    /// there. The default is [`Generator::generate`].
    async fn generate_section(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        _max_lines: usize,
        cancel: &CancellationToken,
    ) -> Result<Generated, GenerateError> {
        self.generate(system_prompt, user_prompt, cancel).await
    }
}

/// Output cap of an Enhanced notes section call: four bullets and two
/// questions with room to spare.
pub const SECTION_MAX_OUTPUT_TOKENS: u32 = 448;
/// A section call taking longer than this is abandoned (its run keeps its
/// live notes, or is written again at stop).
pub const SECTION_TIMEOUT: Duration = Duration::from_secs(90);

fn section_timeout() -> Duration {
    // Developer evaluations on a loaded machine may allow longer.
    #[cfg(test)]
    if let Some(seconds) = std::env::var("MEETILY_SECTION_TIMEOUT_S")
        .ok()
        .and_then(|s| s.parse().ok())
    {
        return Duration::from_secs(seconds);
    }
    SECTION_TIMEOUT
}

/// The generation settings of a section call: the live notes' (low
/// temperature, fixed seed, background priority), with a longer answer.
pub(crate) fn section_generation_options() -> GenerationOptions {
    GenerationOptions {
        max_tokens: Some(SECTION_MAX_OUTPUT_TOKENS),
        output_cap: Some(SECTION_MAX_OUTPUT_TOKENS),
        timeout: Some(section_timeout()),
        ..generation_options()
    }
}

/// The generation settings of a fine-tuned window: the adapter, greedy (the
/// closed-loop evaluation's decoding), "." answered without generating when
/// the model is sure of it, a small context.
pub(crate) fn finetuned_generation_options(adapter: &Path) -> GenerationOptions {
    use super::finetuned::{CONTEXT_SIZE, GATE_THRESHOLD, GATE_TOKEN, MAX_OUTPUT_TOKENS};
    GenerationOptions {
        max_tokens: Some(MAX_OUTPUT_TOKENS),
        output_cap: Some(MAX_OUTPUT_TOKENS),
        temperature: Some(0.0),
        sampling_temperature: Some(0.0),
        lora_path: Some(adapter.to_path_buf()),
        gate: Some(Gate {
            token: GATE_TOKEN.to_string(),
            threshold: GATE_THRESHOLD,
        }),
        context_size: Some(CONTEXT_SIZE),
        ..generation_options()
    }
}

/// The summary model setting, resolved for live summary calls.
pub struct SummaryModelGenerator {
    provider_id: String,
    config: LlmConfig,
    app_data_dir: Option<PathBuf>,
    client: reqwest::Client,
    /// Ollama's recording-time num_ctx, shared by every call while recording.
    num_ctx: Option<u32>,
    /// Built-in AI with the live notes model (Gemma 4 E2B): that model and
    /// whether it runs on its own helper. `None` keeps the summary model's
    /// path (the JSON prompt, a fresh context on the shared helper).
    builtin: Option<(String, bool)>,
    /// The live notes LoRA adapter, when `builtin` is the live notes model
    /// and the adapter is on disk.
    adapter: Option<PathBuf>,
}

/// Providers that run the built-in model.
pub fn is_builtin(provider: &str) -> bool {
    matches!(provider, "builtin-ai" | "local-llama" | "localllama")
}

/// The user's choice of model for live notes (`liveNotesModel` in the app
/// store). Absent means no explicit choice: the default, which is the live
/// notes model once it is downloaded on a Mac with 16 GB or more.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LiveNotesChoice {
    /// Never chosen: the live notes model when it is on disk and the Mac
    /// has `DEFAULT_MIN_MEMORY_BYTES`, the summary model otherwise.
    #[default]
    Default,
    /// "Same as summary model", picked explicitly.
    SummaryModel,
    /// The live notes model, picked explicitly (or by onboarding's opt-in).
    LiveNotesModel,
}

impl LiveNotesChoice {
    pub const SUMMARY: &'static str = "summary";

    pub fn from_stored(value: Option<&str>) -> Self {
        match value {
            Some(crate::summary::summary_engine::models::LIVE_NOTES_MODEL) => Self::LiveNotesModel,
            Some(Self::SUMMARY) => Self::SummaryModel,
            _ => Self::Default,
        }
    }

    /// The stored value; `None` for no explicit choice.
    pub fn stored(self) -> Option<&'static str> {
        match self {
            Self::Default => None,
            Self::SummaryModel => Some(Self::SUMMARY),
            Self::LiveNotesModel => Some(crate::summary::summary_engine::models::LIVE_NOTES_MODEL),
        }
    }
}

/// Macs with this much memory or less never keep two models resident while
/// recording: live notes stay on the summary model's helper.
pub const LOW_MEMORY_BYTES: u64 = 8 * 1024 * 1024 * 1024;

/// From this much memory the live notes model is the default once it is
/// downloaded (and onboarding offers its download ticked).
pub const DEFAULT_MIN_MEMORY_BYTES: u64 = 16 * 1024 * 1024 * 1024;

/// Whether `total_memory` is a low-memory machine (unknown counts as not).
pub fn is_low_memory(total_memory: Option<u64>) -> bool {
    total_memory.is_some_and(|bytes| bytes <= LOW_MEMORY_BYTES)
}

/// Whether the live notes model is the default on this Mac (unknown memory
/// counts as not: the default never risks two resident models).
pub fn live_notes_model_by_default(total_memory: Option<u64>) -> bool {
    total_memory.is_some_and(|bytes| bytes >= DEFAULT_MIN_MEMORY_BYTES)
}

/// Whether live notes run on the live notes model rather than the summary
/// model, once that model is on disk: an explicit choice wins (the live notes
/// model still never on an 8 GB Mac); without one, the default by memory.
pub fn prefers_live_notes_model(choice: LiveNotesChoice, total_memory: Option<u64>) -> bool {
    match choice {
        LiveNotesChoice::SummaryModel => false,
        LiveNotesChoice::LiveNotesModel => !is_low_memory(total_memory),
        LiveNotesChoice::Default => live_notes_model_by_default(total_memory),
    }
}

/// Whether the live notes model's file is in `models_dir`.
pub fn live_notes_model_downloaded(models_dir: &std::path::Path) -> bool {
    use crate::summary::summary_engine::models::{get_model_by_name, LIVE_NOTES_MODEL};
    get_model_by_name(LIVE_NOTES_MODEL)
        .is_some_and(|model| models_dir.join(model.gguf_file).is_file())
}

/// The built-in live notes model and whether it runs on its own helper, or
/// `None` for the summary model's usual path:
/// - the summary model is the live notes model: it, on the one helper;
/// - the live notes model is downloaded and preferred
///   ([`prefers_live_notes_model`]): it, on its own helper (no reloads
///   against Copilot);
/// - otherwise (not downloaded, "Same as summary model" picked, an 8 GB Mac,
///   or under 16 GB without an explicit choice): the summary model, exactly
///   as before the live notes model existed.
pub fn live_notes_model(
    models_dir: &std::path::Path,
    summary_model: &str,
    choice: LiveNotesChoice,
    total_memory: Option<u64>,
) -> Option<(String, bool)> {
    use crate::summary::summary_engine::models::LIVE_NOTES_MODEL;
    if summary_model == LIVE_NOTES_MODEL {
        return Some((LIVE_NOTES_MODEL.to_string(), false));
    }
    (prefers_live_notes_model(choice, total_memory) && live_notes_model_downloaded(models_dir))
        .then(|| (LIVE_NOTES_MODEL.to_string(), true))
}

/// The live notes adapter for `model`, when it is the live notes model and
/// the adapter is on disk.
pub fn live_notes_adapter(models_dir: &Path, model: &str) -> Option<PathBuf> {
    use crate::summary::summary_engine::adapters::{adapter_path, Adapter};
    (model == crate::summary::summary_engine::models::LIVE_NOTES_MODEL)
        .then(|| adapter_path(models_dir, Adapter::LiveNotes))
        .flatten()
}

impl SummaryModelGenerator {
    /// The user's summary model, or why there is none to use.
    pub async fn from_settings(
        pool: &SqlitePool,
        app_data_dir: Option<PathBuf>,
        choice: LiveNotesChoice,
    ) -> anyhow::Result<Self> {
        let settings = SettingsRepository::get_model_config(pool)
            .await
            .map_err(|e| anyhow!("Could not read the summary model setting: {e}"))?
            .filter(|s| !s.provider.trim().is_empty() && !s.model.trim().is_empty())
            .ok_or_else(|| anyhow!("No summary model is configured"))?;
        let config = load_llm_config(pool, &settings.provider, &settings.model, Some(&settings))
            .await
            .map_err(|e| anyhow!(e))?;
        // The timeline only runs while recording.
        let num_ctx = crate::summary::service::ollama_num_ctx_for(
            &config.provider,
            &config.model_name,
            config.ollama_endpoint.as_deref(),
            crate::summary::service::OllamaPhase::Recording,
        )
        .await;
        let builtin = is_builtin(&settings.provider)
            .then(|| {
                let models_dir = app_data_dir
                    .as_deref()
                    .map(crate::summary::summary_engine::models::get_models_directory)
                    .unwrap_or_default();
                live_notes_model(
                    &models_dir,
                    &config.model_name,
                    choice,
                    crate::summary::summary_engine::client::total_memory_bytes(),
                )
                .map(|builtin| (builtin, models_dir))
            })
            .flatten();
        let (builtin, adapter) = match builtin {
            Some((builtin, models_dir)) => {
                let adapter = live_notes_adapter(&models_dir, &builtin.0);
                (Some(builtin), adapter)
            }
            None => (None, None),
        };
        Ok(Self {
            num_ctx,
            builtin,
            adapter,
            provider_id: settings.provider,
            config,
            app_data_dir,
            client: reqwest::Client::new(),
        })
    }
}

impl SummaryModelGenerator {
    /// A built-in model without the settings database (developer
    /// evaluations): `own_helper` runs it on the live notes helper.
    #[cfg(test)]
    pub(crate) fn builtin_for_eval(app_data_dir: PathBuf, model: &str, own_helper: bool) -> Self {
        use crate::summary::llm_client::LLMProvider;
        Self {
            provider_id: "builtin-ai".to_string(),
            config: LlmConfig {
                provider: LLMProvider::BuiltInAI,
                model_name: model.to_string(),
                api_key: String::new(),
                ollama_endpoint: None,
                custom_openai_endpoint: None,
                max_tokens: None,
                temperature: None,
                top_p: None,
            },
            app_data_dir: Some(app_data_dir),
            client: reqwest::Client::new(),
            num_ctx: None,
            builtin: Some((model.to_string(), own_helper)),
            adapter: None,
        }
    }
}

#[async_trait]
impl Generator for SummaryModelGenerator {
    fn provider(&self) -> &str {
        &self.provider_id
    }

    fn model(&self) -> &str {
        match &self.builtin {
            Some((model, _)) => model,
            None => &self.config.model_name,
        }
    }

    async fn generate(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        cancel: &CancellationToken,
    ) -> Result<Generated, GenerateError> {
        self.generate_with_limit(
            system_prompt,
            user_prompt,
            None,
            generation_options(),
            cancel,
        )
        .await
    }

    async fn generate_notes(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        max_lines: usize,
        cancel: &CancellationToken,
    ) -> Result<Generated, GenerateError> {
        self.generate_with_limit(
            system_prompt,
            user_prompt,
            u32::try_from(max_lines).ok(),
            generation_options(),
            cancel,
        )
        .await
    }

    async fn generate_section(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        max_lines: usize,
        cancel: &CancellationToken,
    ) -> Result<Generated, GenerateError> {
        self.generate_with_limit(
            system_prompt,
            user_prompt,
            u32::try_from(max_lines).ok(),
            section_generation_options(),
            cancel,
        )
        .await
    }

    fn live_notes_adapter(&self) -> Option<&Path> {
        self.adapter.as_deref()
    }

    async fn generate_finetuned(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        cancel: &CancellationToken,
    ) -> Result<Generated, GenerateError> {
        let Some(adapter) = self.adapter.as_deref() else {
            return self.generate(system_prompt, user_prompt, cancel).await;
        };
        self.generate_with_limit(
            system_prompt,
            user_prompt,
            None,
            finetuned_generation_options(adapter),
            cancel,
        )
        .await
    }
}

/// The line limit only applies to the live notes model's compact one-note-
/// per-line answer. Other built-in models answer in JSON, which may span many
/// lines, so cutting them off after a few lines would lose notes.
fn line_limit_for(model: &str, max_lines: Option<u32>) -> Option<u32> {
    use crate::summary::summary_engine::models::LIVE_NOTES_MODEL;
    if model == LIVE_NOTES_MODEL {
        max_lines
    } else {
        None
    }
}

impl SummaryModelGenerator {
    /// The call, with a line limit on the live notes model's compact answer
    /// (other models and providers write JSON: no limit).
    async fn generate_with_limit(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        max_lines: Option<u32>,
        options: GenerationOptions,
        cancel: &CancellationToken,
    ) -> Result<Generated, GenerateError> {
        if let (Some((model, own_helper)), Some(app_data_dir)) =
            (&self.builtin, self.app_data_dir.as_ref())
        {
            use crate::summary::summary_engine::client::{
                generate_live_notes, BuiltinGenerateOptions, LiveNotesRequest,
            };
            // No grammar: the live notes model keeps the compact format on
            // its own (no unreadable window in the benchmark), and checking
            // every token against a grammar made generation 10-25 % slower.
            let grammar = None;
            let max_lines = line_limit_for(model, max_lines);
            return generate_live_notes(
                app_data_dir,
                LiveNotesRequest {
                    model_name: model,
                    system_prompt,
                    user_prompt,
                    grammar,
                    max_lines,
                    own_helper: *own_helper,
                    one_off: false,
                    options: BuiltinGenerateOptions::from(&options),
                },
                Some(cancel),
            )
            .await
            .map(|completion| Generated {
                text: completion.text,
                truncated: completion.truncated,
            })
            .map_err(|error| GenerateError::from_provider_error(error.to_string()));
        }
        let mut options = options;
        options.num_ctx = self.num_ctx;
        generate_summary(
            &self.client,
            &self.config.provider,
            &self.config.model_name,
            &self.config.api_key,
            system_prompt,
            user_prompt,
            self.config.ollama_endpoint.as_deref(),
            self.config.custom_openai_endpoint.as_deref(),
            &options,
            self.app_data_dir.as_ref(),
            Some(cancel),
        )
        .await
        .map(|completion| Generated {
            text: completion.content,
            truncated: completion.truncated,
        })
        .map_err(GenerateError::from_provider_error)
    }
}

#[cfg(test)]
mod line_limit_tests {
    use super::line_limit_for;
    use crate::summary::summary_engine::models::LIVE_NOTES_MODEL;

    #[test]
    fn only_the_compact_live_notes_model_stops_early() {
        assert_eq!(line_limit_for(LIVE_NOTES_MODEL, Some(4)), Some(4));
        assert_eq!(line_limit_for("qwen3.5:4b", Some(4)), None);
        assert_eq!(line_limit_for("gemma3:1b", Some(4)), None);
        assert_eq!(line_limit_for(LIVE_NOTES_MODEL, None), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1024 * 1024 * 1024;

    #[test]
    fn live_notes_use_their_model_by_default_once_downloaded_on_16_gb() {
        use crate::summary::summary_engine::models::{get_model_by_name, LIVE_NOTES_MODEL};
        use LiveNotesChoice::{Default, LiveNotesModel, SummaryModel};
        let dir = tempfile::tempdir().unwrap();
        let e2b = || Some((LIVE_NOTES_MODEL.to_string(), true));
        let (gb8, gb16, gb24) = (Some(8 * GB), Some(16 * GB), Some(24 * GB));
        // Not downloaded: the summary model's usual path, whatever the choice.
        for choice in [Default, SummaryModel, LiveNotesModel] {
            assert_eq!(
                live_notes_model(dir.path(), "qwen3.5:4b", choice, gb24),
                None
            );
        }
        let file = get_model_by_name(LIVE_NOTES_MODEL).unwrap().gguf_file;
        std::fs::write(dir.path().join(file), b"GGUF").unwrap();
        // Downloaded, no explicit choice: the live notes model from 16 GB.
        assert_eq!(
            live_notes_model(dir.path(), "qwen3.5:4b", Default, gb16),
            e2b()
        );
        assert_eq!(
            live_notes_model(dir.path(), "qwen3.5:4b", Default, gb24),
            e2b()
        );
        assert_eq!(
            live_notes_model(dir.path(), "qwen3.5:4b", Default, gb8),
            None
        );
        assert_eq!(
            live_notes_model(dir.path(), "qwen3.5:4b", Default, None),
            None
        );
        // "Same as summary model" picked explicitly: kept.
        assert_eq!(
            live_notes_model(dir.path(), "qwen3.5:4b", SummaryModel, gb24),
            None
        );
        // Picked explicitly: used above 8 GB.
        assert_eq!(
            live_notes_model(dir.path(), "qwen3.5:4b", LiveNotesModel, Some(12 * GB)),
            e2b()
        );
        // 8 GB Macs never keep two models resident while recording.
        assert_eq!(
            live_notes_model(dir.path(), "qwen3.5:4b", LiveNotesModel, gb8),
            None
        );
        // Already the summary model: one helper serves both.
        for choice in [Default, SummaryModel, LiveNotesModel] {
            assert_eq!(
                live_notes_model(dir.path(), LIVE_NOTES_MODEL, choice, gb8),
                Some((LIVE_NOTES_MODEL.to_string(), false))
            );
        }
    }

    #[test]
    fn no_stored_choice_is_the_default_and_an_explicit_one_round_trips() {
        use crate::summary::summary_engine::models::LIVE_NOTES_MODEL;
        // Existing users never wrote the key unless they picked: they get the default.
        assert_eq!(LiveNotesChoice::from_stored(None), LiveNotesChoice::Default);
        assert_eq!(
            LiveNotesChoice::from_stored(Some("bogus")),
            LiveNotesChoice::Default
        );
        assert_eq!(
            LiveNotesChoice::from_stored(Some("summary")),
            LiveNotesChoice::SummaryModel
        );
        assert_eq!(
            LiveNotesChoice::from_stored(Some(LIVE_NOTES_MODEL)),
            LiveNotesChoice::LiveNotesModel
        );
        for choice in [
            LiveNotesChoice::Default,
            LiveNotesChoice::SummaryModel,
            LiveNotesChoice::LiveNotesModel,
        ] {
            assert_eq!(LiveNotesChoice::from_stored(choice.stored()), choice);
        }
        assert!(is_low_memory(Some(8 * GB)));
        assert!(!is_low_memory(Some(16 * GB)));
        assert!(!is_low_memory(None));
        assert!(live_notes_model_by_default(Some(16 * GB)));
        assert!(!live_notes_model_by_default(Some(8 * GB)));
        assert!(!live_notes_model_by_default(None));
    }

    #[test]
    fn the_adapter_is_only_for_the_live_notes_model_and_runs_greedy_and_gated() {
        use crate::summary::summary_engine::client::BuiltinGenerateOptions;
        use crate::summary::summary_engine::models::LIVE_NOTES_MODEL;
        let dir = tempfile::tempdir().unwrap();
        // Without the adapter: today's live notes, unchanged.
        assert_eq!(live_notes_adapter(dir.path(), LIVE_NOTES_MODEL), None);
        let adapter = dir.path().join("minutes-notes-lora-v3.gguf");
        std::fs::write(&adapter, b"GGUF").unwrap();
        assert_eq!(
            live_notes_adapter(dir.path(), LIVE_NOTES_MODEL),
            Some(adapter.clone())
        );
        assert_eq!(live_notes_adapter(dir.path(), "qwen3.5:4b"), None);

        let options = BuiltinGenerateOptions::from(&finetuned_generation_options(&adapter));
        assert_eq!(options.max_tokens, Some(300));
        assert_eq!(options.temperature, Some(0.0));
        assert_eq!(options.lora_path.as_deref(), Some(adapter.as_path()));
        let gate = options.gate.unwrap();
        assert_eq!((gate.token.as_str(), gate.threshold), (".", 0.9));
        assert_eq!(options.context_size, Some(4096));
        assert_eq!(options.on_abandon, OnAbandon::Yield);
        // Every other live notes call keeps the base model.
        let plain = BuiltinGenerateOptions::from(&generation_options());
        assert_eq!(
            (plain.lora_path, plain.gate, plain.context_size),
            (None, None, None)
        );
    }

    use crate::summary::llm_client::LLMProvider;
    use crate::summary::llm_client::{build_ollama_chat_body, build_openai_compat_chat_body};

    #[test]
    fn every_provider_gets_the_low_temperature_seed_and_cap() {
        let options = generation_options();
        for provider in [
            LLMProvider::OpenAI,
            LLMProvider::Groq,
            LLMProvider::OpenRouter,
        ] {
            let body = build_openai_compat_chat_body(&provider, "m", "s", "u", &options);
            assert_eq!(body["max_tokens"], MAX_OUTPUT_TOKENS);
            assert_eq!(body["seed"], SUMMARY_SEED);
            assert!((body["temperature"].as_f64().unwrap() - TEMPERATURE as f64).abs() < 1e-6);
        }
        let custom =
            build_openai_compat_chat_body(&LLMProvider::CustomOpenAI, "m", "s", "u", &options);
        assert_eq!(custom["max_tokens"], MAX_OUTPUT_TOKENS);
        assert!((custom["temperature"].as_f64().unwrap() - TEMPERATURE as f64).abs() < 1e-6);
        let ollama = build_ollama_chat_body("m", "s", "u", &options);
        assert_eq!(ollama["options"]["num_predict"], MAX_OUTPUT_TOKENS);
        assert_eq!(ollama["options"]["seed"], SUMMARY_SEED);
        assert_eq!(options.on_abandon, OnAbandon::Yield);
    }

    #[test]
    fn auth_and_rate_limit_errors_mean_unavailable() {
        for message in [
            "LLM API request failed with status 401 Unauthorized: bad key",
            "LLM API request failed with status 429 Too Many Requests: slow down",
        ] {
            assert_eq!(
                GenerateError::from_provider_error(message.into()).kind,
                FailureKind::Unavailable
            );
        }
        assert_eq!(
            GenerateError::from_provider_error("connection reset".into()).kind,
            FailureKind::Transient
        );
        let preempted = crate::summary::summary_engine::sidecar::SidecarError::Preempted;
        assert_eq!(
            GenerateError::from_provider_error(preempted.to_string()).kind,
            FailureKind::Preempted
        );
    }
}
