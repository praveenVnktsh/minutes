//! Tauri command surface for OpenAI's model list.
//!
//! The actual fetching, caching and offline fallback live in
//! `model_catalog` (see `model_catalog/mod.rs`) — this file only narrows
//! [`model_catalog::CatalogModel`] down to the shape the frontend's
//! `ModelConfigForm.tsx` already expects (`item.id`), and exposes it as the
//! `get_openai_models` command the frontend invokes today.

use crate::model_catalog::{self, CatalogProvider};
use serde::{Deserialize, Serialize};
use tauri::command;

/// OpenAI model information returned to frontend
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct OpenAIModel {
    pub id: String,
}

/// Fetch OpenAI models from API
///
/// # Arguments
/// * `api_key` - OpenAI API key
///
/// # Returns
/// Vector of available models. Always `Ok`: `model_catalog::list_models` is
/// infallible and falls back to its offline list on any failure, so a user
/// with no network never sees this surface as a thrown error.
#[command]
pub async fn get_openai_models(api_key: Option<String>) -> Result<Vec<OpenAIModel>, String> {
    let models = model_catalog::list_models(CatalogProvider::OpenAI, api_key.as_deref()).await;
    Ok(models
        .into_iter()
        .map(|m| OpenAIModel { id: m.id })
        .collect())
}

/// Clear the models cache (useful when API key changes)
pub fn clear_cache() {
    model_catalog::cache::clear(CatalogProvider::OpenAI);
    log::info!("OpenAI models cache cleared");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_catalog::CatalogModel;

    #[test]
    fn catalog_model_maps_to_openai_model_preserving_id() {
        let catalog_model = CatalogModel::from_id("gpt-4o");
        let openai_model = OpenAIModel {
            id: catalog_model.id.clone(),
        };
        assert_eq!(openai_model.id, "gpt-4o");
    }
}
