// High-level client API for built-in AI summary generation
// Provides simple interface for generating text using the sidecar

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::sync::RwLock;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use super::models;
use super::sidecar::{OnAbandon, SidecarManager};

// ============================================================================
// Request/Response Types
// ============================================================================

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Request {
    Generate {
        prompt: String,
        max_tokens: Option<i32>,
        context_size: Option<u32>,
        model_path: Option<String>,
        // Sampling parameters
        temperature: Option<f32>,
        top_k: Option<i32>,
        top_p: Option<f32>,
        presence_penalty: Option<f32>,
        frequency_penalty: Option<f32>,
        repeat_penalty: Option<f32>,
        penalty_last_n: Option<i32>,
        stop_tokens: Option<Vec<String>>,
        seed: Option<u32>,
    },
    Tokenize {
        text: String,
        context_size: Option<u32>,
        model_path: Option<String>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
// The variant name preserves the established `{"type":"response"}` wire protocol.
#[allow(clippy::enum_variant_names)]
enum Response {
    Response {
        text: String,
        error: Option<String>,
        #[serde(default)]
        finish_reason: Option<String>,
        #[serde(default)]
        prompt_tokens: Option<u32>,
        #[serde(default)]
        error_kind: Option<String>,
    },
    TokenCount {
        count: u32,
    },
    Error {
        message: String,
    },
}

/// Fixed sampling seed for built-in generation, so the same prompt gives the same output.
pub const BUILTIN_SEED: u32 = 42;

/// Error text llama-helper uses when prompt + output budget exceeds the context.
pub const CONTEXT_OVERFLOW_MARKER: &str = "context overflow";

/// Typed built-in failures callers branch on. They travel inside
/// `anyhow::Error`; use `downcast_ref` rather than matching message text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BuiltinError {
    /// llama-helper refused the request because prompt + output budget does
    /// not fit the context (`error_kind: "context_overflow"`).
    #[error("Generation failed ({CONTEXT_OVERFLOW_MARKER}): {0}")]
    ContextOverflow(String),
    /// The helper does not understand the request (an older llama-helper
    /// without `tokenize`).
    #[error("Sidecar error: {0}")]
    UnsupportedRequest(String),
}

/// A built-in generation result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltinCompletion {
    pub text: String,
    /// True when generation stopped at `max_tokens` rather than at end-of-turn.
    pub truncated: bool,
    pub prompt_tokens: Option<u32>,
}

fn parse_generate_response(response_json: &str) -> Result<BuiltinCompletion> {
    let response: Response = serde_json::from_str(response_json)
        .with_context(|| format!("Failed to parse response: {}", response_json))?;
    match response {
        Response::Response {
            text,
            error,
            finish_reason,
            prompt_tokens,
            error_kind,
        } => {
            if let Some(err_msg) = error {
                if error_kind.as_deref() == Some("context_overflow") {
                    return Err(anyhow!(BuiltinError::ContextOverflow(err_msg)));
                }
                return Err(anyhow!("Generation failed: {}", err_msg));
            }
            Ok(BuiltinCompletion {
                text,
                truncated: finish_reason.as_deref() == Some("length"),
                prompt_tokens,
            })
        }
        Response::TokenCount { .. } => Err(anyhow!("Unexpected token count response")),
        Response::Error { message } => Err(anyhow!("Sidecar error: {}", message)),
    }
}

fn parse_token_count_response(response_json: &str) -> Result<usize> {
    let response: Response = serde_json::from_str(response_json)
        .with_context(|| format!("Failed to parse response: {}", response_json))?;
    match response {
        Response::TokenCount { count } => Ok(count as usize),
        // llama-helper answers a request type it cannot parse with
        // "Invalid request: unknown variant ...".
        Response::Error { message } if message.starts_with("Invalid request") => {
            Err(anyhow!(BuiltinError::UnsupportedRequest(message)))
        }
        Response::Error { message } => Err(anyhow!("Sidecar error: {}", message)),
        Response::Response { error, .. } => Err(anyhow!(
            "Unexpected generate response to tokenize request: {:?}",
            error
        )),
    }
}

// ============================================================================
// Global Sidecar Manager
// ============================================================================

lazy_static::lazy_static! {
    static ref SIDECAR_MANAGER: Arc<Mutex<Option<Arc<SidecarManager>>>> = Arc::new(Mutex::new(None));
}

// Model path cache to avoid repeated filesystem I/O and model lookups
static MODEL_PATH_CACHE: Lazy<RwLock<HashMap<String, PathBuf>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));

/// Initialize the global sidecar manager
pub async fn init_sidecar_manager(app_data_dir: PathBuf) -> Result<()> {
    let manager = SidecarManager::new(app_data_dir)?;
    let mut global_manager = SIDECAR_MANAGER.lock().await;
    *global_manager = Some(Arc::new(manager));
    Ok(())
}

/// Get the global sidecar manager
async fn get_sidecar_manager() -> Result<Arc<SidecarManager>> {
    let global_manager = SIDECAR_MANAGER.lock().await;
    global_manager
        .clone()
        .ok_or_else(|| anyhow!("Sidecar manager not initialized. Call init_sidecar_manager first."))
}

/// Get cached model path with read-through caching to avoid repeated filesystem I/O
fn get_cached_model_path(app_data_dir: &Path, model_name: &str) -> Result<PathBuf> {
    // Try read lock first (fast path for cache hits)
    {
        let cache = MODEL_PATH_CACHE.read().unwrap();
        if let Some(path) = cache.get(model_name) {
            // Verify file still exists before returning cached path
            if path.exists() {
                return Ok(path.clone());
            }
        }
    }

    // Cache miss or file deleted - acquire write lock and update cache
    let mut cache = MODEL_PATH_CACHE.write().unwrap();

    // Double-check after acquiring write lock (another thread may have updated it)
    if let Some(path) = cache.get(model_name) {
        if path.exists() {
            return Ok(path.clone());
        }
    }

    // Resolve model path (involves model lookup + filesystem operations)
    let model_path = models::get_model_path(app_data_dir, model_name)?;

    if !model_path.exists() {
        return Err(anyhow!(
            "Model file not found: {}. Please download the model '{}' first.",
            model_path.display(),
            model_name
        ));
    }

    // Cache the validated path
    cache.insert(model_name.to_string(), model_path.clone());
    Ok(model_path)
}

// ============================================================================
// Public API
// ============================================================================

async fn running_manager(
    app_data_dir: &Path,
    model_name: &str,
) -> Result<(Arc<SidecarManager>, models::ModelDef, PathBuf)> {
    let model_def = models::get_model_by_name(model_name)
        .ok_or_else(|| anyhow!("Unknown model: {}", model_name))?;
    // Resolve model path with caching (avoids repeated filesystem I/O)
    let model_path = get_cached_model_path(app_data_dir, model_name)?;
    let manager = {
        let mut global_manager = SIDECAR_MANAGER.lock().await;
        if global_manager.is_none() {
            log::info!("Initializing sidecar manager");
            let new_manager = SidecarManager::new(app_data_dir.to_path_buf())?;
            *global_manager = Some(Arc::new(new_manager));
        }
        global_manager.clone().unwrap()
    };
    manager.ensure_running(model_path.clone()).await?;
    Ok((manager, model_def, model_path))
}

async fn send_cancellable(
    manager: &SidecarManager,
    request_json: String,
    timeout: Duration,
    cancellation_token: Option<&CancellationToken>,
    on_abandon: OnAbandon,
) -> Result<String> {
    manager
        .send_request(request_json, timeout, cancellation_token, on_abandon)
        .await
}

/// Formats the prompt exactly as `generate_with_builtin` will, so token counts
/// of it match what llama-helper sees.
pub fn format_builtin_prompt(
    model_name: &str,
    system_prompt: &str,
    user_prompt: &str,
) -> Result<String> {
    let model_def = models::get_model_by_name(model_name)
        .ok_or_else(|| anyhow!("Unknown model: {}", model_name))?;
    models::format_prompt(&model_def.template, system_prompt, user_prompt)
}

/// Counts `text`'s tokens with the built-in model's own tokenizer.
pub async fn count_builtin_tokens(
    app_data_dir: &Path,
    model_name: &str,
    text: &str,
    cancellation_token: Option<&CancellationToken>,
) -> Result<usize> {
    let (manager, model_def, model_path) = running_manager(app_data_dir, model_name).await?;
    let request_json = serde_json::to_string(&Request::Tokenize {
        text: text.to_string(),
        context_size: Some(model_def.context_size),
        model_path: Some(model_path.to_string_lossy().to_string()),
    })?;
    let response_json = send_cancellable(
        &manager,
        request_json,
        Duration::from_secs(TOKENIZE_TIMEOUT_SECS),
        cancellation_token,
        // Counting is cheap and runs under the summary's own cancellation.
        OnAbandon::StopSidecar,
    )
    .await?;
    parse_token_count_response(&response_json)
}

const TOKENIZE_TIMEOUT_SECS: u64 = 120;

/// Per-call overrides for [`generate_with_builtin`]. `None` keeps the model's
/// own sampling defaults, the fixed seed and the standard timeout.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct BuiltinGenerateOptions {
    /// Output budget; `None` uses `models::DEFAULT_MAX_TOKENS`.
    pub max_tokens: Option<u32>,
    /// Sampling temperature; `None` uses the model's default.
    pub temperature: Option<f32>,
    /// Sampling seed; `None` uses [`BUILTIN_SEED`].
    pub seed: Option<u32>,
    /// Time the request may take once it reaches the sidecar (waiting for
    /// another caller's request does not count).
    pub timeout: Option<Duration>,
    /// What a timeout or cancellation does to the shared sidecar.
    pub on_abandon: OnAbandon,
}

/// Generate text using built-in AI
///
/// # Arguments
/// * `app_data_dir` - Application data directory (for model resolution)
/// * `model_name` - Model name (e.g., "gemma3:1b")
/// * `system_prompt` - System instructions for the model
/// * `user_prompt` - User message/task
/// * `options` - Output budget, sampling overrides, timeout and abandon policy
/// * `cancellation_token` - Optional token for cancellation
///
/// # Returns
/// The generated text, and whether it stopped at the output budget.
pub async fn generate_with_builtin(
    app_data_dir: &Path,
    model_name: &str,
    system_prompt: &str,
    user_prompt: &str,
    options: BuiltinGenerateOptions,
    cancellation_token: Option<&CancellationToken>,
) -> Result<BuiltinCompletion> {
    if cancellation_token.is_some_and(CancellationToken::is_cancelled) {
        return Err(anyhow!("Generation cancelled before starting"));
    }

    log::info!("Built-in AI generation request");
    log::info!("Model: {}", model_name);

    let formatted_prompt = format_builtin_prompt(model_name, system_prompt, user_prompt)?;
    let (manager, model_def, model_path) = running_manager(app_data_dir, model_name).await?;

    if cancellation_token.is_some_and(CancellationToken::is_cancelled) {
        return Err(anyhow!("Generation cancelled during sidecar startup"));
    }

    // Prepare generation request with model-specific sampling parameters
    let sampling = model_def.sampling.sanitize_for_llama_helper();
    let max_tokens = options
        .max_tokens
        .and_then(|value| i32::try_from(value).ok())
        .filter(|value| *value > 0)
        .unwrap_or(models::DEFAULT_MAX_TOKENS);
    let request = Request::Generate {
        prompt: formatted_prompt,
        max_tokens: Some(max_tokens),
        context_size: Some(model_def.context_size),
        model_path: Some(model_path.to_string_lossy().to_string()),
        temperature: Some(options.temperature.unwrap_or(sampling.temperature)),
        top_k: Some(sampling.top_k),
        top_p: Some(sampling.top_p),
        presence_penalty: Some(sampling.presence_penalty),
        frequency_penalty: Some(sampling.frequency_penalty),
        repeat_penalty: Some(sampling.repeat_penalty),
        penalty_last_n: Some(sampling.penalty_last_n),
        stop_tokens: Some(sampling.stop_tokens),
        seed: Some(options.seed.unwrap_or(BUILTIN_SEED)),
    };

    let request_json = serde_json::to_string(&request)?;
    let timeout = options
        .timeout
        .unwrap_or(Duration::from_secs(models::GENERATION_TIMEOUT_SECS));

    log::info!("Sending generation request to sidecar (max_tokens: {max_tokens})");
    let response_json = send_cancellable(
        &manager,
        request_json,
        timeout,
        cancellation_token,
        options.on_abandon,
    )
    .await?;

    if cancellation_token.is_some_and(CancellationToken::is_cancelled) {
        return Err(anyhow!("Generation cancelled"));
    }

    let completion = parse_generate_response(&response_json)?;
    log::info!(
        "Generation completed: {} chars, truncated: {}",
        completion.text.len(),
        completion.truncated
    );
    Ok(completion)
}

/// Shutdown the global sidecar (graceful cleanup)
/// Detaches the current manager and spawns a background task to drain active requests
pub async fn shutdown_sidecar_gracefully() -> Result<()> {
    let manager_opt = {
        let mut global_manager = SIDECAR_MANAGER.lock().await;
        global_manager.take()
    };

    if let Some(manager) = manager_opt {
        log::info!("Detaching sidecar manager for graceful shutdown");

        // Spawn background task to wait for active requests and then kill
        tokio::spawn(async move {
            if let Err(e) = manager.shutdown_gracefully().await {
                log::error!("Error during graceful shutdown: {}", e);
            }
        });
    }

    Ok(())
}

/// Force shutdown the global sidecar (for app exit)
/// Directly kills the process without waiting for active requests to complete.
/// This is synchronous and blocks until the sidecar is terminated.
pub async fn force_shutdown_sidecar() -> Result<()> {
    let manager_opt = {
        let mut global_manager = SIDECAR_MANAGER.lock().await;
        global_manager.take()
    };

    if let Some(manager) = manager_opt {
        log::info!("Force shutting down sidecar for app exit");
        // Call shutdown() directly - sends shutdown command and force kills after 3s
        manager.shutdown().await?;
    }

    Ok(())
}

/// Check if sidecar is healthy
pub async fn is_sidecar_healthy() -> bool {
    if let Ok(manager) = get_sidecar_manager().await {
        manager.is_healthy()
    } else {
        false
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_serialization() {
        let request = Request::Generate {
            prompt: "test prompt".to_string(),
            max_tokens: Some(512),
            context_size: Some(2048),
            model_path: Some("/path/to/model.gguf".to_string()),
            temperature: Some(1.0),
            top_k: Some(64),
            top_p: Some(0.95),
            presence_penalty: Some(0.3),
            frequency_penalty: Some(0.0),
            repeat_penalty: Some(1.05),
            penalty_last_n: Some(256),
            stop_tokens: Some(vec!["<end_of_turn>".to_string()]),
            seed: Some(BUILTIN_SEED),
        };

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"type\":\"generate\""));
        assert!(json.contains("\"prompt\":\"test prompt\""));
        assert!(json.contains("\"max_tokens\":512"));
        assert!(json.contains("\"temperature\":1.0"));
        assert!(json.contains("\"presence_penalty\":0.3"));
        assert!(json.contains("\"frequency_penalty\":0.0"));
        assert!(json.contains("\"repeat_penalty\":1.05"));
        assert!(json.contains("\"penalty_last_n\":256"));
        assert!(json.contains("\"seed\":42"));
    }

    #[test]
    fn generate_response_reports_truncation_from_finish_reason() {
        let done = parse_generate_response(
            r#"{"type":"response","text":"ok","error":null,"finish_reason":"stop","prompt_tokens":10}"#,
        )
        .unwrap();
        assert_eq!(
            done,
            BuiltinCompletion {
                text: "ok".to_string(),
                truncated: false,
                prompt_tokens: Some(10),
            }
        );
        let cut = parse_generate_response(
            r#"{"type":"response","text":"par","error":null,"finish_reason":"length"}"#,
        )
        .unwrap();
        assert!(cut.truncated);
        // Responses from an older helper carry no finish_reason.
        let legacy =
            parse_generate_response(r#"{"type":"response","text":"x","error":null}"#).unwrap();
        assert!(!legacy.truncated);
    }

    #[test]
    fn generate_response_overflow_error_keeps_marker() {
        let error = parse_generate_response(
            r#"{"type":"response","text":"","error":"Generation failed: context overflow: prompt has 9 tokens","error_kind":"context_overflow"}"#,
        )
        .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<BuiltinError>(),
            Some(BuiltinError::ContextOverflow(_))
        ));
        assert!(error.to_string().contains(CONTEXT_OVERFLOW_MARKER));
        // Only error_kind decides, never the message text.
        let plain = parse_generate_response(
            r#"{"type":"response","text":"","error":"exceeds the context window"}"#,
        )
        .unwrap_err();
        assert!(plain.downcast_ref::<BuiltinError>().is_none());
    }

    #[test]
    fn old_helper_rejecting_tokenize_is_typed_as_unsupported() {
        let error = parse_token_count_response(
            r#"{"type":"error","message":"Invalid request: unknown variant `tokenize`"}"#,
        )
        .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<BuiltinError>(),
            Some(BuiltinError::UnsupportedRequest(_))
        ));
    }

    #[test]
    fn tokenize_request_and_count_response() {
        let json = serde_json::to_string(&Request::Tokenize {
            text: "hi".to_string(),
            context_size: Some(4096),
            model_path: None,
        })
        .unwrap();
        assert!(json.contains("\"type\":\"tokenize\""));
        assert_eq!(
            parse_token_count_response(r#"{"type":"token_count","count":17}"#).unwrap(),
            17
        );
        assert!(parse_token_count_response(r#"{"type":"error","message":"no"}"#).is_err());
    }

    #[test]
    fn test_response_deserialization() {
        let json = r#"{"type":"response","text":"generated text","error":null}"#;
        let response: Response = serde_json::from_str(json).unwrap();

        match response {
            Response::Response { text, error, .. } => {
                assert_eq!(text, "generated text");
                assert!(error.is_none());
            }
            _ => panic!("Wrong response type"),
        }
    }

    #[test]
    fn test_error_response_deserialization() {
        let json = r#"{"type":"error","message":"something went wrong"}"#;
        let response: Response = serde_json::from_str(json).unwrap();

        match response {
            Response::Error { message } => {
                assert_eq!(message, "something went wrong");
            }
            _ => panic!("Wrong response type"),
        }
    }
}
