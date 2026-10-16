//! One model catalog for every provider that serves an OpenAI-shaped
//! `GET /v1/models`: OpenAI, Groq and OpenRouter.
//!
//! Before this module each provider carried its own hardcoded model list, so a
//! model released after the last build was unreachable until the next release
//! shipped — and a user whose account already had it could not select it at
//! all. The catalog asks the provider instead, once per session, and falls back
//! to the hardcoded list only when the provider cannot be reached.
//!
//! Anthropic (`anthropic/anthropic.rs`) and Ollama (`ollama/metadata.rs`) still
//! carry their own copies of this path; their response shapes differ and they
//! are tracked separately.

use serde::{Deserialize, Serialize};
use std::time::Duration;

pub mod cache;
pub mod endpoints;
pub mod fallback;
pub mod filter;

/// A provider whose models endpoint speaks the OpenAI shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CatalogProvider {
    OpenAI,
    Groq,
    OpenRouter,
}

/// One model as the catalog knows it.
///
/// The superset of what the three providers report. Each provider's Tauri
/// command narrows this to the shape its frontend caller already expects, so
/// the fields no provider fills stay `None`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogModel {
    pub id: String,
    pub name: Option<String>,
    pub owned_by: Option<String>,
    pub context_length: Option<u32>,
    pub prompt_price: Option<String>,
    pub completion_price: Option<String>,
}

impl CatalogModel {
    /// A model known only by its id, which is all a fallback list holds.
    pub fn from_id(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: None,
            owned_by: None,
            context_length: None,
            prompt_price: None,
            completion_price: None,
        }
    }
}

/// How long a models listing may take before the catalog gives up on it.
///
/// Five seconds, matching what `openai.rs` and `groq.rs` used before this
/// module: the settings form shows a spinner for exactly as long as it always
/// has, and a provider that has gone dark does not hold the dropdown hostage.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// One model as a provider's `GET /v1/models` reports it.
///
/// One struct for all three providers, because the three fill different
/// subsets of the same shape: Groq sends `owned_by`, OpenRouter sends `name`,
/// `context_length`, `top_provider.context_length` and `pricing`, and OpenAI
/// sends essentially just `id`. Everything but `id` is `Option` with
/// `#[serde(default)]` so a provider adding or dropping a field does not turn
/// the whole listing into a parse error and drop the user onto the fallback.
#[derive(Debug, Deserialize)]
struct ApiModel {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    owned_by: Option<String>,
    #[serde(default)]
    context_length: Option<u32>,
    #[serde(default)]
    top_provider: Option<ApiTopProvider>,
    #[serde(default)]
    pricing: Option<ApiPricing>,
}

/// OpenRouter's per-provider block, whose `context_length` is the one the
/// model is actually served with.
#[derive(Debug, Default, Deserialize)]
struct ApiTopProvider {
    #[serde(default)]
    context_length: Option<u32>,
}

/// OpenRouter's per-token prices, reported as decimal strings rather than
/// numbers, and passed through as strings so no precision is invented.
#[derive(Debug, Default, Deserialize)]
struct ApiPricing {
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    completion: Option<String>,
}

/// The envelope all three providers wrap their listing in.
#[derive(Debug, Deserialize)]
struct ApiResponse {
    data: Vec<ApiModel>,
}

/// Parses a models-listing body into the catalog's own shape, keeping only
/// the models that can summarise a transcript.
///
/// Split out of [`list_models`] so the mapping is testable without a network:
/// the crate has no HTTP mock and the test suite must never make a real
/// request, so this is where the coverage lives.
fn models_from_body(
    provider: CatalogProvider,
    body: &str,
) -> Result<Vec<CatalogModel>, serde_json::Error> {
    let response: ApiResponse = serde_json::from_str(body)?;

    Ok(response
        .data
        .into_iter()
        .filter(|model| filter::is_summarisation_capable(provider, &model.id))
        .map(|model| CatalogModel {
            id: model.id,
            name: model.name,
            owned_by: model.owned_by,
            // `top_provider.context_length` wins over the top-level field:
            // it is the window the model is actually served with, and it is
            // the number the pre-catalog `get_openrouter_models` reported.
            // The OpenRouter command that replaces it must keep reporting the
            // same one.
            context_length: model
                .top_provider
                .as_ref()
                .and_then(|top| top.context_length)
                .or(model.context_length),
            prompt_price: model.pricing.as_ref().and_then(|p| p.prompt.clone()),
            completion_price: model.pricing.as_ref().and_then(|p| p.completion.clone()),
        })
        .collect())
}

/// The models `provider` can summarise with, fetched once per session.
///
/// Infallible by design — it returns a `Vec`, never a `Result`. No network, a
/// rejected key, an unreadable body, a provider serving only models we filter
/// out: every one of those ends in [`fallback::fallback_models`], because the
/// one thing a user must never be shown is an empty model dropdown.
///
/// Only a real, non-empty listing is cached. A fallback is never written to
/// the cache, so the moment a user pastes a key — or their network comes back
/// — the next call goes to the provider instead of replaying the offline list
/// for the rest of the session.
pub async fn list_models(provider: CatalogProvider, api_key: Option<&str>) -> Vec<CatalogModel> {
    // Normalise first, so a key of `Some("  ")` is treated as absent by the
    // cache lookup, the requires-a-key check and the request alike.
    let api_key = api_key.map(str::trim).filter(|key| !key.is_empty());

    if let Some(models) = cache::cached(provider, api_key) {
        log::info!(
            "model_catalog: serving {} cached {:?} models",
            models.len(),
            provider
        );
        return models;
    }

    if api_key.is_none() && endpoints::requires_api_key(provider) {
        // Info, not warn: a user who has not typed a key yet is on the happy
        // path through the settings form, not in an error state.
        log::info!(
            "model_catalog: no API key for {:?} yet, offering the offline list",
            provider
        );
        return fallback::fallback_models(provider);
    }

    let url = endpoints::models_url(provider);
    log::info!("model_catalog: fetching {:?} models from {}", provider, url);

    let client = reqwest::Client::new();
    let response = match client
        .get(url)
        .headers(endpoints::auth_headers(provider, api_key))
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
    {
        Ok(response) => response,
        Err(err) => {
            log::warn!(
                "model_catalog: request for {:?} models failed: {}. Using the offline list.",
                provider,
                err
            );
            return fallback::fallback_models(provider);
        }
    };

    let status = response.status();
    if !status.is_success() {
        log::warn!(
            "model_catalog: {:?} returned status {}. Using the offline list.",
            provider,
            status
        );
        return fallback::fallback_models(provider);
    }

    // Read the body as text rather than calling `.json()`, so the parse is
    // the pure function above and the tests exercise the same code path the
    // app does.
    let body = match response.text().await {
        Ok(body) => body,
        Err(err) => {
            log::warn!(
                "model_catalog: could not read the {:?} response body: {}. Using the offline list.",
                provider,
                err
            );
            return fallback::fallback_models(provider);
        }
    };

    let models = match models_from_body(provider, &body) {
        Ok(models) => models,
        Err(err) => {
            log::warn!(
                "model_catalog: could not parse the {:?} response: {}. Using the offline list.",
                provider,
                err
            );
            return fallback::fallback_models(provider);
        }
    };

    if models.is_empty() {
        // A restricted key, or a provider serving nothing we can summarise
        // with. Either way an empty dropdown is not an answer a user can act
        // on, so treat it as a failure and leave the cache untouched.
        log::warn!(
            "model_catalog: {:?} returned no summarisation-capable models. Using the offline list.",
            provider
        );
        return fallback::fallback_models(provider);
    }

    log::info!(
        "model_catalog: fetched {} {:?} models",
        models.len(),
        provider
    );
    cache::store(provider, api_key, &models);
    models
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_body_keeps_only_chat_models() {
        let body = r#"{
            "object": "list",
            "data": [
                {"id": "gpt-4o", "object": "model", "owned_by": "system"},
                {"id": "text-embedding-3-large", "object": "model", "owned_by": "system"},
                {"id": "whisper-1", "object": "model", "owned_by": "openai-internal"}
            ]
        }"#;

        let models = models_from_body(CatalogProvider::OpenAI, body).expect("body should parse");

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "gpt-4o");
    }

    #[test]
    fn groq_body_carries_owned_by_through() {
        let body = r#"{
            "object": "list",
            "data": [
                {
                    "id": "llama-3.3-70b-versatile",
                    "object": "model",
                    "owned_by": "Meta",
                    "context_window": 131072
                }
            ]
        }"#;

        let models = models_from_body(CatalogProvider::Groq, body).expect("body should parse");

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "llama-3.3-70b-versatile");
        assert_eq!(models[0].owned_by.as_deref(), Some("Meta"));
    }

    #[test]
    fn openrouter_body_carries_name_context_and_prices() {
        let body = r#"{
            "data": [
                {
                    "id": "anthropic/claude-sonnet-4.5",
                    "name": "Anthropic: Claude Sonnet 4.5",
                    "context_length": 200000,
                    "top_provider": {"context_length": 1000000},
                    "pricing": {"prompt": "0.000003", "completion": "0.000015"}
                }
            ]
        }"#;

        let models =
            models_from_body(CatalogProvider::OpenRouter, body).expect("body should parse");

        assert_eq!(models.len(), 1);
        let model = &models[0];
        assert_eq!(model.name.as_deref(), Some("Anthropic: Claude Sonnet 4.5"));
        assert_eq!(model.prompt_price.as_deref(), Some("0.000003"));
        assert_eq!(model.completion_price.as_deref(), Some("0.000015"));
        // `top_provider.context_length` wins over the top-level one, which is
        // the number the pre-catalog OpenRouter command reported.
        assert_eq!(model.context_length, Some(1_000_000));
    }

    #[test]
    fn context_length_falls_back_to_the_top_level_field() {
        let body = r#"{
            "data": [
                {"id": "meta-llama/llama-3.3-70b-instruct", "context_length": 131072}
            ]
        }"#;

        let models =
            models_from_body(CatalogProvider::OpenRouter, body).expect("body should parse");

        assert_eq!(models[0].context_length, Some(131_072));
    }

    #[test]
    fn an_unknown_field_does_not_break_the_listing() {
        // A provider adding a field must not cost every user their dropdown.
        let body = r#"{
            "data": [
                {"id": "gpt-4o", "some_field_we_have_never_seen": {"nested": true}}
            ],
            "an_unknown_envelope_field": 7
        }"#;

        let models = models_from_body(CatalogProvider::OpenAI, body).expect("body should parse");

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "gpt-4o");
    }

    #[test]
    fn a_malformed_body_is_an_error_not_a_panic() {
        let body = "<html>502 Bad Gateway</html>";

        assert!(models_from_body(CatalogProvider::OpenAI, body).is_err());
    }
}
