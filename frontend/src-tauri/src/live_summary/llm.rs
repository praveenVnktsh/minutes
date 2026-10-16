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

/// Output cap per request: one or two entries of JSON fit comfortably.
pub const MAX_OUTPUT_TOKENS: u32 = 150;
/// Sampling temperature: low, so entries stay factual and stable.
pub const TEMPERATURE: f32 = 0.2;
/// A request taking longer than this is abandoned; the next window retries.
/// Time spent queued behind another built-in request does not count.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// The generation settings every live summary request uses, whatever the
/// provider: the window must never interrupt or slow the meeting's summary,
/// so an abandoned built-in request finishes in the background instead of
/// stopping the shared sidecar.
pub(crate) fn generation_options() -> GenerationOptions {
    GenerationOptions {
        max_tokens: Some(MAX_OUTPUT_TOKENS),
        output_cap: Some(MAX_OUTPUT_TOKENS),
        temperature: Some(TEMPERATURE),
        sampling_temperature: Some(TEMPERATURE),
        seed: Some(SUMMARY_SEED),
        timeout: Some(REQUEST_TIMEOUT),
        on_abandon: OnAbandon::LetFinish,
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
        let kind = if is_unavailable_error(&message) {
            FailureKind::Unavailable
        } else {
            FailureKind::Transient
        };
        Self { kind, message }
    }
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
    /// Ollama's num_ctx, the same as every other app call to the model.
    num_ctx: Option<u32>,
}

impl SummaryModelGenerator {
    /// The user's summary model, or why there is none to use.
    pub async fn from_settings(
        pool: &SqlitePool,
        app_data_dir: Option<PathBuf>,
    ) -> anyhow::Result<Self> {
        let settings = SettingsRepository::get_model_config(pool)
            .await
            .map_err(|e| anyhow!("Could not read the summary model setting: {e}"))?
            .filter(|s| !s.provider.trim().is_empty() && !s.model.trim().is_empty())
            .ok_or_else(|| anyhow!("No summary model is configured"))?;
        let config = load_llm_config(pool, &settings.provider, &settings.model, Some(&settings))
            .await
            .map_err(|e| anyhow!(e))?;
        let num_ctx = crate::summary::service::ollama_num_ctx(
            &config.provider,
            &config.model_name,
            config.ollama_endpoint.as_deref(),
        )
        .await;
        Ok(Self {
            num_ctx,
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
        &self.config.model_name
    }

    async fn generate(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        cancel: &CancellationToken,
    ) -> Result<Generated, GenerateError> {
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
        assert_eq!(options.on_abandon, OnAbandon::LetFinish);
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
    }
}
