//! OpenRouter's model listing, as a thin shape adapter over
//! [`crate::model_catalog`].
//!
//! This file used to own the whole path: a `reqwest::blocking::Client` inside
//! an async Tauri command, no API key, no cache, no capability filter and an
//! `Err` on any failure — which the settings form turns into a red banner and
//! an empty dropdown, leaving OpenRouter unusable offline. All of that now
//! lives once in `model_catalog`, shared with OpenAI and Groq, and what is
//! left here is the mapping from [`CatalogModel`] to the `OpenRouterModel`
//! shape the frontend already reads.
//!
//! `api_key` is optional rather than required because OpenRouter's listing
//! endpoint needs no key: without one it serves the public catalog, and with
//! one it serves the account-aware list. So a user who has not pasted a key
//! yet still gets a real, live dropdown, and a user who has gets the models
//! their account can actually reach.

use serde::{Deserialize, Serialize};
use tauri::command;

use crate::model_catalog::{self, CatalogModel, CatalogProvider};

/// One OpenRouter model as the settings form consumes it.
///
/// The frontend reads `id` and `name` off each entry to populate the model
/// dropdown, and the remaining fields to show context window and pricing.
#[derive(Debug, Serialize, Deserialize)]
pub struct OpenRouterModel {
    pub id: String,
    pub name: String,
    pub context_length: Option<u32>,
    pub prompt_price: Option<String>,
    pub completion_price: Option<String>,
}

/// Narrows a catalog entry to OpenRouter's own shape.
///
/// Split out of the command so the mapping is testable without a network.
///
/// An unnamed model falls back to its id. The pre-catalog code substituted
/// the literal `"Unknown"` here, which put the word "Unknown" in the user's
/// dropdown for any model OpenRouter had not labelled — the id is always the
/// more useful thing to show, and it is what the user pastes into a config
/// anyway.
fn to_openrouter_model(model: CatalogModel) -> OpenRouterModel {
    let CatalogModel {
        id,
        name,
        context_length,
        prompt_price,
        completion_price,
        ..
    } = model;

    OpenRouterModel {
        name: name.unwrap_or_else(|| id.clone()),
        id,
        context_length,
        prompt_price,
        completion_price,
    }
}

/// The OpenRouter models available for summarisation.
///
/// # Arguments
/// * `api_key` - the user's OpenRouter key, if they have entered one. Absent
///   is a normal case, not an error: see the module docs.
///
/// # Returns
/// Always `Ok`. [`model_catalog::list_models`] is infallible and falls back to
/// the offline list when OpenRouter cannot be reached, so there is no failure
/// left to report — but the `Result` stays in the signature to match the other
/// providers' commands and the frontend's `invoke` typing.
#[command]
pub async fn get_openrouter_models(
    api_key: Option<String>,
) -> Result<Vec<OpenRouterModel>, String> {
    let models = model_catalog::list_models(CatalogProvider::OpenRouter, api_key.as_deref()).await;

    Ok(models.into_iter().map(to_openrouter_model).collect())
}

/// Clears the cached OpenRouter listing, so the next call goes to the network.
///
/// Called when the user's API key changes: the list an anonymous request
/// returns is not the list their account sees. Not a Tauri command.
pub fn clear_cache() {
    model_catalog::cache::clear(CatalogProvider::OpenRouter);
    log::info!("OpenRouter models cache cleared");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_named_model_keeps_its_name() {
        let model = CatalogModel {
            name: Some("Anthropic: Claude Sonnet 4.5".to_string()),
            ..CatalogModel::from_id("anthropic/claude-sonnet-4.5")
        };

        let mapped = to_openrouter_model(model);

        assert_eq!(mapped.id, "anthropic/claude-sonnet-4.5");
        assert_eq!(mapped.name, "Anthropic: Claude Sonnet 4.5");
    }

    #[test]
    fn an_unnamed_model_falls_back_to_its_id() {
        // Not to the literal "Unknown", which is what the pre-catalog code
        // showed and which tells the user nothing about what they are picking.
        let mapped =
            to_openrouter_model(CatalogModel::from_id("meta-llama/llama-3.3-70b-instruct"));

        assert_eq!(mapped.name, "meta-llama/llama-3.3-70b-instruct");
        assert_eq!(mapped.id, "meta-llama/llama-3.3-70b-instruct");
    }

    #[test]
    fn context_length_and_prices_survive_the_mapping() {
        let model = CatalogModel {
            context_length: Some(1_000_000),
            prompt_price: Some("0.000003".to_string()),
            completion_price: Some("0.000015".to_string()),
            ..CatalogModel::from_id("anthropic/claude-sonnet-4.5")
        };

        let mapped = to_openrouter_model(model);

        assert_eq!(mapped.context_length, Some(1_000_000));
        assert_eq!(mapped.prompt_price.as_deref(), Some("0.000003"));
        assert_eq!(mapped.completion_price.as_deref(), Some("0.000015"));
    }

    #[test]
    fn absent_context_and_prices_stay_absent() {
        let mapped = to_openrouter_model(CatalogModel::from_id("openai/gpt-4o"));

        assert_eq!(mapped.context_length, None);
        assert_eq!(mapped.prompt_price, None);
        assert_eq!(mapped.completion_price, None);
    }
}
