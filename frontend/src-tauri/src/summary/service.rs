use crate::database::models::{Setting, Transcript};
use crate::database::repositories::{
    meeting::MeetingsRepository, setting::SettingsRepository, summary::SummaryProcessesRepository,
};
use crate::ollama::metadata::ModelMetadataCache;
use crate::summary::language_detection::detect_summary_language;
use crate::summary::llm_client::{generate_summary, LLMProvider};
use crate::summary::metadata::read_detected_summary_language_from_metadata;
use crate::summary::processor::{
    build_title_user_prompt, clean_llm_markdown_detailed, default_notes_system_prompt,
    extract_meeting_name_from_markdown, generate_meeting_summary, is_prompt_echo_title,
    language_name_from_code, require_visible_markdown, sanitize_generated_title,
    title_system_prompt,
};
use crate::summary::templates::{self, Template};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

static METADATA_CACHE: LazyLock<ModelMetadataCache> =
    LazyLock::new(|| ModelMetadataCache::new(Duration::from_secs(300)));

#[derive(Clone)]
struct RegisteredCancellation {
    started_at: DateTime<Utc>,
    token: CancellationToken,
}

static CANCELLATION_REGISTRY: LazyLock<Arc<Mutex<HashMap<String, RegisteredCancellation>>>> =
    LazyLock::new(|| Arc::new(Mutex::new(HashMap::new())));

/// Strips the first `#` heading line; returns "" if no `#` is found.
fn strip_leading_title(markdown: &str) -> String {
    if let Some(hash_pos) = markdown.find('#') {
        let body_start = markdown[hash_pos..]
            .find('\n')
            .map_or(markdown.len(), |line_end| hash_pos + line_end);
        markdown[body_start..].trim_start().to_string()
    } else {
        String::new()
    }
}

/// Strips the leading H1 (`# Title\n...`) only when the markdown starts with one.
/// No-op on already-stripped values, values starting with `## Subheading`, or values
/// without any heading. Avoids the silent-empty-return case where `strip_leading_title`
/// returns "" for input lacking a leading `#`.
fn strip_title_if_present(markdown: &str) -> String {
    if markdown.trim_start().starts_with("# ") {
        strip_leading_title(markdown)
    } else {
        markdown.to_string()
    }
}

const ENGLISH_CACHE_FIELD: &str = "english_cache";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct SummaryCacheSource {
    transcript_fingerprint: String,
    custom_prompt_fingerprint: String,
    template_id: String,
    template_fingerprint: String,
    #[serde(default)]
    notes_prompt_fingerprint: String,
    token_threshold: usize,
    model_provider: String,
    model_name: String,
    ollama_endpoint: Option<String>,
    custom_openai_endpoint: Option<String>,
    max_tokens: Option<u32>,
    temperature: Option<f32>,
    top_p: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct EnglishSummaryCache {
    markdown: String,
    source: SummaryCacheSource,
    output_language: Option<String>,
}

fn stable_text_fingerprint(text: &str) -> String {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;

    let mut hash = FNV_OFFSET;
    for byte in text.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("{:016x}:{}", hash, text.len())
}

#[allow(clippy::too_many_arguments)]
fn build_summary_cache_source(
    text: &str,
    custom_prompt: &str,
    template_id: &str,
    template_fingerprint: &str,
    notes_prompt_fingerprint: &str,
    token_threshold: usize,
    model_provider: &str,
    model_name: &str,
    ollama_endpoint: Option<&str>,
    custom_openai_endpoint: Option<&str>,
    max_tokens: Option<u32>,
    temperature: Option<f32>,
    top_p: Option<f32>,
) -> SummaryCacheSource {
    SummaryCacheSource {
        transcript_fingerprint: stable_text_fingerprint(text),
        custom_prompt_fingerprint: stable_text_fingerprint(custom_prompt),
        template_id: template_id.to_string(),
        template_fingerprint: template_fingerprint.to_string(),
        notes_prompt_fingerprint: notes_prompt_fingerprint.to_string(),
        token_threshold,
        model_provider: model_provider.to_string(),
        model_name: model_name.to_string(),
        ollama_endpoint: ollama_endpoint.map(str::to_string),
        custom_openai_endpoint: custom_openai_endpoint.map(str::to_string),
        max_tokens,
        temperature,
        top_p,
    }
}

fn template_cache_fingerprint(template: &Template) -> String {
    let rendered_template = format!(
        "{}\n---SECTION-INSTRUCTIONS---\n{}",
        template.to_markdown_structure(),
        template.to_section_instructions()
    );
    stable_text_fingerprint(&rendered_template)
}

fn normalise_summary_language_for_cache(summary_language: Option<&str>) -> Option<String> {
    language_name_from_code(summary_language?.trim()).map(str::to_string)
}

fn build_summary_result_json(
    final_markdown: &str,
    english_markdown: &str,
    source: SummaryCacheSource,
    output_language: Option<&str>,
    reasoning_stripped: bool,
    normalization_fallback: bool,
) -> Result<serde_json::Value, String> {
    let cleaned_final = clean_llm_markdown_detailed(final_markdown);
    require_visible_markdown("Final summary", &cleaned_final)?;
    let markdown = strip_title_if_present(&cleaned_final.markdown);
    if markdown.trim().is_empty() {
        return Err("Final summary contains no visible content after title removal".to_string());
    }

    let cleaned_english = clean_llm_markdown_detailed(english_markdown);
    require_visible_markdown("English summary", &cleaned_english)?;

    Ok(serde_json::json!({
        "markdown": markdown,
        ENGLISH_CACHE_FIELD: EnglishSummaryCache {
            markdown: cleaned_english.markdown,
            source,
            output_language: normalise_summary_language_for_cache(output_language),
        },
        "reasoning_stripped": reasoning_stripped
            || cleaned_final.reasoning_stripped
            || cleaned_english.reasoning_stripped,
        "normalization_fallback": normalization_fallback,
    }))
}

/// Parses a `summary_processes.result` JSON blob and extracts a cached English
/// summary only when it was produced from exactly the same source inputs and
/// the user is switching to a different non-English target language.
fn extract_cached_english_markdown(
    raw: &str,
    expected_source: &SummaryCacheSource,
    requested_language: Option<&str>,
) -> Result<Option<String>, serde_json::Error> {
    let requested_language = match normalise_summary_language_for_cache(requested_language) {
        Some(language) if language != "English" => language,
        _ => return Ok(None),
    };

    let value: serde_json::Value = serde_json::from_str(raw)?;
    let Some(cache_value) = value.get(ENGLISH_CACHE_FIELD) else {
        return Ok(None);
    };

    let cache: EnglishSummaryCache = match serde_json::from_value(cache_value.clone()) {
        Ok(cache) => cache,
        Err(_) => return Ok(None),
    };

    if cache.source != *expected_source {
        return Ok(None);
    }

    if cache.output_language.as_deref() == Some(requested_language.as_str()) {
        return Ok(None);
    }

    let markdown = cache.markdown.trim();
    if markdown.is_empty() {
        Ok(None)
    } else {
        Ok(Some(cache.markdown))
    }
}

/// Everything an LLM call needs beyond the prompts, resolved from saved settings.
struct LlmConfig {
    provider: LLMProvider,
    model_name: String,
    api_key: String,
    ollama_endpoint: Option<String>,
    custom_openai_endpoint: Option<String>,
    max_tokens: Option<u32>,
    temperature: Option<f32>,
    top_p: Option<f32>,
}

/// Resolves the API key, Ollama endpoint and CustomOpenAI settings for a provider/model pair.
/// `settings` is the already-loaded settings row, when the caller has one, so Ollama's
/// endpoint lookup does not read it again.
async fn load_llm_config(
    pool: &SqlitePool,
    model_provider: &str,
    model_name: &str,
    settings: Option<&Setting>,
) -> Result<LlmConfig, String> {
    let provider = LLMProvider::from_str(model_provider)?;
    let mut config = LlmConfig {
        provider: provider.clone(),
        model_name: model_name.to_string(),
        api_key: String::new(),
        ollama_endpoint: None,
        custom_openai_endpoint: None,
        max_tokens: None,
        temperature: None,
        top_p: None,
    };

    match provider {
        LLMProvider::BuiltInAI => {}
        LLMProvider::Ollama => {
            config.ollama_endpoint = match settings {
                Some(settings) => settings.ollama_endpoint.clone(),
                None => match SettingsRepository::get_model_config(pool).await {
                    Ok(settings) => settings.and_then(|settings| settings.ollama_endpoint),
                    Err(e) => {
                        info!("Failed to retrieve Ollama endpoint: {}, using default", e);
                        None
                    }
                },
            };
        }
        LLMProvider::CustomOpenAI => {
            let custom = match SettingsRepository::get_custom_openai_config(pool).await {
                Ok(Some(custom)) => custom,
                Ok(None) => {
                    return Err(
                        "Custom OpenAI provider selected but no configuration found".to_string()
                    );
                }
                Err(e) => {
                    return Err(format!("Failed to retrieve custom OpenAI config: {}", e));
                }
            };
            info!("✓ Using custom OpenAI endpoint: {}", custom.endpoint);
            config.api_key = custom.api_key.unwrap_or_default();
            config.custom_openai_endpoint = Some(custom.endpoint);
            config.max_tokens = custom.max_tokens.map(|t| t as u32);
            config.temperature = custom.temperature;
            config.top_p = custom.top_p;
        }
        _ => {
            config.api_key = match SettingsRepository::get_api_key(pool, model_provider).await {
                Ok(Some(key)) if !key.is_empty() => key,
                Ok(None) | Ok(Some(_)) => {
                    return Err(format!("API key not found for {}", model_provider));
                }
                Err(e) => {
                    return Err(format!(
                        "Failed to retrieve API key for {}: {}",
                        model_provider, e
                    ));
                }
            };
        }
    }

    Ok(config)
}

/// Joins stored transcript segments into the plain text the title prompt reads,
/// labelling each line with its speaker the way the summary transcript does.
fn title_transcript_text(segments: &[Transcript]) -> String {
    segments
        .iter()
        .filter_map(|segment| {
            let text = segment.transcript.trim();
            if text.is_empty() {
                return None;
            }
            Some(match segment.speaker.as_deref().map(str::trim) {
                Some("mic") => format!("[You] {text}"),
                Some("system") => format!("[Others] {text}"),
                Some(speaker) if !speaker.is_empty() => format!("[{speaker}] {text}"),
                _ => text.to_string(),
            })
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Summary service - handles all summary generation logic
pub struct SummaryService;

impl SummaryService {
    /// Registers a new cancellation token for a meeting.
    pub(crate) fn register_cancellation_token(
        meeting_id: &str,
        started_at: DateTime<Utc>,
    ) -> CancellationToken {
        let token = CancellationToken::new();
        let mut registry = CANCELLATION_REGISTRY
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(previous) = registry.insert(
            meeting_id.to_string(),
            RegisteredCancellation {
                started_at,
                token: token.clone(),
            },
        ) {
            previous.token.cancel();
        }
        info!("Registered cancellation token for meeting: {}", meeting_id);
        token
    }

    /// Cancels only the active generation identified by `started_at`.
    pub fn cancel_summary(meeting_id: &str, started_at: DateTime<Utc>) -> bool {
        let registry = CANCELLATION_REGISTRY
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(entry) = registry
            .get(meeting_id)
            .filter(|entry| entry.started_at == started_at)
        {
            info!("Cancelling summary generation for meeting: {}", meeting_id);
            entry.token.cancel();
            true
        } else {
            warn!(
                "No active summary generation found for meeting: {}",
                meeting_id
            );
            false
        }
    }

    /// Cleans up only the matching generation token after processing completes.
    fn cleanup_cancellation_token(meeting_id: &str, started_at: DateTime<Utc>) {
        let mut registry = CANCELLATION_REGISTRY
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if registry
            .get(meeting_id)
            .is_some_and(|entry| entry.started_at == started_at)
        {
            registry.remove(meeting_id);
            info!("Cleaned up cancellation token for meeting: {}", meeting_id);
        }
    }

    async fn read_detected_summary_language(pool: &SqlitePool, meeting_id: &str) -> Option<String> {
        let meeting = match MeetingsRepository::get_meeting_metadata(pool, meeting_id).await {
            Ok(Some(meeting)) => meeting,
            Ok(None) => {
                warn!(
                    "Meeting not found while reading detected summary language: {}",
                    meeting_id
                );
                return None;
            }
            Err(e) => {
                warn!(
                    "Failed to read meeting metadata for detected summary language (meeting_id={}): {}",
                    meeting_id, e
                );
                return None;
            }
        };

        let folder_path = meeting.folder_path.filter(|p| !p.trim().is_empty())?;

        match read_detected_summary_language_from_metadata(Path::new(&folder_path)) {
            Ok(language) => language,
            Err(e) => {
                warn!(
                    "Failed to read detected summary language metadata for meeting_id={}: {}",
                    meeting_id, e
                );
                None
            }
        }
    }

    fn detect_summary_language_from_text(text: &str) -> Option<String> {
        let transcript_texts = [text.to_string()];
        let detection = detect_summary_language(&transcript_texts);
        match &detection.language {
            Some(language) => {
                info!(
                    "Detected transcript summary language for normalization: {}",
                    language
                );
            }
            None => {
                info!(
                    "Transcript summary language unknown for normalization: {:?}",
                    detection.reason
                );
            }
        }
        detection.language
    }

    /// Processes transcript in the background and generates summary
    ///
    /// This function is designed to be spawned as an async task and does not block
    /// the main thread. It updates the database with progress and results.
    ///
    /// # Arguments
    /// * `app` - Tauri app handle (used for app data dir and the saved notes-prompt override)
    /// * `pool` - SQLx connection pool
    /// * `meeting_id` - Unique identifier for the meeting
    /// * `text` - Full transcript text
    /// * `model_provider` - LLM provider name (e.g., "ollama", "openai")
    /// * `model_name` - Specific model (e.g., "gpt-4", "llama3.2:latest")
    /// * `custom_prompt` - Optional user-provided context
    /// * `template_id` - Template identifier (e.g., "daily_standup", "standard_meeting")
    // This task boundary carries the persisted command inputs into detached processing.
    #[allow(clippy::too_many_arguments)]
    pub async fn process_transcript_background<R: tauri::Runtime>(
        app: AppHandle<R>,
        pool: SqlitePool,
        meeting_id: String,
        started_at: DateTime<Utc>,
        cancellation_token: CancellationToken,
        text: String,
        model_provider: String,
        model_name: String,
        custom_prompt: String,
        template_id: String,
        summary_language: Option<String>,
    ) {
        let start_time = Instant::now();
        info!(
            "Starting background processing for meeting_id: {}",
            meeting_id
        );

        let LlmConfig {
            provider,
            model_name,
            api_key: final_api_key,
            ollama_endpoint,
            custom_openai_endpoint,
            max_tokens: custom_openai_max_tokens,
            temperature: custom_openai_temperature,
            top_p: custom_openai_top_p,
        } = match load_llm_config(&pool, &model_provider, &model_name, None).await {
            Ok(config) => config,
            Err(e) => {
                Self::fail_and_cleanup(&pool, &meeting_id, started_at, &e).await;
                return;
            }
        };

        // Dynamically fetch context size based on provider and model
        let token_threshold = if provider == LLMProvider::Ollama {
            match METADATA_CACHE
                .get_or_fetch(&model_name, ollama_endpoint.as_deref())
                .await
            {
                Ok(metadata) => {
                    // Reserve 300 tokens for prompt overhead
                    let optimal = metadata.context_size.saturating_sub(300);
                    info!(
                        "✓ Using dynamic context for {}: {} tokens (chunk size: {})",
                        model_name, metadata.context_size, optimal
                    );
                    optimal
                }
                Err(e) => {
                    warn!(
                        "Failed to fetch context for {}: {}. Using default 4000",
                        model_name, e
                    );
                    4000 // Fallback to safe default
                }
            }
        } else if provider == LLMProvider::BuiltInAI {
            // Get model's context size from registry
            use crate::summary::summary_engine::models;
            let model = models::get_model_by_name(&model_name)
                .ok_or_else(|| format!("Unknown model: {}", model_name));

            match model {
                Ok(model_def) => {
                    // Reserve 300 tokens for prompt overhead
                    let optimal = model_def.context_size.saturating_sub(300) as usize;
                    info!(
                        "✓ Using BuiltInAI context size: {} tokens (chunk size: {})",
                        model_def.context_size, optimal
                    );
                    optimal
                }
                Err(e) => {
                    warn!("{}, using default 2048", e);
                    1748 // 2048 - 300 for overhead
                }
            }
        } else {
            // Cloud providers (OpenAI, Claude, Groq, CustomOpenAI) handle large contexts automatically
            100000 // Effectively unlimited for single-pass processing
        };

        // Get app data directory for BuiltInAI provider
        let app_data_dir = app.path().app_data_dir().ok();

        let notes_prompt_override = crate::summary::prompt_settings::load_notes_prompt(&app);
        let effective_notes_prompt = match notes_prompt_override.as_deref().map(str::trim) {
            Some(trimmed) if !trimmed.is_empty() => trimmed.to_string(),
            _ => default_notes_system_prompt(),
        };

        if let Some(code) = &summary_language {
            info!("📝 Summary language preference: {}", code);
        }

        let detected_summary_language = Self::read_detected_summary_language(&pool, &meeting_id)
            .await
            .or_else(|| Self::detect_summary_language_from_text(&text));

        if let Some(code) = &detected_summary_language {
            info!("📝 Detected transcript summary language: {}", code);
        }

        let template = match templates::get_template(&template_id) {
            Ok(template) => template,
            Err(e) => {
                let err_msg = format!("Failed to load template '{}': {}", template_id, e);
                Self::fail_and_cleanup(&pool, &meeting_id, started_at, &err_msg).await;
                return;
            }
        };
        let template_fingerprint = template_cache_fingerprint(&template);

        let notes_prompt_fingerprint = stable_text_fingerprint(&effective_notes_prompt);

        let cache_source = build_summary_cache_source(
            &text,
            &custom_prompt,
            &template_id,
            &template_fingerprint,
            &notes_prompt_fingerprint,
            token_threshold,
            &model_provider,
            &model_name,
            ollama_endpoint.as_deref(),
            custom_openai_endpoint.as_deref(),
            custom_openai_max_tokens,
            custom_openai_temperature,
            custom_openai_top_p,
        );

        let cached_english = match SummaryProcessesRepository::get_summary_data(&pool, &meeting_id).await {
            Err(e) => {
                warn!(
                    "Failed to load prior summary row for cache lookup (meeting_id={}): {}. Falling back to full pass-1 generation.",
                    meeting_id, e
                );
                None
            }
            Ok(None) => None,
            Ok(Some(process)) => process.result.and_then(|raw| {
                match extract_cached_english_markdown(
                    &raw,
                    &cache_source,
                    summary_language.as_deref(),
                ) {
                    Ok(opt) => opt,
                    Err(e) => {
                        warn!(
                            "Cached summary result for meeting_id={} is not valid JSON ({}); ignoring cache.",
                            meeting_id, e
                        );
                        None
                    }
                }
            }),
        };

        let client = reqwest::Client::new();
        let result = generate_meeting_summary(
            &client,
            &provider,
            &model_name,
            &final_api_key,
            &text,
            &custom_prompt,
            &template_id,
            &template,
            Some(effective_notes_prompt.as_str()),
            token_threshold,
            ollama_endpoint.as_deref(),
            custom_openai_endpoint.as_deref(),
            custom_openai_max_tokens,
            custom_openai_temperature,
            custom_openai_top_p,
            app_data_dir.as_ref(),
            Some(&cancellation_token),
            summary_language.as_deref(),
            detected_summary_language.as_deref(),
            cached_english.as_deref(),
        )
        .await;

        let duration = start_time.elapsed().as_secs_f64();

        match result {
            Ok(generated) => {
                info!(
                    "✓ Successfully processed {} chunks for meeting_id: {}. Duration: {:.2}s",
                    generated.successful_chunk_count, meeting_id, duration
                );
                let result_json = match build_summary_result_json(
                    &generated.final_markdown,
                    &generated.english_markdown,
                    cache_source,
                    summary_language.as_deref(),
                    generated.reasoning_stripped,
                    generated.normalization_fallback,
                ) {
                    Ok(result) => result,
                    Err(error) => {
                        Self::update_process_failed(&pool, &meeting_id, started_at, &error).await;
                        Self::cleanup_cancellation_token(&meeting_id, started_at);
                        return;
                    }
                };

                match SummaryProcessesRepository::update_process_completed(
                    &pool,
                    &meeting_id,
                    started_at,
                    result_json,
                    generated.successful_chunk_count,
                    duration,
                )
                .await
                {
                    Ok(true) => {
                        if let Some(name) =
                            extract_meeting_name_from_markdown(&generated.final_markdown)
                                .filter(|name| !name.is_empty())
                        {
                            // Old notes-prompt overrides still carry the example title,
                            // which small models copy verbatim instead of naming the meeting.
                            if is_prompt_echo_title(&name, &text) {
                                warn!(
                                    "Keeping meeting name for {}: generated title '{}' echoes the prompt example",
                                    meeting_id, name
                                );
                            } else if let Err(error) =
                                MeetingsRepository::update_meeting_name(&pool, &meeting_id, &name)
                                    .await
                            {
                                error!(
                                    "Failed to update meeting name for {}: {}",
                                    meeting_id, error
                                );
                            }
                        }
                        info!("Summary saved successfully for meeting_id: {}", meeting_id);
                    }
                    Ok(false) => warn!(
                        "Skipped stale summary completion for meeting_id: {}",
                        meeting_id
                    ),
                    Err(error) => error!(
                        "Failed to save completed process for {}: {}",
                        meeting_id, error
                    ),
                }
            }
            Err(_) if cancellation_token.is_cancelled() => {
                match SummaryProcessesRepository::update_process_cancelled(
                    &pool,
                    &meeting_id,
                    started_at,
                )
                .await
                {
                    Ok(false) => warn!(
                        "Skipped stale summary cancellation for meeting_id: {}",
                        meeting_id
                    ),
                    Ok(true) => info!(
                        "Summary generation was cancelled for meeting_id: {}",
                        meeting_id
                    ),
                    Err(db_error) => error!(
                        "Failed to update DB status to cancelled for {}: {}",
                        meeting_id, db_error
                    ),
                }
            }
            Err(error) => {
                Self::update_process_failed(&pool, &meeting_id, started_at, &error).await;
            }
        }
        Self::cleanup_cancellation_token(&meeting_id, started_at);
    }

    /// Asks the configured summary model for a fresh meeting title from the stored
    /// transcript and the user's notes, saves it, and returns it.
    pub async fn regenerate_meeting_title<R: tauri::Runtime>(
        app: &AppHandle<R>,
        pool: &SqlitePool,
        meeting_id: &str,
    ) -> Result<String, String> {
        let model_config = SettingsRepository::get_model_config(pool)
            .await
            .map_err(|e| format!("Failed to load summary model settings: {}", e))?
            .filter(|config| !config.provider.trim().is_empty() && !config.model.trim().is_empty())
            .ok_or_else(|| "No summary model configured".to_string())?;
        let config = load_llm_config(
            pool,
            &model_config.provider,
            &model_config.model,
            Some(&model_config),
        )
        .await?;

        let (segments, _) =
            MeetingsRepository::get_meeting_transcripts_paginated(pool, meeting_id, i64::MAX, 0)
                .await
                .map_err(|e| format!("Failed to load transcript for {}: {}", meeting_id, e))?;
        let transcript = title_transcript_text(&segments);
        if transcript.trim().is_empty() {
            return Err("This meeting has no transcript to generate a title from".to_string());
        }

        let notes = sqlx::query_scalar::<_, Option<String>>(
            "SELECT notes_markdown FROM meeting_notes WHERE meeting_id = ?",
        )
        .bind(meeting_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| format!("Failed to load notes for {}: {}", meeting_id, e))?
        .flatten();

        info!(
            "Regenerating title for meeting_id {} with {}/{}",
            meeting_id, model_config.provider, config.model_name
        );
        let app_data_dir = app.path().app_data_dir().ok();
        let client = reqwest::Client::new();
        let completion = generate_summary(
            &client,
            &config.provider,
            &config.model_name,
            &config.api_key,
            title_system_prompt(),
            &build_title_user_prompt(&transcript, notes.as_deref()),
            config.ollama_endpoint.as_deref(),
            config.custom_openai_endpoint.as_deref(),
            config.max_tokens,
            config.temperature,
            config.top_p,
            app_data_dir.as_ref(),
            None,
        )
        .await?;

        let title = sanitize_generated_title(&completion.content)
            .ok_or_else(|| "The model did not return a usable title".to_string())?;

        match MeetingsRepository::update_meeting_name(pool, meeting_id, &title).await {
            Ok(true) => {
                info!("Regenerated title for meeting_id {}: {}", meeting_id, title);
                Ok(title)
            }
            Ok(false) => Err(format!("Meeting not found: {}", meeting_id)),
            Err(e) => Err(format!("Failed to save meeting title: {}", e)),
        }
    }

    /// Updates the summary process status to failed with error message
    async fn fail_and_cleanup(
        pool: &SqlitePool,
        meeting_id: &str,
        started_at: DateTime<Utc>,
        error_msg: &str,
    ) {
        Self::update_process_failed(pool, meeting_id, started_at, error_msg).await;
        Self::cleanup_cancellation_token(meeting_id, started_at);
    }

    async fn update_process_failed(
        pool: &SqlitePool,
        meeting_id: &str,
        started_at: DateTime<Utc>,
        error_msg: &str,
    ) {
        error!(
            "Processing failed for meeting_id {}: {}",
            meeting_id, error_msg
        );
        match SummaryProcessesRepository::update_process_failed(
            pool, meeting_id, started_at, error_msg,
        )
        .await
        {
            Ok(false) => warn!(
                "Skipped stale summary failure for meeting_id: {}",
                meeting_id
            ),
            Ok(true) => {}
            Err(e) => error!(
                "Failed to update DB status to failed for {}: {}",
                meeting_id, e
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_cleanup_keeps_the_replacement_cancellation_token() {
        let meeting_id = format!("summary-test-{}", uuid::Uuid::new_v4());
        let first_started_at = Utc::now();
        let second_started_at = first_started_at + chrono::Duration::nanoseconds(1);
        let first = SummaryService::register_cancellation_token(&meeting_id, first_started_at);
        let second = SummaryService::register_cancellation_token(&meeting_id, second_started_at);

        assert!(first.is_cancelled());
        SummaryService::cleanup_cancellation_token(&meeting_id, first_started_at);
        assert!(SummaryService::cancel_summary(
            &meeting_id,
            second_started_at
        ));
        assert!(second.is_cancelled());
        SummaryService::cleanup_cancellation_token(&meeting_id, second_started_at);
    }

    fn segment(text: &str, speaker: Option<&str>) -> Transcript {
        Transcript {
            id: uuid::Uuid::new_v4().to_string(),
            meeting_id: "meeting-1".to_string(),
            transcript: text.to_string(),
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            speaker: speaker.map(str::to_string),
            summary: None,
            action_items: None,
            key_points: None,
            audio_start_time: None,
            audio_end_time: None,
            duration: None,
        }
    }

    #[test]
    fn title_transcript_text_labels_speakers_in_order_and_skips_blank_segments() {
        let segments = [
            segment("  Kick off the hiring review. ", Some("mic")),
            segment("   ", Some("system")),
            segment("Two offers are out.", Some("system")),
            segment("I own the budget.", Some("Priya")),
            segment("Unattributed line", None),
            segment("Blank speaker", Some("  ")),
        ];
        assert_eq!(
            title_transcript_text(&segments),
            "[You] Kick off the hiring review.\n[Others] Two offers are out.\n[Priya] I own the budget.\nUnattributed line\nBlank speaker"
        );
    }

    #[test]
    fn title_transcript_text_is_empty_without_spoken_text() {
        assert_eq!(title_transcript_text(&[]), "");
        assert_eq!(title_transcript_text(&[segment(" \n ", Some("mic"))]), "");
    }

    #[tokio::test]
    async fn llm_config_resolves_keyless_providers_and_rejects_unknown_ones() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        let config = load_llm_config(&pool, "builtin-ai", "gemma3:1b", None)
            .await
            .unwrap_or_else(|e| panic!("builtin-ai should need no settings: {e}"));
        assert_eq!(config.provider, LLMProvider::BuiltInAI);
        assert_eq!(config.model_name, "gemma3:1b");
        assert!(config.api_key.is_empty());
        assert!(config.ollama_endpoint.is_none());
        assert!(config.custom_openai_endpoint.is_none());

        assert_eq!(
            load_llm_config(&pool, "not-a-provider", "x", None)
                .await
                .err(),
            Some("Unsupported LLM provider: not-a-provider".to_string())
        );
    }

    #[test]
    fn test_strip_leading_title_with_body() {
        let input = "# Meeting Title\nThis is the body.\nMore content.";
        let result = strip_leading_title(input);
        assert_eq!(result, "This is the body.\nMore content.");
    }

    #[test]
    fn test_strip_leading_title_only() {
        let input = "# Meeting Title";
        let result = strip_leading_title(input);
        assert_eq!(result, "");
    }

    #[test]
    fn test_strip_leading_title_no_heading() {
        let input = "No heading here.\nJust body.";
        let result = strip_leading_title(input);
        assert_eq!(result, "");
    }

    #[test]
    fn test_strip_leading_title_multiline_body() {
        let input = "# Title\n## Subheading\nParagraph 1\n\nParagraph 2";
        let result = strip_leading_title(input);
        assert_eq!(result, "## Subheading\nParagraph 1\n\nParagraph 2");
    }

    #[test]
    fn test_strip_leading_title_empty_after_heading() {
        let input = "# Title\n";
        let result = strip_leading_title(input);
        assert_eq!(result, "");
    }

    #[test]
    fn test_strip_leading_title_whitespace_after_heading() {
        let input = "# Title\n   \n Body with leading spaces";
        let result = strip_leading_title(input);
        assert_eq!(result, "Body with leading spaces");
    }

    #[test]
    fn test_strip_title_if_present_preserves_already_stripped() {
        assert_eq!(
            strip_title_if_present("## Action Items\nfoo"),
            "## Action Items\nfoo"
        );
    }

    #[test]
    fn test_strip_title_if_present_strips_leading_h1() {
        assert_eq!(
            strip_title_if_present("# Meeting Title\n## Action Items\nfoo"),
            "## Action Items\nfoo"
        );
    }

    #[test]
    fn test_strip_title_if_present_no_heading_preserved() {
        // Distinct from strip_leading_title which returns "" — this preserves input.
        assert_eq!(strip_title_if_present("Just body text"), "Just body text");
    }

    #[test]
    fn test_strip_title_if_present_hash_no_space_preserved() {
        // `#NoSpace` is not a markdown H1 — preserve.
        assert_eq!(strip_title_if_present("#NoSpace\nbody"), "#NoSpace\nbody");
    }

    #[test]
    fn test_strip_title_if_present_mid_document_h1_preserved() {
        // H1 after body content must NOT be stripped — guards the asymmetry where
        // extract_meeting_name_from_markdown scans every line for "# ".
        let input = "Some paragraph\n\n# H1 on line 3\n## Section\nbody";
        assert_eq!(strip_title_if_present(input), input);
    }

    #[test]
    fn test_strip_title_if_present_leading_whitespace_h1_stripped() {
        assert_eq!(
            strip_title_if_present("  # Title\n## Section\nbody"),
            "## Section\nbody"
        );
    }

    fn sample_notes_prompt_fingerprint() -> String {
        stable_text_fingerprint("standard notes prompt")
    }

    fn sample_cache_source() -> SummaryCacheSource {
        let template_fingerprint = stable_text_fingerprint("standard template prompt");
        let notes_prompt_fingerprint = sample_notes_prompt_fingerprint();
        build_summary_cache_source(
            "transcript body",
            "custom prompt",
            "standard_meeting",
            &template_fingerprint,
            &notes_prompt_fingerprint,
            3700,
            "ollama",
            "gemma3:1b",
            Some("http://localhost:11434"),
            None,
            None,
            None,
            None,
        )
    }

    fn test_template(section_title: &str) -> Template {
        Template {
            name: "Test".to_string(),
            description: "Test template".to_string(),
            sections: vec![crate::summary::templates::TemplateSection {
                title: section_title.to_string(),
                instruction: "Summarize this section".to_string(),
                format: "paragraph".to_string(),
                item_format: None,
                example_item_format: None,
            }],
        }
    }

    #[test]
    fn test_template_cache_fingerprint_changes_with_rendered_template() {
        assert_ne!(
            template_cache_fingerprint(&test_template("Summary")),
            template_cache_fingerprint(&test_template("Decisions"))
        );
    }

    #[test]
    fn test_legacy_english_markdown_field_is_cache_miss() {
        let raw = serde_json::json!({
            "markdown": "translated",
            "english_markdown": "# Old English\nBody"
        })
        .to_string();

        assert_eq!(
            extract_cached_english_markdown(&raw, &sample_cache_source(), Some("de")).unwrap(),
            None
        );
    }

    #[test]
    fn test_matching_source_changed_translation_target_reuses_cache() {
        let source = sample_cache_source();
        let raw = build_summary_result_json(
            "# Reunion\n## Points\nBonjour",
            "# Meeting\n## Points\nHello",
            source.clone(),
            Some("fr"),
            false,
            false,
        )
        .unwrap()
        .to_string();

        assert_eq!(
            extract_cached_english_markdown(&raw, &source, Some("de")).unwrap(),
            Some("# Meeting\n## Points\nHello".to_string())
        );
    }

    #[test]
    fn test_same_language_regeneration_rejects_cache() {
        let source = sample_cache_source();
        let raw = build_summary_result_json(
            "# Reunion\n## Points\nBonjour",
            "# Meeting\n## Points\nHello",
            source.clone(),
            Some("fr"),
            false,
            false,
        )
        .unwrap()
        .to_string();

        assert_eq!(
            extract_cached_english_markdown(&raw, &source, Some("fr")).unwrap(),
            None
        );
    }

    #[test]
    fn test_changed_summary_inputs_reject_cache() {
        let source = sample_cache_source();
        let template_fingerprint = source.template_fingerprint.clone();
        let notes_prompt_fingerprint = source.notes_prompt_fingerprint.clone();
        let raw = build_summary_result_json(
            "# Reunion\n## Points\nBonjour",
            "# Meeting\n## Points\nHello",
            source,
            Some("fr"),
            false,
            false,
        )
        .unwrap()
        .to_string();

        let changed_sources = [
            build_summary_cache_source(
                "changed transcript",
                "custom prompt",
                "standard_meeting",
                &template_fingerprint,
                &notes_prompt_fingerprint,
                3700,
                "ollama",
                "gemma3:1b",
                Some("http://localhost:11434"),
                None,
                None,
                None,
                None,
            ),
            build_summary_cache_source(
                "transcript body",
                "changed prompt",
                "standard_meeting",
                &template_fingerprint,
                &notes_prompt_fingerprint,
                3700,
                "ollama",
                "gemma3:1b",
                Some("http://localhost:11434"),
                None,
                None,
                None,
                None,
            ),
            build_summary_cache_source(
                "transcript body",
                "custom prompt",
                "daily_standup",
                &template_fingerprint,
                &notes_prompt_fingerprint,
                3700,
                "ollama",
                "gemma3:1b",
                Some("http://localhost:11434"),
                None,
                None,
                None,
                None,
            ),
            build_summary_cache_source(
                "transcript body",
                "custom prompt",
                "standard_meeting",
                &template_fingerprint,
                &notes_prompt_fingerprint,
                3700,
                "openai",
                "gemma3:1b",
                Some("http://localhost:11434"),
                None,
                None,
                None,
                None,
            ),
            build_summary_cache_source(
                "transcript body",
                "custom prompt",
                "standard_meeting",
                &template_fingerprint,
                &notes_prompt_fingerprint,
                3700,
                "ollama",
                "qwen2.5:3b",
                Some("http://localhost:11434"),
                None,
                None,
                None,
                None,
            ),
            build_summary_cache_source(
                "transcript body",
                "custom prompt",
                "standard_meeting",
                &template_fingerprint,
                &notes_prompt_fingerprint,
                3700,
                "ollama",
                "gemma3:1b",
                Some("http://localhost:11500"),
                None,
                None,
                None,
                None,
            ),
            build_summary_cache_source(
                "transcript body",
                "custom prompt",
                "standard_meeting",
                &template_fingerprint,
                &notes_prompt_fingerprint,
                3700,
                "ollama",
                "gemma3:1b",
                Some("http://localhost:11434"),
                Some("https://custom.example/v1"),
                Some(2048),
                Some(0.2),
                Some(0.9),
            ),
        ];

        for changed_source in changed_sources {
            assert_eq!(
                extract_cached_english_markdown(&raw, &changed_source, Some("de")).unwrap(),
                None
            );
        }
    }

    #[test]
    fn test_changed_template_content_rejects_cache() {
        let source = sample_cache_source();
        let raw = build_summary_result_json(
            "# Reunion\n## Points\nBonjour",
            "# Meeting\n## Points\nHello",
            source.clone(),
            Some("fr"),
            false,
            false,
        )
        .unwrap()
        .to_string();

        let changed_template = SummaryCacheSource {
            template_fingerprint: stable_text_fingerprint("changed template prompt"),
            ..source
        };

        assert_eq!(
            extract_cached_english_markdown(&raw, &changed_template, Some("de")).unwrap(),
            None
        );
    }

    #[test]
    fn test_changed_notes_prompt_rejects_cache() {
        let source = sample_cache_source();
        let raw = build_summary_result_json(
            "# Reunion\n## Points\nBonjour",
            "# Meeting\n## Points\nHello",
            source.clone(),
            Some("fr"),
            false,
            false,
        )
        .unwrap()
        .to_string();

        let changed_notes_prompt = SummaryCacheSource {
            notes_prompt_fingerprint: stable_text_fingerprint("changed notes prompt"),
            ..source
        };

        assert_eq!(
            extract_cached_english_markdown(&raw, &changed_notes_prompt, Some("de")).unwrap(),
            None
        );
    }

    #[test]
    fn test_changed_token_threshold_rejects_cache() {
        let source = sample_cache_source();
        let raw = build_summary_result_json(
            "# Reunion\n## Points\nBonjour",
            "# Meeting\n## Points\nHello",
            source.clone(),
            Some("fr"),
            false,
            false,
        )
        .unwrap()
        .to_string();

        let changed_threshold = SummaryCacheSource {
            token_threshold: 8192,
            ..source
        };

        assert_eq!(
            extract_cached_english_markdown(&raw, &changed_threshold, Some("de")).unwrap(),
            None
        );
    }

    #[test]
    fn test_result_json_strips_display_markdown_but_keeps_cache_title() {
        let result = build_summary_result_json(
            "# Translated Title\n## Decisions\nDone",
            "# English Title\n## Decisions\nDone",
            sample_cache_source(),
            Some("fr"),
            false,
            false,
        )
        .unwrap();

        assert_eq!(result["markdown"], "## Decisions\nDone");
        assert_eq!(
            result["english_cache"]["markdown"],
            "# English Title\n## Decisions\nDone"
        );
    }

    #[test]
    fn result_json_rejects_title_only_display_markdown() {
        assert_eq!(
            build_summary_result_json(
                "# Title",
                "# Title",
                sample_cache_source(),
                None,
                false,
                false,
            ),
            Err("Final summary contains no visible content after title removal".to_string())
        );
    }

    #[test]
    fn result_json_never_persists_reasoning_text() {
        let result = build_summary_result_json(
            "# Title\nVisible",
            "# Title\nVisible",
            sample_cache_source(),
            None,
            true,
            false,
        )
        .unwrap();
        assert_eq!(result["reasoning_stripped"], true);
        assert!(result.get("reasoning").is_none());
    }

    #[test]
    fn test_extract_cached_english_from_malformed_json_errors() {
        let raw = r#"{ not valid json"#;
        assert!(extract_cached_english_markdown(raw, &sample_cache_source(), Some("de")).is_err());
    }
}
