//! Per-provider request construction: where each provider's models endpoint
//! and chat-completions endpoint live, and what authenticates a request to
//! them.
//!
//! Before this module, a provider's base URL was inlined once in the code
//! that lists its models and again in `summary/llm_client.rs`, wherever a
//! chat completion is sent. Two copies of the same URL drift apart the first
//! time only one of them gets updated. Centralising both endpoints — and the
//! auth headers every caller needs — here means every provider's address is
//! defined exactly once.

use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};

use super::CatalogProvider;

/// Where `provider` publishes its list of available models.
pub fn models_url(provider: CatalogProvider) -> &'static str {
    match provider {
        CatalogProvider::OpenAI => "https://api.openai.com/v1/models",
        CatalogProvider::Groq => "https://api.groq.com/openai/v1/models",
        CatalogProvider::OpenRouter => "https://openrouter.ai/api/v1/models",
    }
}

/// Where `provider` accepts chat-completion requests.
pub fn chat_completions_url(provider: CatalogProvider) -> &'static str {
    match provider {
        CatalogProvider::OpenAI => "https://api.openai.com/v1/chat/completions",
        CatalogProvider::Groq => "https://api.groq.com/openai/v1/chat/completions",
        CatalogProvider::OpenRouter => "https://openrouter.ai/api/v1/chat/completions",
    }
}

/// Whether `provider` refuses a models-listing request with no API key.
///
/// OpenAI and Groq do. OpenRouter serves its public catalog unauthenticated
/// — which is why the pre-catalog `get_openrouter_models` took no key at
/// all — but a key is still worth sending when the user has one, since an
/// authenticated listing reflects models their account can actually use.
pub fn requires_api_key(provider: CatalogProvider) -> bool {
    match provider {
        CatalogProvider::OpenAI | CatalogProvider::Groq => true,
        CatalogProvider::OpenRouter => false,
    }
}

/// The headers that authenticate a request to `provider`.
///
/// All three providers are Bearer-token. When `api_key` is `Some` and
/// non-empty after trimming, this returns a single `Authorization: Bearer
/// <key>` header; otherwise it returns an empty map, so a caller can insert
/// these headers unconditionally without checking `api_key` itself first.
///
/// An API key is user input pasted from elsewhere, and a stray character —
/// most commonly a trailing newline — can make it invalid as a header value.
/// `HeaderValue::from_str` rejects that instead of panicking, and this
/// function just drops the header in that case rather than propagating the
/// error, since a request sent without auth fails the same way a request
/// with a malformed key would: the provider rejects it and the caller falls
/// back to the offline model list.
pub fn auth_headers(provider: CatalogProvider, api_key: Option<&str>) -> HeaderMap {
    // Every current provider is Bearer-token, so `provider` doesn't branch
    // this yet. It stays a parameter because the day one provider signs
    // requests differently, this is where that split belongs.
    let _ = provider;

    let mut headers = HeaderMap::new();
    let Some(key) = api_key else {
        return headers;
    };
    let key = key.trim();
    if key.is_empty() {
        return headers;
    }

    match HeaderValue::from_str(&format!("Bearer {key}")) {
        Ok(value) => {
            headers.insert(AUTHORIZATION, value);
        }
        Err(err) => {
            log::warn!("model_catalog::endpoints: dropping unparsable API key header: {err}");
        }
    }

    headers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_url_matches_each_provider() {
        assert_eq!(
            models_url(CatalogProvider::OpenAI),
            "https://api.openai.com/v1/models"
        );
        assert_eq!(
            models_url(CatalogProvider::Groq),
            "https://api.groq.com/openai/v1/models"
        );
        assert_eq!(
            models_url(CatalogProvider::OpenRouter),
            "https://openrouter.ai/api/v1/models"
        );
    }

    #[test]
    fn chat_completions_url_matches_each_provider() {
        assert_eq!(
            chat_completions_url(CatalogProvider::OpenAI),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_url(CatalogProvider::Groq),
            "https://api.groq.com/openai/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_url(CatalogProvider::OpenRouter),
            "https://openrouter.ai/api/v1/chat/completions"
        );
    }

    #[test]
    fn only_openrouter_allows_no_key() {
        assert!(requires_api_key(CatalogProvider::OpenAI));
        assert!(requires_api_key(CatalogProvider::Groq));
        assert!(!requires_api_key(CatalogProvider::OpenRouter));
    }

    #[test]
    fn auth_headers_sets_bearer_token_for_a_key() {
        let headers = auth_headers(CatalogProvider::OpenAI, Some("sk-test-123"));
        assert_eq!(headers.get(AUTHORIZATION).unwrap(), "Bearer sk-test-123");
    }

    #[test]
    fn auth_headers_is_empty_for_no_key() {
        let headers = auth_headers(CatalogProvider::OpenRouter, None);
        assert!(headers.is_empty());
    }

    #[test]
    fn auth_headers_is_empty_for_whitespace_only_key() {
        let headers = auth_headers(CatalogProvider::Groq, Some("   "));
        assert!(headers.is_empty());
    }

    #[test]
    fn auth_headers_does_not_panic_on_a_newline_in_the_key() {
        // A pasted key can carry a trailing newline; that must be handled,
        // not crash the app.
        let headers = auth_headers(CatalogProvider::OpenAI, Some("sk-test\n123"));
        assert!(headers.is_empty());
    }
}
