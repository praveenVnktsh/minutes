//! The model call for a live summary window: the user's summary provider and
//! model, through the existing LLM entry points, with a short output cap, a
//! low temperature and a timeout of our own.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use sqlx::SqlitePool;
use tokio_util::sync::CancellationToken;

use crate::database::repositories::setting::SettingsRepository;
use crate::summary::llm_client::{generate_summary, LLMProvider};
use crate::summary::service::{load_llm_config, LlmConfig};
use crate::summary::summary_engine::client::{
    generate_with_builtin_options, BuiltinGenerateOptions,
};

/// Output cap per request: one or two entries of JSON fit comfortably.
pub const MAX_OUTPUT_TOKENS: u32 = 150;
/// Sampling temperature: low, so entries stay factual and stable.
pub const TEMPERATURE: f32 = 0.2;
/// A request taking longer than this is abandoned; the next window retries.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

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
    ) -> Result<String>;
}

/// The summary model setting, resolved for live summary calls.
pub struct SummaryModelGenerator {
    provider_id: String,
    config: LlmConfig,
    app_data_dir: Option<PathBuf>,
    client: reqwest::Client,
}

impl SummaryModelGenerator {
    /// The user's summary model, or why there is none to use.
    pub async fn from_settings(pool: &SqlitePool, app_data_dir: Option<PathBuf>) -> Result<Self> {
        let settings = SettingsRepository::get_model_config(pool)
            .await
            .map_err(|e| anyhow!("Could not read the summary model setting: {e}"))?
            .filter(|s| !s.provider.trim().is_empty() && !s.model.trim().is_empty())
            .ok_or_else(|| anyhow!("No summary model is configured"))?;
        let config = load_llm_config(pool, &settings.provider, &settings.model, Some(&settings))
            .await
            .map_err(|e| anyhow!(e))?;
        Ok(Self {
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
    ) -> Result<String> {
        let call = async {
            if self.config.provider == LLMProvider::BuiltInAI {
                let app_data_dir = self
                    .app_data_dir
                    .as_deref()
                    .ok_or_else(|| anyhow!("The app data folder is unavailable"))?;
                return generate_with_builtin_options(
                    app_data_dir,
                    &self.config.model_name,
                    system_prompt,
                    user_prompt,
                    BuiltinGenerateOptions {
                        max_tokens: Some(MAX_OUTPUT_TOKENS as i32),
                        temperature: Some(TEMPERATURE),
                        timeout: Some(REQUEST_TIMEOUT),
                    },
                    Some(cancel),
                )
                .await;
            }
            // Merge point with fix/summary-consistency: generate_summary only
            // forwards max_tokens/temperature for Custom OpenAI today. The other
            // providers rely on the prompt's length rules and the parser's
            // tolerance of truncated output.
            generate_summary(
                &self.client,
                &self.config.provider,
                &self.config.model_name,
                &self.config.api_key,
                system_prompt,
                user_prompt,
                self.config.ollama_endpoint.as_deref(),
                self.config.custom_openai_endpoint.as_deref(),
                Some(MAX_OUTPUT_TOKENS),
                Some(TEMPERATURE),
                self.config.top_p,
                self.app_data_dir.as_ref(),
                Some(cancel),
            )
            .await
            .map(|completion| completion.content)
            .map_err(|e| anyhow!(e))
        };
        // The built-in path enforces REQUEST_TIMEOUT itself; this outer bound
        // is the backstop for HTTP providers (their own timeout is minutes).
        match tokio::time::timeout(REQUEST_TIMEOUT + Duration::from_secs(5), call).await {
            Ok(result) => result,
            Err(_) => {
                cancel.cancel();
                Err(anyhow!(
                    "The summary model took longer than {}s",
                    REQUEST_TIMEOUT.as_secs()
                ))
            }
        }
    }
}
