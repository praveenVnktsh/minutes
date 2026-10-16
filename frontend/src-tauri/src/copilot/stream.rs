//! Streaming model calls for Copilot, one per provider family. The request
//! bodies come from the summary client's builders, so provider quirks stay in
//! one place; only `stream: true` and the reading of the stream are new.

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::header;
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use super::context::PromptParts;
use super::stream_parse::{
    claude_event, ollama_line, openai_event, LineBuffer, SseDecoder, StreamItem,
};
use crate::summary::llm_client::{
    build_claude_body, build_ollama_chat_body, build_openai_compat_chat_body,
    compatibility_retry_body, GenerationOptions, LLMProvider,
};
use crate::summary::service::LlmConfig;
use crate::summary::summary_engine::client::{generate_with_builtin_stream, BuiltinStreamOptions};
use crate::summary::summary_engine::sidecar::SidecarError;

/// The helper's cached Copilot context. Prompts are capped well below it.
pub const BUILTIN_CACHE_CONTEXT: u32 = 8192;
/// Ollama context when the model's own size is unknown. Normally the
/// streamer carries the app-wide per-model num_ctx, so Ollama never reloads.
const OLLAMA_FALLBACK_NUM_CTX: u32 = 8192;
/// Compatibility retries per request (each drops what the last rejection named).
const MAX_COMPATIBILITY_RETRIES: usize = 2;
/// Upper bound for one request; first-token and stall timeouts are enforced
/// by the caller and are much shorter.
pub const HARD_TIMEOUT: Duration = Duration::from_secs(90);
const CLAUDE_VERSION: &str = "2023-06-01";

/// A finished stream.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StreamOutcome {
    pub text: String,
    pub truncated: bool,
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
    /// Prompt tokens the built-in model restored instead of evaluating.
    pub reused_tokens: Option<u32>,
    /// Generation speed as the built-in model measured it.
    pub tokens_per_second: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamErrorKind {
    /// Bad or missing API key, or no access to the model.
    Auth,
    /// 429 or a rate-limit message.
    RateLimited,
    /// The built-in model stayed busy with other work.
    Busy,
    Cancelled,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct StreamError {
    pub kind: StreamErrorKind,
    pub message: String,
}

impl StreamError {
    pub fn new(kind: StreamErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    fn other(message: impl Into<String>) -> Self {
        Self::new(StreamErrorKind::Other, message)
    }

    fn cancelled() -> Self {
        Self::new(StreamErrorKind::Cancelled, "Stopped")
    }
}

/// Something that streams an answer. The Copilot core only sees this.
#[async_trait]
pub trait Streamer: Send + Sync {
    fn provider(&self) -> &str;
    fn model(&self) -> &str;
    /// A hosted provider, where "use the built-in model instead" is an option.
    fn is_cloud(&self) -> bool;
    async fn stream(
        &self,
        parts: &PromptParts,
        max_tokens: u32,
        temperature: f32,
        deltas: UnboundedSender<String>,
        cancel: &CancellationToken,
    ) -> Result<StreamOutcome, StreamError>;
}

/// The configured provider and model.
pub struct ProviderStreamer {
    pub provider_id: String,
    pub(crate) config: LlmConfig,
    pub app_data_dir: Option<PathBuf>,
    pub client: reqwest::Client,
    /// Ollama's num_ctx: the same value every other app call uses for the model.
    pub num_ctx: Option<u32>,
}

/// Classifies an HTTP failure for the user and for back-off.
pub fn http_error(status: u16, body: &str) -> StreamError {
    let short: String = body.chars().take(240).collect();
    match status {
        401 | 403 => StreamError::new(
            StreamErrorKind::Auth,
            "The model provider refused the request. Check the API key in Settings.",
        ),
        429 => StreamError::new(
            StreamErrorKind::RateLimited,
            "The model provider is rate limiting requests. Copilot will not retry on its own.",
        ),
        _ if body.to_ascii_lowercase().contains("rate limit") => StreamError::new(
            StreamErrorKind::RateLimited,
            "The model provider is rate limiting requests. Copilot will not retry on its own.",
        ),
        _ => StreamError::other(format!("The model provider answered {status}: {short}")),
    }
}

fn in_stream_error(message: String) -> StreamError {
    let lower = message.to_ascii_lowercase();
    if lower.contains("rate limit") || lower.contains("overloaded") {
        StreamError::new(StreamErrorKind::RateLimited, message)
    } else {
        StreamError::other(message)
    }
}

#[derive(Clone, Copy)]
enum Format {
    OpenAi,
    Claude,
    Ollama,
}

impl ProviderStreamer {
    fn request(
        &self,
        parts: &PromptParts,
        max_tokens: u32,
        temperature: f32,
    ) -> Result<(String, header::HeaderMap, Value, Format), StreamError> {
        let config = &self.config;
        let user = parts.user();
        let options = GenerationOptions {
            max_tokens: Some(max_tokens),
            output_cap: Some(max_tokens),
            sampling_temperature: Some(temperature),
            num_ctx: Some(self.num_ctx.unwrap_or(OLLAMA_FALLBACK_NUM_CTX)),
            // Short answers: reasoning models get low effort and room to think.
            brief: true,
            ..Default::default()
        };
        let mut headers = header::HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
        let bearer = |headers: &mut header::HeaderMap| -> Result<(), StreamError> {
            if !config.api_key.is_empty() {
                headers.insert(
                    header::AUTHORIZATION,
                    format!("Bearer {}", config.api_key)
                        .parse()
                        .map_err(|_| StreamError::other("Invalid API key format"))?,
                );
            }
            Ok(())
        };
        let openai = |url: String, headers: header::HeaderMap| {
            let mut body = build_openai_compat_chat_body(
                &config.provider,
                &config.model_name,
                &parts.system,
                &user,
                &options,
            );
            body["stream"] = Value::Bool(true);
            (url, headers, body, Format::OpenAi)
        };
        Ok(match config.provider {
            LLMProvider::OpenAI => {
                bearer(&mut headers)?;
                let (url, headers, mut body, format) =
                    openai("https://api.openai.com/v1/chat/completions".into(), headers);
                body["stream_options"] = serde_json::json!({"include_usage": true});
                (url, headers, body, format)
            }
            LLMProvider::Groq => {
                bearer(&mut headers)?;
                openai(
                    "https://api.groq.com/openai/v1/chat/completions".into(),
                    headers,
                )
            }
            LLMProvider::OpenRouter => {
                bearer(&mut headers)?;
                openai(
                    "https://openrouter.ai/api/v1/chat/completions".into(),
                    headers,
                )
            }
            LLMProvider::CustomOpenAI => {
                bearer(&mut headers)?;
                let endpoint = config
                    .custom_openai_endpoint
                    .as_deref()
                    .ok_or_else(|| StreamError::other("Custom OpenAI endpoint not configured"))?;
                openai(
                    format!("{}/chat/completions", endpoint.trim_end_matches('/')),
                    headers,
                )
            }
            LLMProvider::Ollama => {
                let host = config
                    .ollama_endpoint
                    .as_deref()
                    .map(|s| s.trim_end_matches('/').to_string())
                    .unwrap_or_else(|| "http://localhost:11434".to_string());
                let mut body =
                    build_ollama_chat_body(&config.model_name, &parts.system, &user, &options);
                body["stream"] = Value::Bool(true);
                (format!("{host}/api/chat"), headers, body, Format::Ollama)
            }
            LLMProvider::Claude => {
                headers.insert(
                    "x-api-key",
                    config
                        .api_key
                        .parse()
                        .map_err(|_| StreamError::other("Invalid API key format"))?,
                );
                headers.insert("anthropic-version", CLAUDE_VERSION.parse().unwrap());
                let mut body =
                    build_claude_body(&config.model_name, &parts.system, &user, &options);
                body["stream"] = Value::Bool(true);
                (
                    "https://api.anthropic.com/v1/messages".into(),
                    headers,
                    body,
                    Format::Claude,
                )
            }
            LLMProvider::BuiltInAI => {
                unreachable!("the built-in model streams through the sidecar")
            }
        })
    }

    async fn send(
        &self,
        url: &str,
        headers: &header::HeaderMap,
        body: &Value,
        cancel: &CancellationToken,
    ) -> Result<reqwest::Response, StreamError> {
        let request = self
            .client
            .post(url)
            .headers(headers.clone())
            .json(body)
            .timeout(HARD_TIMEOUT)
            .send();
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(StreamError::cancelled()),
            response = request => response.map_err(|e| {
                if e.is_connect() {
                    StreamError::other(format!("Could not reach the model provider: {e}"))
                } else {
                    StreamError::other(format!("The request failed: {e}"))
                }
            }),
        }
    }

    async fn stream_http(
        &self,
        parts: &PromptParts,
        max_tokens: u32,
        temperature: f32,
        deltas: UnboundedSender<String>,
        cancel: &CancellationToken,
    ) -> Result<StreamOutcome, StreamError> {
        let (url, headers, body, format) = self.request(parts, max_tokens, temperature)?;
        let mut response = self.send(&url, &headers, &body, cancel).await?;
        let mut body = body;
        let mut retries = 0;
        while !response.status().is_success() {
            let status = response.status();
            let error_body = response.text().await.unwrap_or_default();
            let retry = (retries < MAX_COMPATIBILITY_RETRIES)
                .then(|| {
                    compatibility_retry_body(
                        &self.config.provider,
                        &body,
                        status,
                        &error_body,
                        true,
                    )
                })
                .flatten();
            let Some(retry) = retry else {
                return Err(http_error(status.as_u16(), &error_body));
            };
            retries += 1;
            response = self.send(&url, &headers, &retry, cancel).await?;
            body = retry;
        }

        let mut outcome = StreamOutcome::default();
        let mut sse = SseDecoder::default();
        let mut lines = LineBuffer::default();
        let mut bytes = response.bytes_stream();
        let mut done = false;
        let apply =
            |items: Vec<StreamItem>, outcome: &mut StreamOutcome| -> Result<bool, StreamError> {
                for item in items {
                    match item {
                        StreamItem::Delta(text) => {
                            outcome.text.push_str(&text);
                            let _ = deltas.send(text);
                        }
                        StreamItem::Finish { truncated } => outcome.truncated = truncated,
                        StreamItem::PromptTokens(n) => outcome.prompt_tokens = Some(n),
                        StreamItem::CompletionTokens(n) => outcome.completion_tokens = Some(n),
                        StreamItem::Error(message) => return Err(in_stream_error(message)),
                        StreamItem::Done => return Ok(true),
                    }
                }
                Ok(false)
            };
        while !done {
            let chunk = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(StreamError::cancelled()),
                chunk = bytes.next() => chunk,
            };
            let Some(chunk) = chunk else { break };
            let chunk =
                chunk.map_err(|e| StreamError::other(format!("The stream broke off: {e}")))?;
            let items: Vec<StreamItem> = match format {
                Format::OpenAi => sse.push(&chunk).iter().flat_map(openai_event).collect(),
                Format::Claude => sse.push(&chunk).iter().flat_map(claude_event).collect(),
                Format::Ollama => lines
                    .push(&chunk)
                    .iter()
                    .flat_map(|line| ollama_line(line))
                    .collect(),
            };
            done = apply(items, &mut outcome)?;
        }
        if !done {
            let items: Vec<StreamItem> = match format {
                Format::OpenAi => sse.finish().iter().flat_map(openai_event).collect(),
                Format::Claude => sse.finish().iter().flat_map(claude_event).collect(),
                Format::Ollama => lines.finish().map(|l| ollama_line(&l)).unwrap_or_default(),
            };
            apply(items, &mut outcome)?;
        }
        Ok(outcome)
    }

    async fn stream_builtin(
        &self,
        parts: &PromptParts,
        max_tokens: u32,
        temperature: f32,
        deltas: UnboundedSender<String>,
        cancel: &CancellationToken,
    ) -> Result<StreamOutcome, StreamError> {
        let app_data_dir = self
            .app_data_dir
            .as_deref()
            .ok_or_else(|| StreamError::other("The app data folder is unavailable"))?;
        let result = generate_with_builtin_stream(
            app_data_dir,
            &self.config.model_name,
            &parts.system,
            &parts.stable,
            &parts.rest,
            BuiltinStreamOptions {
                max_tokens,
                temperature,
                timeout: HARD_TIMEOUT,
                cache_context_size: BUILTIN_CACHE_CONTEXT,
            },
            deltas,
            Some(cancel),
        )
        .await;
        match result {
            Ok(done) => Ok(StreamOutcome {
                text: done.text,
                truncated: done.truncated,
                prompt_tokens: done.prompt_tokens,
                completion_tokens: done.completion_tokens,
                reused_tokens: done.reused_tokens,
                tokens_per_second: done.tokens_per_second,
            }),
            Err(error) => Err(match error.downcast_ref::<SidecarError>() {
                Some(SidecarError::Cancelled) => StreamError::cancelled(),
                Some(SidecarError::Busy(_)) => StreamError::new(
                    StreamErrorKind::Busy,
                    "The built-in model is busy with another task (a summary or title).",
                ),
                Some(SidecarError::TimedOut(_)) => {
                    StreamError::other("The built-in model took too long")
                }
                _ => StreamError::other(format!("The built-in model failed: {error:#}")),
            }),
        }
    }
}

#[async_trait]
impl Streamer for ProviderStreamer {
    fn provider(&self) -> &str {
        &self.provider_id
    }

    fn model(&self) -> &str {
        &self.config.model_name
    }

    fn is_cloud(&self) -> bool {
        !matches!(
            self.config.provider,
            LLMProvider::BuiltInAI | LLMProvider::Ollama
        )
    }

    async fn stream(
        &self,
        parts: &PromptParts,
        max_tokens: u32,
        temperature: f32,
        deltas: UnboundedSender<String>,
        cancel: &CancellationToken,
    ) -> Result<StreamOutcome, StreamError> {
        if cancel.is_cancelled() {
            return Err(StreamError::cancelled());
        }
        if self.config.provider == LLMProvider::BuiltInAI {
            self.stream_builtin(parts, max_tokens, temperature, deltas, cancel)
                .await
        } else {
            self.stream_http(parts, max_tokens, temperature, deltas, cancel)
                .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn parts() -> PromptParts {
        PromptParts {
            system: "sys".into(),
            stable: "stable\n".into(),
            rest: "TASK:\nwhat next?".into(),
        }
    }

    fn config(provider: LLMProvider, endpoint: Option<String>) -> LlmConfig {
        LlmConfig {
            provider,
            model_name: "m".into(),
            api_key: "k".into(),
            ollama_endpoint: endpoint.clone(),
            custom_openai_endpoint: endpoint,
            max_tokens: None,
            temperature: None,
            top_p: None,
        }
    }

    fn streamer(provider: LLMProvider, endpoint: Option<String>) -> ProviderStreamer {
        ProviderStreamer {
            provider_id: "test".into(),
            config: config(provider, endpoint),
            app_data_dir: None,
            client: reqwest::Client::new(),
            num_ctx: Some(4096),
        }
    }

    /// Serves one HTTP response (in two writes, to exercise chunking) and
    /// hands back the request body it got.
    async fn serve_once(
        status: &'static str,
        content_type: &'static str,
        body: String,
    ) -> (String, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 4096];
            loop {
                let n = socket.read(&mut buffer).await.unwrap();
                request.extend_from_slice(&buffer[..n]);
                let text = String::from_utf8_lossy(&request).to_string();
                if let Some(end) = text.find("\r\n\r\n") {
                    let length: usize = text[..end]
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.eq_ignore_ascii_case("content-length")
                                .then(|| v.trim().parse().unwrap())
                        })
                        .unwrap_or(0);
                    if request.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            let head = format!(
                "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            socket.write_all(head.as_bytes()).await.unwrap();
            let (first, second) = body.as_bytes().split_at(body.len() / 2);
            socket.write_all(first).await.unwrap();
            socket.flush().await.unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
            socket.write_all(second).await.unwrap();
            let text = String::from_utf8_lossy(&request).to_string();
            text[text.find("\r\n\r\n").unwrap() + 4..].to_string()
        });
        (address, handle)
    }

    #[tokio::test]
    async fn custom_openai_streams_deltas_and_sends_stream_true() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hello \"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"there [01:02].\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let (address, server) = serve_once("200 OK", "text/event-stream", body.into()).await;
        let streamer = streamer(LLMProvider::CustomOpenAI, Some(address));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let outcome = streamer
            .stream(&parts(), 300, 0.3, tx, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(outcome.text, "Hello there [01:02].");
        let mut streamed = String::new();
        while let Ok(d) = rx.try_recv() {
            streamed.push_str(&d);
        }
        assert_eq!(streamed, outcome.text);
        let sent: Value = serde_json::from_str(&server.await.unwrap()).unwrap();
        assert_eq!(sent["stream"], true);
        assert_eq!(sent["max_tokens"], 300);
        assert!((sent["temperature"].as_f64().unwrap() - 0.3).abs() < 1e-6);
        assert_eq!(sent["messages"][1]["content"], "stable\nTASK:\nwhat next?");
    }

    #[tokio::test]
    async fn ollama_streams_ndjson() {
        let body = concat!(
            "{\"message\":{\"content\":\"Ask about \"},\"done\":false}\n",
            "{\"message\":{\"content\":\"the budget.\"},\"done\":false}\n",
            "{\"message\":{\"content\":\"\"},\"done\":true,\"done_reason\":\"stop\",\"prompt_eval_count\":42}\n",
        );
        let (address, server) = serve_once("200 OK", "application/x-ndjson", body.into()).await;
        let streamer = streamer(LLMProvider::Ollama, Some(address));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let outcome = streamer
            .stream(&parts(), 300, 0.3, tx, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(outcome.text, "Ask about the budget.");
        assert_eq!(outcome.prompt_tokens, Some(42));
        let sent: Value = serde_json::from_str(&server.await.unwrap()).unwrap();
        assert_eq!(sent["stream"], true);
        assert_eq!(sent["think"], false);
        assert_eq!(sent["options"]["num_predict"], 300);
    }

    #[tokio::test]
    async fn auth_and_rate_limit_failures_are_typed() {
        let (address, _server) = serve_once(
            "401 Unauthorized",
            "application/json",
            r#"{"error":"bad key"}"#.into(),
        )
        .await;
        let streamer = streamer(LLMProvider::CustomOpenAI, Some(address));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let error = streamer
            .stream(&parts(), 300, 0.3, tx, &CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(error.kind, StreamErrorKind::Auth);
        assert_eq!(http_error(429, "").kind, StreamErrorKind::RateLimited);
        assert_eq!(http_error(500, "boom").kind, StreamErrorKind::Other);
    }

    #[test]
    fn streaming_bodies_use_the_shared_builders_and_retry_rules() {
        let (_, headers, claude, _) = streamer(LLMProvider::Claude, None)
            .request(&parts(), 300, 0.3)
            .unwrap();
        assert_eq!(claude["stream"], true);
        assert!(claude.get("temperature").is_none());
        assert_eq!(
            claude["output_config"]["effort"], "low",
            "brief: low effort"
        );
        assert!(headers.get("anthropic-version").is_some());

        let (_, _, ollama, _) = streamer(LLMProvider::Ollama, None)
            .request(&parts(), 300, 0.3)
            .unwrap();
        assert_eq!(ollama["options"]["num_ctx"], 4096, "the app-wide num_ctx");

        let (_, _, gpt5, _) = streamer(LLMProvider::OpenAI, None)
            .request(&parts(), 300, 0.3)
            .unwrap();
        let mut gpt5 = gpt5;
        gpt5["model"] = "gpt-5-mini".into();
        let (_, _, plain, _) = streamer(LLMProvider::OpenRouter, None)
            .request(&parts(), 300, 0.3)
            .unwrap();
        let retry = compatibility_retry_body(
            &LLMProvider::OpenRouter,
            &plain,
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"error":{"message":"Unsupported parameter: 'max_tokens' is not supported with this model. Use 'max_completion_tokens' instead.","param":"max_tokens"}}"#,
            true,
        )
        .unwrap();
        assert!(retry.get("max_tokens").is_none());
        assert!(retry.get("max_completion_tokens").is_some());
        assert_eq!(retry["stream"], true, "the retry still streams");
    }

    #[test]
    fn only_hosted_providers_are_cloud() {
        assert!(streamer(LLMProvider::Claude, None).is_cloud());
        assert!(streamer(LLMProvider::Groq, None).is_cloud());
        assert!(!streamer(LLMProvider::Ollama, None).is_cloud());
        assert!(!streamer(LLMProvider::BuiltInAI, None).is_cloud());
    }
}
