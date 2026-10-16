//! Decides which of a provider's models can actually summarise a transcript.
//!
//! A provider's `GET /v1/models` returns everything the account can reach:
//! embeddings, speech-to-text, text-to-speech, image and video generators,
//! moderation classifiers, rerankers and realtime voice endpoints. None of
//! those can summarise a meeting, so every one that reaches the dropdown is a
//! way for a user to pick a model that will simply fail at summary time.
//!
//! The shape is **deny-list for all three providers, plus an allow-list that
//! applies to OpenAI only**, because the three naming schemes are not alike:
//!
//! * OpenAI's families are few and stable (`gpt-`, `o1-`, `o3-`, `o4-`,
//!   `chatgpt-`), so an allow-list is both safe and precise there — and it is
//!   the behaviour PRA-492 shipped, which this module must not regress.
//! * Groq serves open-weights models whose ids share no prefix at all
//!   (`llama-3.3-70b-versatile`, `mixtral-8x7b-32768`, `qwen-…`). An allow-list
//!   would reject the catalog wholesale.
//! * OpenRouter ids are `vendor/model` (`anthropic/claude-sonnet-4.5`,
//!   `meta-llama/llama-3.3-70b-instruct`), so an OpenAI-shaped prefix check
//!   would reject literally every entry.
//!
//! Matching is on id *segments*, not bare substrings: a term only counts when
//! it is bounded by a non-alphanumeric character (`-`, `/`, `.`, `_`) or the
//! ends of the id. `guard` must catch `llama-guard-3-8b` without catching a
//! model that merely has the letters in a longer word, and `embed` must not
//! half-match `embedding` into a false sense of coverage.

use super::CatalogProvider;

/// Capability markers that disqualify a model on every provider.
///
/// **Rule 1 — this list must not eat a capable model.** Nothing here may be
/// part of the ordinary *name* of a chat model on any of the three providers.
/// That is why `instruct` is absent: it disqualifies a model on OpenAI
/// (`gpt-3.5-turbo-instruct` is a completion endpoint) but is part of the name
/// of most capable open-weights chat models — `meta-llama/llama-3.3-70b-instruct`
/// on OpenRouter, and Groq's own instruct variants. Moving `instruct` here
/// empties the OpenRouter dropdown; it lives in `OPENAI_DENY` instead.
///
/// `vision` is absent for the same reason: a vision-*capable* chat model such
/// as `llama-3.2-90b-vision-preview` summarises text perfectly well. Only
/// image *generators* are denied, and those are caught by `image`/`dall-e`.
const SHARED_DENY: &[&str] = &[
    // Embeddings. Both spellings: segment matching means `embed` does not
    // cover `text-embedding-3-large`.
    "embed",
    "embedding",
    "embeddings",
    // Speech in and out.
    "tts",
    "stt",
    "whisper",
    "transcribe",
    "transcription",
    "speech",
    "audio",
    "voice",
    "realtime",
    // Image generation.
    "dall-e",
    "image",
    "imagen",
    "flux",
    "stable-diffusion",
    // Video and music families OpenRouter carries.
    "video",
    "sora",
    "veo",
    "music",
    // Classifiers and rankers, which answer with a label rather than prose.
    "moderation",
    "guard",
    "rerank",
    "reranker",
];

/// OpenAI model families that can hold a conversation.
///
/// Carried over verbatim from `openai::is_chat_model` (PRA-492). Widening it
/// is a deliberate decision, not a tidy-up: a family added here that OpenAI
/// does not serve is harmless, but one removed silently hides working models.
const OPENAI_ALLOW_PREFIXES: &[&str] = &["gpt-", "o1-", "o3-", "o4-", "chatgpt-"];

/// Denials that hold for OpenAI and would be wrong anywhere else.
///
/// `instruct` is rule 1's exception (see `SHARED_DENY`). `babbage` and
/// `davinci` are OpenAI's legacy base models; they already fail the allow-list,
/// and are kept so this module's behaviour matches PRA-492 term for term.
const OPENAI_DENY: &[&str] = &["instruct", "babbage", "davinci"];

/// Denials specific to Groq, whose catalog carries tool-calling-only variants
/// (`llama3-groq-70b-8192-tool-use-preview`) that do not produce free prose.
const GROQ_DENY: &[&str] = &["tool-use"];

/// Whether `model_id` names a model that can summarise a meeting transcript.
///
/// Provider-aware on purpose: see the module docs for why OpenAI gets an
/// allow-list and the other two cannot have one.
pub fn is_summarisation_capable(provider: CatalogProvider, model_id: &str) -> bool {
    let id = model_id.trim().to_lowercase();
    if id.is_empty() {
        return false;
    }

    if SHARED_DENY.iter().any(|term| contains_segment(&id, term)) {
        return false;
    }

    match provider {
        CatalogProvider::OpenAI => {
            OPENAI_ALLOW_PREFIXES
                .iter()
                .any(|prefix| id.starts_with(prefix))
                && !OPENAI_DENY.iter().any(|term| contains_segment(&id, term))
        }
        CatalogProvider::Groq => !GROQ_DENY.iter().any(|term| contains_segment(&id, term)),
        CatalogProvider::OpenRouter => true,
    }
}

/// Whether `term` appears in `id` as a whole segment rather than as a bare
/// substring — bounded on both sides by a non-alphanumeric character or by the
/// ends of the id.
///
/// Bare `contains` over-matches across three naming schemes: it would let
/// `embed` stand in for `embedding`, and it would deny any future model whose
/// name merely happens to spell a capability marker inside a longer word. The
/// term itself may contain delimiters (`dall-e`, `tool-use`); only its outer
/// edges are checked.
fn contains_segment(id: &str, term: &str) -> bool {
    id.match_indices(term).any(|(start, matched)| {
        let end = start + matched.len();
        // `map_or` rather than `is_none_or`: the crate declares an MSRV of
        // 1.77 and `Option::is_none_or` only stabilised in 1.82.
        let boundary_before = id[..start]
            .chars()
            .next_back()
            .map_or(true, |c| !c.is_ascii_alphanumeric());
        let boundary_after = id[end..]
            .chars()
            .next()
            .map_or(true, |c| !c.is_ascii_alphanumeric());
        boundary_before && boundary_after
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_chat_families_pass() {
        for id in [
            "gpt-4o",
            "gpt-4o-mini",
            "gpt-5",
            "gpt-4.1",
            "o1-preview",
            "o3-2025-04-16",
            "o4-mini",
            "chatgpt-4o-latest",
        ] {
            assert!(
                is_summarisation_capable(CatalogProvider::OpenAI, id),
                "{id} should be offered for OpenAI"
            );
        }
    }

    #[test]
    fn openai_non_chat_models_are_rejected() {
        for id in [
            "text-embedding-3-large",
            "text-embedding-ada-002",
            "tts-1",
            "gpt-4o-mini-tts",
            "whisper-1",
            "gpt-4o-transcribe",
            "dall-e-3",
            "gpt-image-1",
            "gpt-3.5-turbo-instruct",
            "gpt-4o-realtime-preview",
            "gpt-4o-audio-preview",
            "babbage-002",
            "davinci-002",
            "omni-moderation-latest",
        ] {
            assert!(
                !is_summarisation_capable(CatalogProvider::OpenAI, id),
                "{id} should not reach the OpenAI dropdown"
            );
        }
    }

    #[test]
    fn openai_allow_list_rejects_unknown_families() {
        // The allow-list is deliberately narrow: anything outside OpenAI's own
        // families is not an OpenAI chat model.
        assert!(!is_summarisation_capable(
            CatalogProvider::OpenAI,
            "llama-3.3-70b-versatile"
        ));
    }

    #[test]
    fn groq_open_weights_models_pass() {
        for id in [
            "llama-3.3-70b-versatile",
            "llama-3.1-8b-instant",
            "mixtral-8x7b-32768",
            "qwen-2.5-32b",
            "gemma2-9b-it",
            "llama-3.2-90b-vision-preview",
        ] {
            assert!(
                is_summarisation_capable(CatalogProvider::Groq, id),
                "{id} should be offered for Groq"
            );
        }
    }

    #[test]
    fn groq_non_chat_models_are_rejected() {
        for id in [
            "whisper-large-v3",
            "whisper-large-v3-turbo",
            "distil-whisper-large-v3-en",
            "llama-guard-3-8b",
            "nomic-embed-text-v1.5",
            "text-embedding-3-large",
            "playai-tts",
            "llama3-groq-70b-8192-tool-use-preview",
        ] {
            assert!(
                !is_summarisation_capable(CatalogProvider::Groq, id),
                "{id} should not reach the Groq dropdown"
            );
        }
    }

    #[test]
    fn openrouter_vendor_prefixed_models_pass() {
        for id in [
            "openai/gpt-4o",
            "anthropic/claude-sonnet-4.5",
            "meta-llama/llama-3.3-70b-instruct",
            "google/gemini-2.5-pro",
            "mistralai/mistral-large",
            "deepseek/deepseek-chat",
        ] {
            assert!(
                is_summarisation_capable(CatalogProvider::OpenRouter, id),
                "{id} should be offered for OpenRouter"
            );
        }
    }

    #[test]
    fn openrouter_non_chat_models_are_rejected() {
        for id in [
            "openai/text-embedding-3-large",
            "openai/whisper-1",
            "openai/dall-e-3",
            "openai/gpt-4o-audio-preview",
            "google/veo-3",
            "black-forest-labs/flux-1.1-pro",
            "meta-llama/llama-guard-4-12b",
            "cohere/rerank-v3.5",
        ] {
            assert!(
                !is_summarisation_capable(CatalogProvider::OpenRouter, id),
                "{id} should not reach the OpenRouter dropdown"
            );
        }
    }

    #[test]
    fn instruct_is_denied_for_openai_only() {
        // Rule 1: `instruct` marks a legacy completion endpoint at OpenAI but
        // is part of the name of most capable open-weights chat models. If this
        // test ever fails because `instruct` moved into SHARED_DENY, the
        // OpenRouter and Groq dropdowns have just been emptied.
        assert!(!is_summarisation_capable(
            CatalogProvider::OpenAI,
            "gpt-3.5-turbo-instruct"
        ));
        assert!(is_summarisation_capable(
            CatalogProvider::OpenRouter,
            "meta-llama/llama-3.3-70b-instruct"
        ));
        assert!(is_summarisation_capable(
            CatalogProvider::Groq,
            "llama-3.1-70b-instruct"
        ));
    }

    #[test]
    fn deny_terms_match_segments_not_substrings() {
        // `embed` as a bare substring would also be claimed to cover
        // `embedding`; both spellings are listed precisely because it does not.
        assert!(contains_segment("nomic-embed-text-v1.5", "embed"));
        assert!(!contains_segment("text-embedding-3-large", "embed"));
        assert!(contains_segment("text-embedding-3-large", "embedding"));

        // A term buried inside a longer word is not a capability marker.
        assert!(!contains_segment("safeguarded-model-v1", "guard"));
        assert!(contains_segment("llama-guard-3-8b", "guard"));

        // Terms may carry their own delimiters.
        assert!(contains_segment("dall-e-3", "dall-e"));
        assert!(contains_segment("openai/dall-e-2", "dall-e"));
    }

    #[test]
    fn blank_ids_are_rejected() {
        for provider in [
            CatalogProvider::OpenAI,
            CatalogProvider::Groq,
            CatalogProvider::OpenRouter,
        ] {
            assert!(!is_summarisation_capable(provider, ""));
            assert!(!is_summarisation_capable(provider, "   "));
        }
    }

    #[test]
    fn matching_is_case_insensitive() {
        assert!(is_summarisation_capable(CatalogProvider::OpenAI, "GPT-4o"));
        assert!(!is_summarisation_capable(
            CatalogProvider::OpenRouter,
            "OpenAI/Whisper-1"
        ));
    }
}
