//! The model call for a live summary window: the user's summary provider and
//! model, through the shared summary entry point (`generate_summary`), with a
//! short output cap, a low temperature, the fixed seed and a timeout of our
//! own. The same options reach every provider.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::anyhow;
use async_trait::async_trait;
use sqlx::SqlitePool;
use tokio_util::sync::CancellationToken;

use crate::database::repositories::setting::SettingsRepository;
use crate::summary::budget::SUMMARY_SEED;
use crate::summary::llm_client::{generate_summary, GenerationOptions};
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
}

/// Providers that run the built-in model.
pub fn is_builtin(provider: &str) -> bool {
    matches!(provider, "builtin-ai" | "local-llama" | "localllama")
}

/// The user's choice of model for live notes (`liveNotesModel` in the app
/// store). Absent means the summary model: nothing changes for anyone who
/// has not opted in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LiveNotesChoice {
    #[default]
    SummaryModel,
    LiveNotesModel,
}

impl LiveNotesChoice {
    pub const SUMMARY: &'static str = "summary";

    pub fn from_stored(value: Option<&str>) -> Self {
        match value {
            Some(crate::summary::summary_engine::models::LIVE_NOTES_MODEL) => Self::LiveNotesModel,
            _ => Self::SummaryModel,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::SummaryModel => Self::SUMMARY,
            Self::LiveNotesModel => crate::summary::summary_engine::models::LIVE_NOTES_MODEL,
        }
    }
}

/// Macs with this much memory or less never keep two models resident while
/// recording: live notes stay on the summary model's helper.
pub const LOW_MEMORY_BYTES: u64 = 8 * 1024 * 1024 * 1024;

/// Whether `total_memory` is a low-memory machine (unknown counts as not).
pub fn is_low_memory(total_memory: Option<u64>) -> bool {
    total_memory.is_some_and(|bytes| bytes <= LOW_MEMORY_BYTES)
}

/// The built-in live notes model and whether it runs on its own helper, or
/// `None` for the summary model's usual path:
/// - the summary model is the live notes model: it, on the one helper;
/// - the user chose the live notes model, its file is downloaded and the Mac
///   has more than 8 GB: it, on its own helper (no reloads against Copilot);
/// - otherwise (not chosen, not downloaded, or 8 GB): the summary model,
///   exactly as before the live notes model existed.
pub fn live_notes_model(
    models_dir: &std::path::Path,
    summary_model: &str,
    choice: LiveNotesChoice,
    low_memory: bool,
) -> Option<(String, bool)> {
    use crate::summary::summary_engine::models::{get_model_by_name, LIVE_NOTES_MODEL};
    if summary_model == LIVE_NOTES_MODEL {
        return Some((LIVE_NOTES_MODEL.to_string(), false));
    }
    let downloaded = get_model_by_name(LIVE_NOTES_MODEL)
        .is_some_and(|model| models_dir.join(model.gguf_file).is_file());
    (choice == LiveNotesChoice::LiveNotesModel && downloaded && !low_memory)
        .then(|| (LIVE_NOTES_MODEL.to_string(), true))
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
                    is_low_memory(crate::summary::summary_engine::client::total_memory_bytes()),
                )
            })
            .flatten();
        Ok(Self {
            num_ctx,
            builtin,
            provider_id: settings.provider,
            config,
            app_data_dir,
            client: reqwest::Client::new(),
        })
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
        if let (Some((model, own_helper)), Some(app_data_dir)) =
            (&self.builtin, self.app_data_dir.as_ref())
        {
            use crate::summary::summary_engine::client::{
                generate_live_notes, BuiltinGenerateOptions, LiveNotesRequest,
            };
            let options = generation_options();
            // No grammar: the live notes model keeps the compact format on
            // its own (no unreadable window in the benchmark), and checking
            // every token against a grammar made generation 10-25 % slower.
            let grammar = None;
            return generate_live_notes(
                app_data_dir,
                LiveNotesRequest {
                    model_name: model,
                    system_prompt,
                    user_prompt,
                    grammar,
                    own_helper: *own_helper,
                    options: BuiltinGenerateOptions {
                        max_tokens: options.max_tokens,
                        temperature: options.sampling_temperature,
                        seed: options.seed,
                        timeout: options.timeout,
                        on_abandon: options.on_abandon,
                    },
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
        let mut options = generation_options();
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
mod tests {
    use super::*;

    #[test]
    fn live_notes_use_their_model_only_once_chosen_and_downloaded() {
        use crate::summary::summary_engine::models::{get_model_by_name, LIVE_NOTES_MODEL};
        use LiveNotesChoice::{LiveNotesModel, SummaryModel};
        let dir = tempfile::tempdir().unwrap();
        let e2b = || Some((LIVE_NOTES_MODEL.to_string(), true));
        // Chosen but not downloaded: the summary model's usual path.
        assert_eq!(
            live_notes_model(dir.path(), "qwen3.5:4b", LiveNotesModel, false),
            None
        );
        let file = get_model_by_name(LIVE_NOTES_MODEL).unwrap().gguf_file;
        std::fs::write(dir.path().join(file), b"GGUF").unwrap();
        // Downloaded but not chosen: existing users are unaffected.
        assert_eq!(
            live_notes_model(dir.path(), "qwen3.5:4b", SummaryModel, false),
            None
        );
        assert_eq!(
            live_notes_model(dir.path(), "qwen3.5:4b", LiveNotesModel, false),
            e2b()
        );
        // 8 GB Macs never keep two models resident while recording.
        assert_eq!(
            live_notes_model(dir.path(), "qwen3.5:4b", LiveNotesModel, true),
            None
        );
        // Already the summary model: one helper serves both.
        for choice in [SummaryModel, LiveNotesModel] {
            assert_eq!(
                live_notes_model(dir.path(), LIVE_NOTES_MODEL, choice, true),
                Some((LIVE_NOTES_MODEL.to_string(), false))
            );
        }
    }

    #[test]
    fn the_live_notes_choice_defaults_to_the_summary_model() {
        use crate::summary::summary_engine::models::LIVE_NOTES_MODEL;
        assert_eq!(
            LiveNotesChoice::from_stored(None),
            LiveNotesChoice::SummaryModel
        );
        assert_eq!(
            LiveNotesChoice::from_stored(Some("summary")),
            LiveNotesChoice::SummaryModel
        );
        assert_eq!(
            LiveNotesChoice::from_stored(Some("bogus")),
            LiveNotesChoice::SummaryModel
        );
        assert_eq!(
            LiveNotesChoice::from_stored(Some(LIVE_NOTES_MODEL)),
            LiveNotesChoice::LiveNotesModel
        );
        assert!(is_low_memory(Some(8 * 1024 * 1024 * 1024)));
        assert!(!is_low_memory(Some(16 * 1024 * 1024 * 1024)));
        assert!(!is_low_memory(None));
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
