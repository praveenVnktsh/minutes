//! The `get_groq_models` Tauri command and the shape it hands to the frontend.
//!
//! The model list itself — live fetch, offline fallback and caching — lives
//! in [`crate::model_catalog`]; this file only narrows
//! [`model_catalog::CatalogModel`] down to what Groq's frontend caller
//! already expects.

use serde::{Deserialize, Serialize};
use tauri::command;

use crate::model_catalog::{self, CatalogProvider};

/// Groq model information returned to frontend
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GroqModel {
    pub id: String,
    pub owned_by: Option<String>,
}

/// Fetch Groq models from the catalog.
///
/// # Arguments
/// * `api_key` - Groq API key
///
/// # Returns
/// Vector of available models. Never fails: with no key, an unreachable
/// API, or a rejected key, [`model_catalog::list_models`] falls back to its
/// offline list, so the dropdown is never empty.
#[command]
pub async fn get_groq_models(api_key: Option<String>) -> Result<Vec<GroqModel>, String> {
    let models = model_catalog::list_models(CatalogProvider::Groq, api_key.as_deref()).await;

    Ok(models
        .into_iter()
        .map(|m| GroqModel {
            id: m.id,
            owned_by: m.owned_by,
        })
        .collect())
}

/// Clear the models cache (useful when API key changes)
pub fn clear_cache() {
    model_catalog::cache::clear(CatalogProvider::Groq);
    log::info!("Groq models cache cleared");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_catalog::CatalogModel;

    #[test]
    fn catalog_model_maps_to_groq_model_keeping_id_and_owned_by() {
        let catalog_model = CatalogModel {
            id: "llama-3.3-70b-versatile".to_string(),
            name: None,
            owned_by: Some("Meta".to_string()),
            context_length: Some(131072),
            prompt_price: None,
            completion_price: None,
        };

        let groq_model = GroqModel {
            id: catalog_model.id.clone(),
            owned_by: catalog_model.owned_by.clone(),
        };

        assert_eq!(groq_model.id, "llama-3.3-70b-versatile");
        assert_eq!(groq_model.owned_by.as_deref(), Some("Meta"));
    }
}
