//! Hardcoded model lists for the OpenAI-shaped providers, used only when the
//! provider's own `/v1/models` endpoint cannot be reached (no network, or an
//! API key the provider rejects before the request even lands).
//!
//! These lists go stale — a model here being retired or superseded upstream
//! is expected, not a bug, and is not by itself a reason to delete the list.
//! The list exists so a user with no network, or whose key is invalid, still
//! sees a usable dropdown instead of an empty one. The only caller is
//! [`super::list_models`], which reaches for these only after the live
//! endpoint has failed.
//!
//! The frontend carries its own copy of this same idea for the case where
//! the Tauri command itself cannot be invoked: see
//! `frontend/src/components/settings/ModelConfigForm.tsx`.

use super::{CatalogModel, CatalogProvider};

/// OpenAI's offline fallback list. Copied verbatim from the `FALLBACK_MODELS`
/// constant in `openai/openai.rs` (which a later cleanup removes now that
/// this copy is the one `list_models` uses) — not curated, reordered or
/// extended here.
const OPENAI_FALLBACK_MODELS: &[&str] = &[
    "gpt-5",
    "gpt-5-mini",
    "gpt-4o",
    "gpt-4.1",
    "gpt-4-turbo",
    "gpt-3.5-turbo",
    "gpt-4o-2024-11-20",
    "gpt-4o-2024-08-06",
    "gpt-4o-mini-2024-07-18",
    "gpt-4.1-2025-04-14",
    "gpt-4.1-nano-2025-04-14",
    "gpt-4.1-mini-2025-04-14",
    "o4-mini-2025-04-16",
    "o3-2025-04-16",
    "o3-mini-2025-01-31",
    "o1-2024-12-17",
    "o1-mini-2024-09-12",
    "gpt-4-turbo-2024-04-09",
    "gpt-4-0125-Preview",
    "gpt-4-vision-preview",
    "gpt-4-1106-Preview",
    "gpt-3.5-turbo-0125",
    "gpt-3.5-turbo-1106",
];

/// Groq's offline fallback list. Copied from the `FALLBACK_MODELS` constant
/// in `groq/groq.rs`. The frontend's `ModelConfigForm.tsx` carries a
/// slightly longer Groq list for its own no-Tauri-command fallback path;
/// that is a separate list for a separate failure mode and is left alone
/// here, not reconciled with this one.
const GROQ_FALLBACK_MODELS: &[&str] = &["llama-3.3-70b-versatile"];

/// OpenRouter's offline fallback list.
///
/// Deliberately four long-lived ids rather than a mirror of the live
/// catalog: OpenRouter lists hundreds of models and reshuffles them weekly,
/// and its models endpoint needs no API key, so this path is only reached
/// when the machine is genuinely offline. A stale copy of a long list would
/// be worse than a short, current one — these four are chosen to still be
/// good choices a long time from now.
const OPENROUTER_FALLBACK_MODELS: &[&str] = &[
    "anthropic/claude-sonnet-4.5",
    "openai/gpt-4o",
    "google/gemini-2.5-pro",
    "meta-llama/llama-3.3-70b-instruct",
];

/// The offline fallback list for `provider`, as `CatalogModel`s known only
/// by id.
pub fn fallback_models(provider: CatalogProvider) -> Vec<CatalogModel> {
    let ids: &[&str] = match provider {
        CatalogProvider::OpenAI => OPENAI_FALLBACK_MODELS,
        CatalogProvider::Groq => GROQ_FALLBACK_MODELS,
        CatalogProvider::OpenRouter => OPENROUTER_FALLBACK_MODELS,
    };
    ids.iter().map(|id| CatalogModel::from_id(*id)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_provider_has_a_non_empty_fallback_list() {
        for provider in [
            CatalogProvider::OpenAI,
            CatalogProvider::Groq,
            CatalogProvider::OpenRouter,
        ] {
            assert!(
                !fallback_models(provider).is_empty(),
                "{provider:?} fallback list must not be empty"
            );
        }
    }

    #[test]
    fn openai_leads_with_gpt5_and_matches_the_const_len() {
        let models = fallback_models(CatalogProvider::OpenAI);
        assert_eq!(models.len(), OPENAI_FALLBACK_MODELS.len());
        assert_eq!(models[0].id, "gpt-5");
    }

    #[test]
    fn every_id_is_non_empty_and_has_no_whitespace() {
        for provider in [
            CatalogProvider::OpenAI,
            CatalogProvider::Groq,
            CatalogProvider::OpenRouter,
        ] {
            for model in fallback_models(provider) {
                assert!(!model.id.is_empty(), "{provider:?} has an empty id");
                assert!(
                    !model.id.chars().any(char::is_whitespace),
                    "{provider:?} id {:?} contains whitespace",
                    model.id
                );
            }
        }
    }

    #[test]
    fn openrouter_ids_are_vendor_qualified() {
        for model in fallback_models(CatalogProvider::OpenRouter) {
            assert!(
                model.id.contains('/'),
                "OpenRouter id {:?} is missing its vendor prefix",
                model.id
            );
        }
    }
}
