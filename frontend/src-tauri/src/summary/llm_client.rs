use crate::summary::budget::{SUMMARY_SEED, SUMMARY_TEMPERATURE};
use reqwest::{header, Client};
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

const REQUEST_TIMEOUT_DURATION: Duration = Duration::from_secs(300);

/// Output budget for Claude. On models with thinking on by default it covers
/// thinking tokens as well as the answer, so a meeting-length summary has to
/// fit in what is left. 8192 truncated it. The request is not streamed, and a
/// full budget still returns inside REQUEST_TIMEOUT_DURATION.
const CLAUDE_MAX_TOKENS: u32 = 16_000;

async fn await_or_cancel<T>(
    operation: impl Future<Output = T>,
    cancellation_token: Option<&CancellationToken>,
) -> Result<T, String> {
    let Some(token) = cancellation_token else {
        return Ok(operation.await);
    };

    tokio::select! {
        biased;
        _ = token.cancelled() => Err("Summary generation was cancelled".to_string()),
        result = operation => Ok(result),
    }
}

/// Per-call generation settings.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct GenerationOptions {
    /// Output budget. A user setting for custom OpenAI endpoints; the budgeted
    /// output size for built-in AI and Ollama. Ignored for the hosted APIs.
    pub max_tokens: Option<u32>,
    /// User-configured temperature (custom OpenAI endpoints only). Every other
    /// provider that accepts a temperature gets `SUMMARY_TEMPERATURE`.
    pub temperature: Option<f32>,
    /// User-configured top_p (custom OpenAI endpoints only).
    pub top_p: Option<f32>,
    /// Context window to run Ollama with (`options.num_ctx`).
    pub num_ctx: Option<u32>,
}

// Generic structure for OpenAI-compatible API chat messages
#[derive(Debug, Serialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

// Generic structure for OpenAI-compatible API chat requests
#[derive(Debug, Serialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<u32>,
}

fn chat_messages(system_prompt: &str, user_prompt: &str) -> Vec<ChatMessage> {
    vec![
        ChatMessage {
            role: "system".to_string(),
            content: system_prompt.to_string(),
        },
        ChatMessage {
            role: "user".to_string(),
            content: user_prompt.to_string(),
        },
    ]
}

/// Build OpenAI-compat JSON body.
///
/// Hosted providers get a low fixed temperature and seed so the same transcript
/// summarises the same way. Custom endpoints keep the user's settings and only
/// fall back to the summary temperature when none is set.
pub(crate) fn build_openai_compat_chat_body(
    provider: &LLMProvider,
    model_name: &str,
    system_prompt: &str,
    user_prompt: &str,
    options: &GenerationOptions,
) -> serde_json::Value {
    let (max_tokens, temperature, top_p, seed) = if *provider == LLMProvider::CustomOpenAI {
        (
            options.max_tokens,
            Some(options.temperature.unwrap_or(SUMMARY_TEMPERATURE)),
            options.top_p,
            None,
        )
    } else {
        (None, Some(SUMMARY_TEMPERATURE), None, Some(SUMMARY_SEED))
    };

    serde_json::json!(ChatRequest {
        model: model_name.to_string(),
        messages: chat_messages(system_prompt, user_prompt),
        max_tokens,
        temperature,
        top_p,
        seed,
    })
}

/// Build the body for Ollama's native `/api/chat`. Unlike the OpenAI-compatible
/// endpoint it accepts `options.num_ctx`; without it Ollama runs at its small
/// server default and silently drops the start of long prompts.
pub(crate) fn build_ollama_chat_body(
    model_name: &str,
    system_prompt: &str,
    user_prompt: &str,
    options: &GenerationOptions,
) -> serde_json::Value {
    let mut model_options = serde_json::json!({
        "temperature": SUMMARY_TEMPERATURE,
        "seed": SUMMARY_SEED,
    });
    if let Some(num_ctx) = options.num_ctx {
        model_options["num_ctx"] = num_ctx.into();
    }
    if let Some(max_tokens) = options.max_tokens {
        model_options["num_predict"] = max_tokens.into();
    }
    serde_json::json!({
        "model": model_name,
        "messages": chat_messages(system_prompt, user_prompt),
        "stream": false,
        "think": false,
        "options": model_options,
    })
}

// Generic structure for OpenAI-compatible API chat responses
#[derive(Deserialize, Debug)]
pub struct ChatResponse {
    pub choices: Vec<Choice>,
}

#[derive(Deserialize, Debug)]
pub struct Choice {
    pub message: MessageContent,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct MessageContent {
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub reasoning: Option<String>,
    #[serde(default)]
    pub reasoning_content: Option<String>,
}

/// Ollama native `/api/chat` non-streaming response.
#[derive(Deserialize, Debug)]
pub struct OllamaChatResponse {
    pub message: OllamaMessage,
    #[serde(default)]
    pub done_reason: Option<String>,
    #[serde(default)]
    pub prompt_eval_count: Option<u32>,
}

#[derive(Deserialize, Debug)]
pub struct OllamaMessage {
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub thinking: Option<String>,
}

impl OllamaChatResponse {
    fn completion(&self) -> LlmCompletion {
        LlmCompletion {
            content: self
                .message
                .content
                .as_deref()
                .unwrap_or_default()
                .trim()
                .to_string(),
            reasoning_stripped: self
                .message
                .thinking
                .as_deref()
                .is_some_and(|thinking| !thinking.trim().is_empty()),
            truncated: self.done_reason.as_deref() == Some("length"),
            prompt_tokens: self.prompt_eval_count,
        }
    }
}

// Claude-specific request structure
#[derive(Debug, Serialize)]
pub struct ClaudeRequest {
    pub model: String,
    pub max_tokens: u32,
    pub system: String,
    pub messages: Vec<ChatMessage>,
}

// Claude-specific response structure
#[derive(Deserialize, Debug)]
pub struct ClaudeChatResponse {
    pub content: Vec<ClaudeChatContent>,
    #[serde(default)]
    pub stop_reason: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct ClaudeChatContent {
    #[serde(rename = "type")]
    pub block_type: String,
    pub text: Option<String>,
}

impl ClaudeChatResponse {
    fn completion(&self) -> Option<LlmCompletion> {
        let content = self
            .content
            .iter()
            .find(|block| block.block_type == "text")
            .and_then(|block| block.text.as_deref())?
            .trim()
            .to_string();
        Some(LlmCompletion {
            content,
            reasoning_stripped: self
                .content
                .iter()
                .any(|block| matches!(block.block_type.as_str(), "thinking" | "redacted_thinking")),
            truncated: self.stop_reason.as_deref() == Some("max_tokens"),
            prompt_tokens: None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LlmCompletion {
    pub content: String,
    pub reasoning_stripped: bool,
    /// True when the provider stopped at its output limit rather than at the
    /// end of the answer, so `content` is incomplete.
    pub truncated: bool,
    /// Prompt tokens as counted by the provider, when it reports them.
    pub prompt_tokens: Option<u32>,
}

impl ChatResponse {
    fn completion(&self) -> Result<LlmCompletion, String> {
        let choice = self.choices.first().ok_or("No content in LLM response")?;
        let message = &choice.message;
        Ok(LlmCompletion {
            content: message
                .content
                .as_deref()
                .unwrap_or_default()
                .trim()
                .to_string(),
            reasoning_stripped: [
                message.reasoning.as_deref(),
                message.reasoning_content.as_deref(),
            ]
            .into_iter()
            .flatten()
            .any(|reasoning| !reasoning.trim().is_empty()),
            truncated: choice.finish_reason.as_deref() == Some("length"),
            prompt_tokens: None,
        })
    }
}

fn json_error_texts(body: &str) -> Option<(serde_json::Value, Vec<String>)> {
    let value = serde_json::from_str::<serde_json::Value>(body).ok()?;
    let mut texts: Vec<String> = [
        value.as_str(),
        value.get("error").and_then(serde_json::Value::as_str),
        value.get("message").and_then(serde_json::Value::as_str),
        value
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(serde_json::Value::as_str),
    ]
    .into_iter()
    .flatten()
    .map(str::to_ascii_lowercase)
    .collect();
    for field in [
        value.get("param"),
        value.get("field"),
        value.get("error").and_then(|error| error.get("param")),
        value.get("error").and_then(|error| error.get("field")),
    ]
    .into_iter()
    .flatten()
    .filter_map(serde_json::Value::as_str)
    {
        texts.push(format!("param:{}", field.to_ascii_lowercase()));
    }
    Some((value, texts))
}

/// True when Ollama rejected the request because of the `think` field (older
/// servers, or models that refuse to toggle thinking), so a retry without it
/// can succeed.
pub(crate) fn ollama_rejects_think(status: reqwest::StatusCode, body: &str) -> bool {
    if !matches!(status.as_u16(), 400 | 422) {
        return false;
    }
    let Some((_, texts)) = json_error_texts(body) else {
        return false;
    };
    texts.iter().any(|text| {
        text == "param:think"
            || text == "param:reasoning_effort"
            || text.contains("reasoning_effort")
            || text.contains("does not support thinking")
            || (text.contains("think value")
                && (text.contains("not supported") || text.contains("invalid")))
    })
}

/// Sampling fields named in a 400/422 rejection, e.g. OpenAI reasoning models
/// refusing `temperature`. A retry without them can succeed.
pub(crate) fn rejected_sampling_fields(status: reqwest::StatusCode, body: &str) -> Vec<&'static str> {
    if !matches!(status.as_u16(), 400 | 422) {
        return Vec::new();
    }
    let Some((_, texts)) = json_error_texts(body) else {
        return Vec::new();
    };
    ["temperature", "seed", "top_p"]
        .into_iter()
        .filter(|field| {
            texts
                .iter()
                .any(|text| text == &format!("param:{field}") || text.contains(field))
        })
        .collect()
}

/// LLM Provider enumeration for multi-provider support
#[derive(Debug, Clone, PartialEq)]
pub enum LLMProvider {
    OpenAI,
    Claude,
    Groq,
    Ollama,
    OpenRouter,
    BuiltInAI,
    CustomOpenAI,
}

impl LLMProvider {
    /// Parse provider from string (case-insensitive)
    // Keep this inherent API because callers outside the summary module use it directly.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Result<Self, String> {
        match s.to_lowercase().as_str() {
            "openai" => Ok(Self::OpenAI),
            "claude" => Ok(Self::Claude),
            "groq" => Ok(Self::Groq),
            "ollama" => Ok(Self::Ollama),
            "openrouter" => Ok(Self::OpenRouter),
            "builtin-ai" | "local-llama" | "localllama" => Ok(Self::BuiltInAI),
            "custom-openai" => Ok(Self::CustomOpenAI),
            _ => Err(format!("Unsupported LLM provider: {}", s)),
        }
    }
}

fn send_error(error: reqwest::Error, what: &str) -> String {
    if error.is_timeout() {
        format!(
            "LLM {what} timed out after {} seconds",
            REQUEST_TIMEOUT_DURATION.as_secs()
        )
    } else {
        format!("Failed to send {what} to LLM: {}", error)
    }
}

/// The compatibility retry body for a rejected request, if the rejection is
/// one a smaller request can avoid.
fn compatibility_retry_body(
    provider: &LLMProvider,
    request_body: &serde_json::Value,
    status: reqwest::StatusCode,
    error_body: &str,
) -> Option<serde_json::Value> {
    let fields: Vec<&str> = match provider {
        LLMProvider::Ollama if ollama_rejects_think(status, error_body) => {
            warn!("Ollama rejected think; retrying once without it");
            vec!["think"]
        }
        LLMProvider::OpenAI
        | LLMProvider::Groq
        | LLMProvider::OpenRouter
        | LLMProvider::CustomOpenAI => {
            let fields = rejected_sampling_fields(status, error_body);
            if !fields.is_empty() {
                warn!(
                    "{} rejected sampling fields {:?}; retrying once without them",
                    provider_name(provider),
                    fields
                );
            }
            fields
        }
        _ => Vec::new(),
    };
    let mut retry_body = request_body.clone();
    let object = retry_body.as_object_mut()?;
    let removed = fields
        .into_iter()
        .filter(|field| object.remove(*field).is_some())
        .count();
    (removed > 0).then_some(retry_body)
}

/// Generates a summary using the specified LLM provider
///
/// # Arguments
/// * `client` - Reqwest HTTP client (reused for performance)
/// * `provider` - The LLM provider to use
/// * `model_name` - The specific model to use (e.g., "gpt-4", "claude-3-opus")
/// * `api_key` - API key for the provider (not needed for Ollama)
/// * `system_prompt` - System instructions for the LLM
/// * `user_prompt` - User query/content to process
/// * `ollama_endpoint` - Optional custom Ollama endpoint (defaults to localhost:11434)
/// * `custom_openai_endpoint` - Optional custom OpenAI-compatible endpoint
/// * `options` - Output budget, user sampling overrides and Ollama context size
/// * `app_data_dir` - Optional app data directory (for BuiltInAI provider)
/// * `cancellation_token` - Optional token to cancel the request
///
/// The generated visible content, whether private reasoning was removed, and
/// whether the provider cut the output off at its limit.
// Provider-specific request options make this shared dispatch boundary inherently wide.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn generate_summary(
    client: &Client,
    provider: &LLMProvider,
    model_name: &str,
    api_key: &str,
    system_prompt: &str,
    user_prompt: &str,
    ollama_endpoint: Option<&str>,
    custom_openai_endpoint: Option<&str>,
    options: &GenerationOptions,
    app_data_dir: Option<&PathBuf>,
    cancellation_token: Option<&CancellationToken>,
) -> Result<LlmCompletion, String> {
    // Check if cancelled before starting
    if let Some(token) = cancellation_token {
        if token.is_cancelled() {
            return Err("Summary generation was cancelled".to_string());
        }
    }

    // Handle BuiltInAI provider separately (uses local sidecar, no HTTP API)
    if provider == &LLMProvider::BuiltInAI {
        let app_data_dir = app_data_dir
            .ok_or_else(|| "app_data_dir is required for BuiltInAI provider".to_string())?;

        return crate::summary::summary_engine::generate_with_builtin(
            app_data_dir,
            model_name,
            system_prompt,
            user_prompt,
            options.max_tokens,
            cancellation_token,
        )
        .await
        .map(|completion| LlmCompletion {
            content: completion.text,
            reasoning_stripped: false,
            truncated: completion.truncated,
            prompt_tokens: completion.prompt_tokens,
        })
        .map_err(|e| e.to_string());
    }

    let (api_url, mut headers) = match provider {
        LLMProvider::OpenAI => (
            "https://api.openai.com/v1/chat/completions".to_string(),
            header::HeaderMap::new(),
        ),
        LLMProvider::Groq => (
            "https://api.groq.com/openai/v1/chat/completions".to_string(),
            header::HeaderMap::new(),
        ),
        LLMProvider::OpenRouter => (
            "https://openrouter.ai/api/v1/chat/completions".to_string(),
            header::HeaderMap::new(),
        ),
        LLMProvider::Ollama => {
            let host = ollama_endpoint
                .map(|s| s.trim_end_matches('/').to_string())
                .unwrap_or_else(|| "http://localhost:11434".to_string());
            (format!("{}/api/chat", host), header::HeaderMap::new())
        }
        LLMProvider::CustomOpenAI => {
            let endpoint = custom_openai_endpoint
                .ok_or_else(|| "Custom OpenAI endpoint not configured".to_string())?;
            (
                format!("{}/chat/completions", endpoint.trim_end_matches('/')),
                header::HeaderMap::new(),
            )
        }
        LLMProvider::Claude => {
            let mut header_map = header::HeaderMap::new();
            header_map.insert(
                "x-api-key",
                api_key
                    .parse()
                    .map_err(|_| "Invalid API key format".to_string())?,
            );
            header_map.insert(
                "anthropic-version",
                "2023-06-01"
                    .parse()
                    .map_err(|_| "Invalid anthropic version".to_string())?,
            );
            (
                "https://api.anthropic.com/v1/messages".to_string(),
                header_map,
            )
        }
        LLMProvider::BuiltInAI => {
            // This case is handled earlier with early returns
            unreachable!("BuiltInAI is handled before this match statement")
        }
    };

    // Add authorization header for non-Claude providers
    if provider != &LLMProvider::Claude {
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {}", api_key)
                .parse()
                .map_err(|_| "Invalid authorization header".to_string())?,
        );
    }
    headers.insert(
        header::CONTENT_TYPE,
        "application/json"
            .parse()
            .map_err(|_| "Invalid content type".to_string())?,
    );

    // Build request body based on provider
    let request_body = match provider {
        // Current Claude models reject `temperature`, so Claude gets none.
        LLMProvider::Claude => serde_json::json!(ClaudeRequest {
            system: system_prompt.to_string(),
            model: model_name.to_string(),
            max_tokens: CLAUDE_MAX_TOKENS,
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: user_prompt.to_string(),
            }]
        }),
        LLMProvider::Ollama => {
            build_ollama_chat_body(model_name, system_prompt, user_prompt, options)
        }
        _ => build_openai_compat_chat_body(
            provider,
            model_name,
            system_prompt,
            user_prompt,
            options,
        ),
    };

    info!(
        "🐞 LLM Request to {}: model={}",
        provider_name(provider),
        model_name
    );

    // Send request with timeout and cancellation support
    let request_future = client
        .post(api_url.clone())
        .headers(headers.clone())
        .json(&request_body)
        .timeout(REQUEST_TIMEOUT_DURATION)
        .send();
    let response = await_or_cancel(request_future, cancellation_token)
        .await?
        .map_err(|e| send_error(e, "request"))?;

    let response = if response.status().is_success() {
        response
    } else {
        let status = response.status();
        let error_body = await_or_cancel(response.text(), cancellation_token)
            .await?
            .unwrap_or_else(|error| format!("Failed to read LLM error response body: {error}"));
        let Some(retry_body) =
            compatibility_retry_body(provider, &request_body, status, &error_body)
        else {
            return Err(format!(
                "LLM API request failed with status {}: {}",
                status, error_body
            ));
        };
        let retry_future = client
            .post(api_url)
            .headers(headers)
            .json(&retry_body)
            .timeout(REQUEST_TIMEOUT_DURATION)
            .send();
        await_or_cancel(retry_future, cancellation_token)
            .await?
            .map_err(|e| send_error(e, "retry request"))?
    };

    if !response.status().is_success() {
        let status = response.status();
        let error_body = await_or_cancel(response.text(), cancellation_token)
            .await?
            .unwrap_or_else(|error| format!("Failed to read LLM error response body: {error}"));
        return Err(format!(
            "LLM API request failed with status {}: {}",
            status, error_body
        ));
    }

    // Parse response based on provider
    let completion = match provider {
        LLMProvider::Claude => {
            let chat_response =
                await_or_cancel(response.json::<ClaudeChatResponse>(), cancellation_token)
                    .await?
                    .map_err(|e| format!("Failed to parse LLM response: {}", e))?;
            chat_response
                .completion()
                .ok_or("No text content in LLM response")?
        }
        LLMProvider::Ollama => {
            await_or_cancel(response.json::<OllamaChatResponse>(), cancellation_token)
                .await?
                .map_err(|e| format!("Failed to parse LLM response: {}", e))?
                .completion()
        }
        _ => await_or_cancel(response.json::<ChatResponse>(), cancellation_token)
            .await?
            .map_err(|e| format!("Failed to parse LLM response: {}", e))?
            .completion()?,
    };
    info!(
        "🐞 LLM Response received from {} (truncated: {})",
        provider_name(provider),
        completion.truncated
    );
    Ok(completion)
}

/// Helper function to get provider name for logging
fn provider_name(provider: &LLMProvider) -> &str {
    match provider {
        LLMProvider::OpenAI => "OpenAI",
        LLMProvider::Claude => "Claude",
        LLMProvider::Groq => "Groq",
        LLMProvider::Ollama => "Ollama",
        LLMProvider::BuiltInAI => "Built-in AI",
        LLMProvider::OpenRouter => "OpenRouter",
        LLMProvider::CustomOpenAI => "Custom OpenAI",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{cell::Cell, task::Poll};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::oneshot,
        time::{timeout, Duration},
    };

    async fn read_http_request(stream: &mut tokio::net::TcpStream) -> Vec<u8> {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];

        loop {
            let bytes_read = stream.read(&mut buffer).await.unwrap();
            assert_ne!(bytes_read, 0, "connection closed before completing request");
            request.extend_from_slice(&buffer[..bytes_read]);

            let Some(headers_end) = request
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|position| position + 4)
            else {
                continue;
            };
            let content_length = std::str::from_utf8(&request[..headers_end])
                .unwrap()
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .expect("request should have Content-Length");
            if request.len() >= headers_end + content_length {
                return request;
            }
        }
    }

    fn request_json(request: &[u8]) -> serde_json::Value {
        let headers_end = request
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|position| position + 4)
            .expect("request should contain complete headers");
        serde_json::from_slice(&request[headers_end..]).expect("request body should be valid JSON")
    }

    fn f32_json(value: f32) -> Option<f64> {
        Some(value as f64)
    }

    #[test]
    fn hosted_providers_get_fixed_low_temperature_and_seed() {
        for provider in [LLMProvider::OpenAI, LLMProvider::Groq, LLMProvider::OpenRouter] {
            let body = build_openai_compat_chat_body(
                &provider,
                "model",
                "sys",
                "user",
                &GenerationOptions {
                    max_tokens: Some(99),
                    temperature: Some(1.5),
                    ..Default::default()
                },
            );
            assert_eq!(body["temperature"].as_f64(), f32_json(SUMMARY_TEMPERATURE));
            assert_eq!(body["seed"], SUMMARY_SEED);
            assert!(body.get("max_tokens").is_none());
            assert!(body.get("think").is_none());
            assert_eq!(body["messages"][0]["role"], "system");
            assert_eq!(body["messages"][1]["content"], "user");
        }
    }

    #[test]
    fn custom_openai_keeps_user_sampling_and_defaults_temperature() {
        let custom = build_openai_compat_chat_body(
            &LLMProvider::CustomOpenAI,
            "model",
            "sys",
            "user",
            &GenerationOptions {
                max_tokens: Some(12),
                temperature: Some(0.3),
                top_p: Some(0.8),
                num_ctx: None,
            },
        );
        assert_eq!(custom["max_tokens"], 12);
        assert_eq!(custom["temperature"].as_f64(), f32_json(0.3));
        assert_eq!(custom["top_p"].as_f64(), f32_json(0.8));
        assert!(custom.get("seed").is_none());

        let defaulted = build_openai_compat_chat_body(
            &LLMProvider::CustomOpenAI,
            "model",
            "sys",
            "user",
            &GenerationOptions::default(),
        );
        assert_eq!(defaulted["temperature"].as_f64(), f32_json(SUMMARY_TEMPERATURE));
        assert!(defaulted.get("max_tokens").is_none());
        assert!(defaulted.get("top_p").is_none());
    }

    #[test]
    fn ollama_native_body_sets_context_output_and_sampling() {
        let body = build_ollama_chat_body(
            "qwen3:8b",
            "sys",
            "user",
            &GenerationOptions {
                max_tokens: Some(4096),
                num_ctx: Some(16384),
                ..Default::default()
            },
        );
        assert_eq!(body["stream"], false);
        assert_eq!(body["think"], false);
        assert_eq!(body["options"]["num_ctx"], 16384);
        assert_eq!(body["options"]["num_predict"], 4096);
        assert_eq!(body["options"]["seed"], SUMMARY_SEED);
        assert_eq!(
            body["options"]["temperature"].as_f64(),
            f32_json(SUMMARY_TEMPERATURE)
        );
        let bare = build_ollama_chat_body("m", "s", "u", &GenerationOptions::default());
        assert!(bare["options"].get("num_ctx").is_none());
        assert!(bare["options"].get("num_predict").is_none());
    }

    #[test]
    fn truncation_is_read_from_every_provider_shape() {
        let openai: ChatResponse = serde_json::from_value(json!({
            "choices": [{"message": {"content": "partial"}, "finish_reason": "length"}]
        }))
        .unwrap();
        assert!(openai.completion().unwrap().truncated);
        let openai_done: ChatResponse = serde_json::from_value(json!({
            "choices": [{"message": {"content": "done"}, "finish_reason": "stop"}]
        }))
        .unwrap();
        assert!(!openai_done.completion().unwrap().truncated);

        let claude: ClaudeChatResponse = serde_json::from_value(json!({
            "content": [{"type": "text", "text": "partial"}],
            "stop_reason": "max_tokens"
        }))
        .unwrap();
        assert!(claude.completion().unwrap().truncated);

        let ollama: OllamaChatResponse = serde_json::from_value(json!({
            "message": {"role": "assistant", "content": "partial", "thinking": "hmm"},
            "done_reason": "length",
            "prompt_eval_count": 321
        }))
        .unwrap();
        assert_eq!(
            ollama.completion(),
            LlmCompletion {
                content: "partial".to_string(),
                reasoning_stripped: true,
                truncated: true,
                prompt_tokens: Some(321),
            }
        );
    }

    #[test]
    fn rejected_sampling_fields_are_detected_for_retry() {
        assert_eq!(
            rejected_sampling_fields(
                reqwest::StatusCode::BAD_REQUEST,
                r#"{"error":{"message":"Unsupported value: 'temperature' does not support 0.2 with this model.","param":"temperature"}}"#,
            ),
            vec!["temperature"]
        );
        assert_eq!(
            rejected_sampling_fields(
                reqwest::StatusCode::BAD_REQUEST,
                r#"{"error":{"message":"Unknown parameter: 'seed'."}}"#,
            ),
            vec!["seed"]
        );
        assert!(rejected_sampling_fields(
            reqwest::StatusCode::UNAUTHORIZED,
            r#"{"error":{"param":"temperature"}}"#,
        )
        .is_empty());
        assert!(rejected_sampling_fields(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"error":{"message":"invalid model"}}"#,
        )
        .is_empty());
        let body = json!({"model": "m", "temperature": 0.2, "seed": 42});
        let retry = compatibility_retry_body(
            &LLMProvider::OpenAI,
            &body,
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"error":{"param":"temperature"}}"#,
        )
        .unwrap();
        assert!(retry.get("temperature").is_none());
        assert_eq!(retry["seed"], 42);
        assert!(compatibility_retry_body(
            &LLMProvider::Claude,
            &body,
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"error":{"param":"temperature"}}"#,
        )
        .is_none());
    }

    #[test]
    fn compatible_reasoning_is_separate_from_visible_content() {
        for message in [
            json!({"reasoning_content": "private"}),
            json!({"content": null, "reasoning_content": "private"}),
        ] {
            let response: ChatResponse = serde_json::from_value(json!({
                "choices": [{"message": message}]
            }))
            .unwrap();
            assert_eq!(
                response.completion().unwrap(),
                LlmCompletion {
                    content: String::new(),
                    reasoning_stripped: true,
                    truncated: false,
                    prompt_tokens: None,
                }
            );
        }
    }

    #[test]
    fn legacy_ollama_rejection_requires_compatible_error() {
        assert!(ollama_rejects_think(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"error":"\"llama3\" does not support thinking"}"#,
        ));
        assert!(ollama_rejects_think(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"error":{"field":"think"}}"#,
        ));
        assert!(ollama_rejects_think(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"error":{"param":"reasoning_effort"}}"#,
        ));
        assert!(ollama_rejects_think(
            reqwest::StatusCode::UNPROCESSABLE_ENTITY,
            r#"{"message":"Unsupported parameter 'ReAsOnInG_EfFoRt'."}"#,
        ));
        assert!(ollama_rejects_think(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"error":"think value \"none\" is not supported for this model"}"#,
        ));
        assert!(!ollama_rejects_think(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"message":"invalid model"}"#,
        ));
        assert!(!ollama_rejects_think(
            reqwest::StatusCode::UNAUTHORIZED,
            r#"{"error":{"param":"reasoning_effort"}}"#,
        ));
        assert!(!ollama_rejects_think(
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            r#"{"error":"think value \"none\" is not supported"}"#,
        ));
    }

    #[tokio::test]
    async fn cancellation_prevents_compatibility_io_from_being_polled() {
        let cancellation_token = CancellationToken::new();
        cancellation_token.cancel();
        let polled = Cell::new(false);

        let result = await_or_cancel(
            std::future::poll_fn(|_| {
                polled.set(true);
                Poll::<()>::Pending
            }),
            Some(&cancellation_token),
        )
        .await;

        assert_eq!(result, Err("Summary generation was cancelled".to_string()));
        assert!(!polled.get());
    }

    #[tokio::test]
    async fn cancellation_during_failed_legacy_ollama_retry_body_returns_promptly() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (retry_headers_sent, mut retry_headers_ready) = oneshot::channel();
        let (release_retry_body, retry_body_released) = oneshot::channel();

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request = read_http_request(&mut stream).await;
            let body = request_json(&request);
            assert_eq!(body["think"], false);

            let rejection_body = br#"{"error":{"param":"reasoning_effort"}}"#;
            let rejection_headers = format!(
                "HTTP/1.1 400 Bad Request\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                rejection_body.len()
            );
            stream
                .write_all(rejection_headers.as_bytes())
                .await
                .unwrap();
            stream.write_all(rejection_body).await.unwrap();
            stream.flush().await.unwrap();
            drop(stream);

            let (mut stream, _) = listener.accept().await.unwrap();
            let request = read_http_request(&mut stream).await;
            let body = request_json(&request);
            assert!(body.get("think").is_none());

            stream
                .write_all(
                    b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 4\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
            stream.flush().await.unwrap();
            let _ = retry_headers_sent.send(());
            let _ = retry_body_released.await;
            let _ = stream.write_all(b"fail").await;
        });

        let client = Client::new();
        let cancellation_token = CancellationToken::new();
        let endpoint = format!("http://{address}");
        let options = GenerationOptions::default();
        let generation = generate_summary(
            &client,
            &LLMProvider::Ollama,
            "model",
            "",
            "system",
            "user",
            Some(&endpoint),
            None,
            &options,
            None,
            Some(&cancellation_token),
        );
        tokio::pin!(generation);

        tokio::select! {
            biased;
            result = &mut generation => panic!("generation ended before retry headers: {result:?}"),
            _ = &mut retry_headers_ready => {}
        }

        cancellation_token.cancel();
        let completion = timeout(Duration::from_secs(1), &mut generation).await;
        let _ = release_retry_body.send(());
        let released_completion = if completion.is_err() {
            Some(timeout(Duration::from_secs(1), &mut generation).await)
        } else {
            None
        };
        server.await.unwrap();

        let result = completion.unwrap_or_else(|_| {
            released_completion
                .expect("generation should be awaited after releasing the retry body")
                .expect("generation should finish after releasing the retry body")
        });
        assert_eq!(result, Err("Summary generation was cancelled".to_string()));
    }

    #[tokio::test]
    async fn cancellation_during_successful_legacy_ollama_retry_body_returns_promptly() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (retry_headers_sent, mut retry_headers_ready) = oneshot::channel();
        let (release_retry_body, retry_body_released) = oneshot::channel();

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request = read_http_request(&mut stream).await;
            let body = request_json(&request);
            assert_eq!(body["think"], false);

            let rejection_body = br#"{"error":{"param":"reasoning_effort"}}"#;
            let rejection_headers = format!(
                "HTTP/1.1 400 Bad Request\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                rejection_body.len()
            );
            stream
                .write_all(rejection_headers.as_bytes())
                .await
                .unwrap();
            stream.write_all(rejection_body).await.unwrap();
            stream.flush().await.unwrap();
            drop(stream);

            let (mut stream, _) = listener.accept().await.unwrap();
            let request = read_http_request(&mut stream).await;
            let body = request_json(&request);
            assert!(body.get("think").is_none());

            let completion_body = br#"{"message":{"role":"assistant","content":"Meeting summary."},"done":true,"done_reason":"stop","prompt_eval_count":12}"#;
            let completion_headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                completion_body.len()
            );
            stream
                .write_all(completion_headers.as_bytes())
                .await
                .unwrap();
            stream.flush().await.unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
            let _ = retry_headers_sent.send(());
            let _ = retry_body_released.await;
            let _ = stream.write_all(completion_body).await;
        });

        let client = Client::new();
        let cancellation_token = CancellationToken::new();
        let endpoint = format!("http://{address}");
        let options = GenerationOptions::default();
        let generation = generate_summary(
            &client,
            &LLMProvider::Ollama,
            "model",
            "",
            "system",
            "user",
            Some(&endpoint),
            None,
            &options,
            None,
            Some(&cancellation_token),
        );
        tokio::pin!(generation);

        tokio::select! {
            biased;
            result = &mut generation => panic!("generation ended before retry headers: {result:?}"),
            _ = &mut retry_headers_ready => {}
        }

        cancellation_token.cancel();
        let completion = timeout(Duration::from_secs(1), &mut generation).await;
        let _ = release_retry_body.send(());
        if completion.is_err() {
            let _ = timeout(Duration::from_secs(1), &mut generation).await;
        }
        server.await.unwrap();

        let result =
            completion.expect("generation should observe cancellation before retry body release");
        assert_eq!(result, Err("Summary generation was cancelled".to_string()));
    }

    #[tokio::test]
    async fn legacy_ollama_retry_without_reasoning_effort_returns_completion() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request = read_http_request(&mut stream).await;
            let body = request_json(&request);
            assert_eq!(body["think"], false);

            let rejection_body = br#"{"error":{"param":"reasoning_effort"}}"#;
            let rejection_headers = format!(
                "HTTP/1.1 400 Bad Request\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                rejection_body.len()
            );
            stream
                .write_all(rejection_headers.as_bytes())
                .await
                .unwrap();
            stream.write_all(rejection_body).await.unwrap();
            stream.flush().await.unwrap();
            drop(stream);

            let (mut stream, _) = listener.accept().await.unwrap();
            let request = read_http_request(&mut stream).await;
            let body = request_json(&request);
            assert!(body.get("think").is_none());

            let completion_body = br#"{"message":{"role":"assistant","content":"Meeting summary."},"done":true,"done_reason":"stop","prompt_eval_count":12}"#;
            let completion_headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                completion_body.len()
            );
            stream
                .write_all(completion_headers.as_bytes())
                .await
                .unwrap();
            stream.write_all(completion_body).await.unwrap();
            stream.flush().await.unwrap();
        });

        let client = Client::new();
        let cancellation_token = CancellationToken::new();
        let endpoint = format!("http://{address}");
        let completion = timeout(
            Duration::from_secs(1),
            generate_summary(
                &client,
                &LLMProvider::Ollama,
                "model",
                "",
                "system",
                "user",
                Some(&endpoint),
                None,
                &GenerationOptions::default(),
                None,
                Some(&cancellation_token),
            ),
        )
        .await;
        server.await.unwrap();

        assert_eq!(
            completion
                .expect("generation should finish after the compatibility retry")
                .unwrap(),
            LlmCompletion {
                content: "Meeting summary.".to_string(),
                reasoning_stripped: false,
                truncated: false,
                prompt_tokens: Some(12),
            }
        );
    }

    #[test]
    fn claude_response_uses_exact_block_types() {
        let response: ClaudeChatResponse = serde_json::from_value(json!({
            "content": [
                {"type": "analysis", "text": "not visible"},
                {"type": "thinking", "thinking": "private"},
                {"type": "text", "text": "Meeting summary."}
            ]
        }))
        .unwrap();
        assert_eq!(
            response.completion(),
            Some(LlmCompletion {
                content: "Meeting summary.".to_string(),
                reasoning_stripped: true,
                truncated: false,
                prompt_tokens: None,
            })
        );
    }
}
