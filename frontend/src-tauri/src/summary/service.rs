use crate::database::models::{Setting, Transcript};
use crate::database::repositories::{
    meeting::{MeetingsRepository, TitleSource, TitleUpdate},
    setting::SettingsRepository,
    summary::SummaryProcessesRepository,
};
use crate::ollama::metadata::ModelMetadataCache;
use crate::summary::budget::{fit_builtin_output, ContextBudget};
use crate::summary::llm_client::{generate_summary, GenerationOptions, LLMProvider};
use crate::summary::markdown::{clean_llm_markdown_detailed, require_visible_markdown};
use crate::summary::title::{
    build_title_user_prompt_with, extract_meeting_name_from_markdown, sanitize_generated_title,
    title_system_prompt,
};
use chrono::{DateTime, Utc};
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

/// When an app call to an Ollama model happens. Ollama reloads the model
/// whenever `num_ctx` changes, and sizes its KV cache to it, so every call
/// in one phase sends the same value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OllamaPhase {
    /// While a recording runs: the live summary timeline, meeting chat and
    /// the live copilot. Kept small so the KV cache stays affordable next to
    /// live transcription (16k on an 8B model is ~2 GB, too much for an
    /// 8 GB Mac).
    Recording,
    /// After the meeting: titles and chat. One model reload when the meeting
    /// ends is the price of the larger context.
    AfterMeeting,
}

/// `num_ctx` for every Ollama call made while recording.
pub(crate) const OLLAMA_RECORDING_NUM_CTX: usize = 8_192;
/// Context assumed for a model whose real context is unknown: its metadata
/// could not be read, or only gave a family default or the 4000 fallback.
const OLLAMA_UNKNOWN_CONTEXT: usize = 8_192;
/// Smallest `num_ctx` a title call gets: its prompt carries up to 12k
/// characters of transcript.
const OLLAMA_MIN_TITLE_NUM_CTX: usize = 8_192;

/// The context window to run an Ollama model with in `phase`, given the
/// model's own context (None when unknown).
/// - Recording: 8192, or the model's context when it is known and smaller.
/// - After the meeting: the model's context capped for memory
///   (`ContextBudget::for_ollama`), 8192 when unknown.
pub(crate) fn ollama_context_for(phase: OllamaPhase, model_context: Option<usize>) -> usize {
    match phase {
        OllamaPhase::Recording => model_context.map_or(OLLAMA_RECORDING_NUM_CTX, |known| {
            known.min(OLLAMA_RECORDING_NUM_CTX)
        }),
        OllamaPhase::AfterMeeting => {
            ContextBudget::for_ollama(model_context.unwrap_or(OLLAMA_UNKNOWN_CONTEXT)).n_ctx
        }
    }
}

/// The model's own context from Ollama's metadata (cached), or None when it
/// cannot be read or is only a guess.
async fn ollama_model_context(model_name: &str, endpoint: Option<&str>) -> Option<usize> {
    match METADATA_CACHE.get_or_fetch(model_name, endpoint).await {
        Ok(metadata) if metadata.context_known => Some(metadata.context_size),
        Ok(metadata) => {
            info!(
                "Ollama context for {} is a guess ({} tokens); treating it as unknown",
                model_name, metadata.context_size
            );
            None
        }
        Err(e) => {
            warn!("Failed to fetch context for {}: {}", model_name, e);
            None
        }
    }
}

/// The phase a call made now belongs to.
pub(crate) async fn ollama_phase_now() -> OllamaPhase {
    if crate::audio::recording_commands::is_recording().await {
        OllamaPhase::Recording
    } else {
        OllamaPhase::AfterMeeting
    }
}

/// `num_ctx` for an Ollama call in `phase` (see [`ollama_context_for`]);
/// None for other providers. Every Ollama call goes through this, so calls
/// in one phase never make Ollama reload the model.
pub(crate) async fn ollama_num_ctx_for(
    provider: &LLMProvider,
    model_name: &str,
    endpoint: Option<&str>,
    phase: OllamaPhase,
) -> Option<u32> {
    if *provider != LLMProvider::Ollama {
        return None;
    }
    let context = ollama_context_for(phase, ollama_model_context(model_name, endpoint).await);
    Some(u32::try_from(context).unwrap_or(u32::MAX))
}

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

/// The stored result of an AI notes job: the notes without their title
/// line (the title becomes the meeting's name), the model the job ran for
/// (`audio::model_provenance` reads it) and whether reasoning was removed.
fn build_summary_result_json(
    markdown: &str,
    model_provider: &str,
    model_name: &str,
) -> Result<serde_json::Value, String> {
    let cleaned = clean_llm_markdown_detailed(markdown);
    require_visible_markdown("AI notes", &cleaned)?;
    let body = strip_title_if_present(&cleaned.markdown);
    if body.trim().is_empty() {
        return Err("AI notes contain no visible content after title removal".to_string());
    }
    Ok(serde_json::json!({
        "markdown": body,
        crate::audio::model_provenance::SUMMARY_MODEL_FIELD: {
            "provider": model_provider,
            "model": model_name,
        },
        "reasoning_stripped": cleaned.reasoning_stripped,
    }))
}

/// Tells the notes editor a meeting's tidy pass result is stored.
pub const TIDY_NOTES_READY_EVENT: &str = "tidy-notes-ready";
/// The result field holding the tidy pass's wrap-up.
pub const TIDY_WRAP_UP_FIELD: &str = "tidy_wrap_up";

/// The stored result with the tidy pass's wrap-up, when it wrote the notes.
fn with_tidy_wrap_up(
    mut result: serde_json::Value,
    wrap_up: Option<&crate::summary::tidy::TidyWrapUp>,
) -> serde_json::Value {
    if let (Some(wrap_up), Some(object)) = (wrap_up, result.as_object_mut()) {
        if let Ok(value) = serde_json::to_value(wrap_up) {
            object.insert(TIDY_WRAP_UP_FIELD.to_string(), value);
        }
    }
    result
}

/// Everything an LLM call needs beyond the prompts, resolved from saved settings.
pub(crate) struct LlmConfig {
    pub(crate) provider: LLMProvider,
    pub(crate) model_name: String,
    pub(crate) api_key: String,
    pub(crate) ollama_endpoint: Option<String>,
    pub(crate) custom_openai_endpoint: Option<String>,
    pub(crate) max_tokens: Option<u32>,
    pub(crate) temperature: Option<f32>,
    pub(crate) top_p: Option<f32>,
}

/// Resolves the API key, Ollama endpoint and CustomOpenAI settings for a provider/model pair.
/// `settings` is the already-loaded settings row, when the caller has one, so Ollama's
/// endpoint lookup does not read it again.
pub(crate) async fn load_llm_config(
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

/// The error a summary gets when the job running it is gone: the app quit or
/// crashed mid-run, so nothing will ever finish the pending row.
pub const INTERRUPTED_SUMMARY_ERROR: &str =
    "Interrupted: the app was closed while this summary was running. Generate it again.";

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

    /// True while this process runs the generation identified by `started_at`.
    /// A pending row without one was orphaned by a quit, crash or lost write.
    pub(crate) fn has_active_job(meeting_id: &str, started_at: DateTime<Utc>) -> bool {
        CANCELLATION_REGISTRY
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(meeting_id)
            .is_some_and(|entry| entry.started_at == started_at)
    }

    /// True while this process runs any summary generation for the meeting.
    pub(crate) fn is_generating(meeting_id: &str) -> bool {
        CANCELLATION_REGISTRY
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains_key(meeting_id)
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

    /// Writes a meeting's AI notes in the background, from its live notes
    /// (`live_enhanced::notes_from_live_notes`), and stores them. Stretches the
    /// live notes missed are written first from the saved transcript
    /// (`live_summary::backfill`). There is no other way to write them: when
    /// that fails, the job fails with the reason and the UI offers a retry.
    ///
    /// # Arguments
    /// * `app` - Tauri app handle (app data dir and the live notes model choice)
    /// * `pool` - SQLx connection pool
    /// * `meeting_id` - Unique identifier for the meeting
    /// * `text` - Full transcript text, one `[mm:ss] [Speaker] text` line per segment
    /// * `model_provider` / `model_name` - the summary model the job was started
    ///   for, recorded with the result
    /// * `custom_prompt` - The user's notes as Markdown
    /// * `regenerate` - Re-enhance: every section is written again
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
        regenerate: bool,
    ) {
        let start_time = Instant::now();
        info!(
            "Starting background processing for meeting_id: {}",
            meeting_id
        );

        let result = crate::summary::live_enhanced::notes_from_live_notes(
            &app,
            &pool,
            &meeting_id,
            &text,
            &custom_prompt,
            regenerate,
            &cancellation_token,
        )
        .await;
        let duration = start_time.elapsed().as_secs_f64();

        match result {
            Ok(notes) => {
                info!(
                    "✓ AI notes written for meeting_id: {} with {} model calls. Duration: {:.2}s",
                    meeting_id, notes.calls, duration
                );
                // The notes document's wrap-up from the tidy pass, stored with
                // the summary for the notes editor (`completeAiNotes`).
                let tidy_wrap_up = notes.tidy.clone();
                let result_json = match build_summary_result_json(
                    &notes.markdown,
                    &model_provider,
                    &model_name,
                ) {
                    Ok(result) => with_tidy_wrap_up(result, tidy_wrap_up.as_ref()),
                    Err(error) => {
                        Self::fail_and_cleanup(&pool, &meeting_id, started_at, &error).await;
                        return;
                    }
                };

                match SummaryProcessesRepository::update_process_completed(
                    &pool,
                    &meeting_id,
                    started_at,
                    result_json,
                    notes.calls as i64,
                    duration,
                )
                .await
                {
                    Ok(true) => {
                        if let Some(name) = extract_meeting_name_from_markdown(&notes.markdown)
                            .filter(|name| !name.is_empty())
                        {
                            match MeetingsRepository::update_meeting_name(
                                &pool,
                                &meeting_id,
                                &name,
                                TitleSource::Generated,
                            )
                            .await
                            {
                                Ok(TitleUpdate::Kept) => info!(
                                    "Keeping the user's title for {}: not renaming it to '{}'",
                                    meeting_id, name
                                ),
                                Ok(_) => {}
                                Err(error) => error!(
                                    "Failed to update meeting name for {}: {}",
                                    meeting_id, error
                                ),
                            }
                        }
                        info!("Summary saved successfully for meeting_id: {}", meeting_id);
                        if tidy_wrap_up.is_some() {
                            use tauri::Emitter;
                            let _ = app.emit(
                                TIDY_NOTES_READY_EVENT,
                                serde_json::json!({ "meetingId": meeting_id }),
                            );
                        }
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
        let attendees: Vec<String> = crate::people::meeting_attendees(pool, meeting_id)
            .await
            .unwrap_or_else(|e| {
                warn!("Could not load attendees for {}: {}", meeting_id, e);
                Vec::new()
            })
            .into_iter()
            .map(|person| person.name)
            .collect();
        let title_user_prompt =
            build_title_user_prompt_with(&transcript, notes.as_deref(), &attendees);
        let title_max_tokens = fit_builtin_output(
            &config.provider,
            &config.model_name,
            app_data_dir.as_ref(),
            title_system_prompt(),
            &title_user_prompt,
            config.max_tokens,
        )
        .await;
        let completion = generate_summary(
            &client,
            &config.provider,
            &config.model_name,
            &config.api_key,
            title_system_prompt(),
            &title_user_prompt,
            config.ollama_endpoint.as_deref(),
            config.custom_openai_endpoint.as_deref(),
            &GenerationOptions {
                max_tokens: title_max_tokens,
                temperature: config.temperature,
                top_p: config.top_p,
                // The title prompt carries up to 12k characters of transcript,
                // so it never runs below 8192, whatever the metadata says.
                num_ctx: ollama_num_ctx_for(
                    &config.provider,
                    &config.model_name,
                    config.ollama_endpoint.as_deref(),
                    ollama_phase_now().await,
                )
                .await
                .map(|num_ctx| num_ctx.max(OLLAMA_MIN_TITLE_NUM_CTX as u32)),
                ..Default::default()
            },
            app_data_dir.as_ref(),
            None,
        )
        .await?;

        let title = sanitize_generated_title(&completion.content)
            .ok_or_else(|| "The model did not return a usable title".to_string())?;

        // The user asked for this title, so it is saved even over their own
        // rename, and later automatic passes keep it.
        match MeetingsRepository::update_meeting_name(
            pool,
            meeting_id,
            &title,
            TitleSource::Requested,
        )
        .await
        {
            Ok(TitleUpdate::Saved) => {
                info!("Regenerated title for meeting_id {}: {}", meeting_id, title);
                Ok(title)
            }
            Ok(TitleUpdate::Kept | TitleUpdate::NotFound) => {
                Err(format!("Meeting not found: {}", meeting_id))
            }
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

    #[test]
    fn ollama_context_is_small_while_recording_and_never_trusts_a_guess() {
        // While recording: 8192, or a known smaller model context.
        assert_eq!(
            ollama_context_for(OllamaPhase::Recording, Some(131_072)),
            8_192
        );
        assert_eq!(ollama_context_for(OllamaPhase::Recording, None), 8_192);
        assert_eq!(
            ollama_context_for(OllamaPhase::Recording, Some(4_096)),
            4_096
        );
        // After the meeting: the model's context capped at 16k, and an
        // unknown context (the 4000/2048 guesses) is 8192, not the guess.
        assert_eq!(
            ollama_context_for(OllamaPhase::AfterMeeting, Some(131_072)),
            16_384
        );
        assert_eq!(ollama_context_for(OllamaPhase::AfterMeeting, None), 8_192);
        assert_eq!(
            ollama_context_for(OllamaPhase::AfterMeeting, Some(8_192)),
            8_192
        );
        assert!(OLLAMA_MIN_TITLE_NUM_CTX >= 8_192);
    }

    #[test]
    fn result_json_strips_the_title_and_records_the_model() {
        let result =
            build_summary_result_json("# Title\n## Decisions\nDone", "builtin-ai", "gemma4:e2b")
                .unwrap();
        assert_eq!(result["markdown"], "## Decisions\nDone");
        assert_eq!(
            result["model"],
            serde_json::json!({ "provider": "builtin-ai", "model": "gemma4:e2b" })
        );
        assert!(result.get("english_cache").is_none());
    }

    #[test]
    fn result_json_carries_the_tidy_wrap_up_only_when_the_tidy_pass_wrote_the_notes() {
        let result = || {
            build_summary_result_json("# Title\n## Summary\nDone", "builtin-ai", "gemma4:e2b")
                .unwrap()
        };
        assert!(with_tidy_wrap_up(result(), None)
            .get(TIDY_WRAP_UP_FIELD)
            .is_none());
        let wrap_up = crate::summary::tidy::TidyWrapUp {
            summary: "Done".into(),
            decisions: vec!["Ship it".into()],
            action_items: vec!["Priya — Send the deck".into()],
            open_questions: Vec::new(),
        };
        let stored = with_tidy_wrap_up(result(), Some(&wrap_up));
        assert_eq!(stored["markdown"], "## Summary\nDone");
        assert_eq!(
            stored[TIDY_WRAP_UP_FIELD],
            serde_json::json!({
                "summary": "Done",
                "decisions": ["Ship it"],
                "action_items": ["Priya — Send the deck"],
                "open_questions": [],
            })
        );
    }

    #[test]
    fn result_json_rejects_title_only_display_markdown() {
        assert_eq!(
            build_summary_result_json("# Title", "builtin-ai", "gemma4:e2b"),
            Err("AI notes contain no visible content after title removal".to_string())
        );
    }

    #[test]
    fn result_json_never_persists_reasoning_text() {
        let result = build_summary_result_json(
            "<think>private</think>\n# Title\nVisible",
            "builtin-ai",
            "gemma4:e2b",
        )
        .unwrap();
        assert_eq!(result["reasoning_stripped"], true);
        assert_eq!(result["markdown"], "Visible");
        assert!(result.get("reasoning").is_none());
    }
}
