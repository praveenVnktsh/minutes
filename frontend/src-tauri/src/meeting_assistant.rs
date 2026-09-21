use crate::database::repositories::setting::SettingsRepository;
use crate::state::AppState;
use crate::summary::llm_client::{generate_summary, LLMProvider};
use crate::summary::processor::clean_llm_markdown_detailed;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, Runtime, State};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingChatMessage {
    pub id: String,
    pub role: String,
    pub content: String,
    pub created_at: String,
}

/// `summary_processes.result` holds `{"markdown": ..., "summary_json": [...]}`; anything else
/// stored there is treated as the notes themselves rather than shown to the model as a JSON blob.
fn enhanced_notes_markdown(stored_result: &str) -> String {
    let trimmed = stored_result.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    serde_json::from_str::<serde_json::Value>(trimmed)
        .ok()
        .and_then(|value| {
            value
                .get("markdown")
                .and_then(|markdown| markdown.as_str())
                .map(|markdown| markdown.trim().to_string())
        })
        .unwrap_or_else(|| trimmed.to_string())
}

fn assistant_reply(raw_content: &str) -> Result<String, String> {
    let cleaned = clean_llm_markdown_detailed(raw_content);
    let reply = if cleaned.markdown.trim().is_empty() {
        raw_content.trim()
    } else {
        cleaned.markdown.trim()
    };
    if reply.is_empty() {
        return Err("The assistant did not return a reply".to_string());
    }
    Ok(reply.to_string())
}

async fn save_chat_message(
    pool: &sqlx::SqlitePool,
    meeting_id: &str,
    role: &str,
    content: &str,
) -> Result<MeetingChatMessage, String> {
    let message = MeetingChatMessage {
        id: Uuid::new_v4().to_string(),
        role: role.to_string(),
        content: content.to_string(),
        created_at: Utc::now().to_rfc3339(),
    };
    sqlx::query(
        "INSERT INTO meeting_chat_messages (id, meeting_id, role, content, created_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&message.id)
    .bind(meeting_id)
    .bind(role)
    .bind(content)
    .bind(&message.created_at)
    .execute(pool)
    .await
    .map_err(|error| format!("Could not save meeting chat: {error}"))?;
    Ok(message)
}

#[tauri::command]
pub async fn get_meeting_chat(
    meeting_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<MeetingChatMessage>, String> {
    let rows = sqlx::query_as::<_, (String, String, String, String)>(
        "SELECT id, role, content, created_at FROM meeting_chat_messages WHERE meeting_id = ? ORDER BY created_at ASC",
    )
    .bind(meeting_id)
    .fetch_all(state.db_manager.pool())
    .await
    .map_err(|error| format!("Could not load meeting chat: {error}"))?;
    Ok(rows
        .into_iter()
        .map(|(id, role, content, created_at)| MeetingChatMessage {
            id,
            role,
            content,
            created_at,
        })
        .collect())
}

#[tauri::command]
pub async fn chat_with_meeting<R: Runtime>(
    app: AppHandle<R>,
    meeting_id: String,
    message: String,
    state: State<'_, AppState>,
) -> Result<MeetingChatMessage, String> {
    let message = message.trim();
    if message.is_empty() {
        return Err("Write a message first".to_string());
    }
    let pool = state.db_manager.pool();
    let config = SettingsRepository::get_model_config(pool)
        .await
        .map_err(|error| format!("Could not load AI settings: {error}"))?
        .ok_or("Choose an AI model in Settings first")?;
    let provider = LLMProvider::from_str(&config.provider)?;
    let standard_api_key = if matches!(
        provider,
        LLMProvider::Ollama | LLMProvider::BuiltInAI | LLMProvider::CustomOpenAI
    ) {
        String::new()
    } else {
        SettingsRepository::get_api_key(pool, &config.provider)
            .await
            .map_err(|error| format!("Could not load the AI provider key: {error}"))?
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("Add an API key for {} in Settings", config.provider))?
    };
    let custom = if provider == LLMProvider::CustomOpenAI {
        SettingsRepository::get_custom_openai_config(pool)
            .await
            .map_err(|error| format!("Could not load custom AI settings: {error}"))?
    } else {
        None
    };
    let api_key = custom
        .as_ref()
        .and_then(|value| value.api_key.clone())
        .unwrap_or(standard_api_key);

    let transcript_rows = sqlx::query_as::<_, (String, String, Option<String>, Option<f64>)>(
        "SELECT id, transcript, speaker, audio_start_time FROM transcripts WHERE meeting_id = ? ORDER BY audio_start_time ASC, timestamp ASC",
    )
    .bind(&meeting_id)
    .fetch_all(pool)
    .await
    .map_err(|error| format!("Could not load the transcript: {error}"))?;
    let transcript_json = serde_json::to_value(
        transcript_rows
            .iter()
            .map(|(id, text, speaker, start)| {
                serde_json::json!({
                    "id": id, "speaker": speaker, "startSeconds": start, "text": text,
                })
            })
            .collect::<Vec<_>>(),
    )
    .map_err(|error| error.to_string())?;
    let raw_notes = sqlx::query_scalar::<_, String>(
        "SELECT notes_markdown FROM meeting_notes WHERE meeting_id = ?",
    )
    .bind(&meeting_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| format!("Could not load raw notes: {error}"))?
    .unwrap_or_default();
    let stored_summary = sqlx::query_scalar::<_, String>(
        "SELECT result FROM summary_processes WHERE meeting_id = ?",
    )
    .bind(&meeting_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| format!("Could not load enhanced notes: {error}"))?
    .unwrap_or_default();
    let enhanced_notes = enhanced_notes_markdown(&stored_summary);
    let history = sqlx::query_as::<_, (String, String)>(
        "SELECT role, content FROM meeting_chat_messages WHERE meeting_id = ? ORDER BY created_at DESC LIMIT 12",
    )
    .bind(&meeting_id)
    .fetch_all(pool)
    .await
    .map_err(|error| format!("Could not load meeting chat history: {error}"))?;

    let context = serde_json::json!({
        "rawNotesMarkdown": raw_notes,
        "enhancedNotesMarkdown": enhanced_notes,
        "transcriptSegments": transcript_json,
        "recentConversationNewestFirst": history,
        "request": message,
    });
    let system_prompt = r#"You are the meeting assistant. Answer the user's question about this meeting using only the material the app supplies: enhancedNotesMarkdown, rawNotesMarkdown, transcriptSegments, and the recent conversation. Ground your answer in the enhanced notes when they exist, and in the transcript otherwise. If enhancedNotesMarkdown is empty, say there are no enhanced notes yet rather than inventing any. If the supplied material does not answer the question, say so plainly instead of guessing. You cannot change anything: never claim to have edited the notes or the transcript, and when the user asks for the notes to change, tell them to use the re-enhance button on the notes panel. Reply in plain Markdown prose, with no JSON envelope and no preamble about these instructions."#;
    let app_data_dir = app.path().app_data_dir().ok();
    let completion = generate_summary(
        &reqwest::Client::new(),
        &provider,
        custom
            .as_ref()
            .map(|value| value.model.as_str())
            .unwrap_or(&config.model),
        &api_key,
        system_prompt,
        &context.to_string(),
        config.ollama_endpoint.as_deref(),
        custom.as_ref().map(|value| value.endpoint.as_str()),
        custom
            .as_ref()
            .and_then(|value| value.max_tokens)
            .map(|value| value as u32),
        custom.as_ref().and_then(|value| value.temperature),
        custom.as_ref().and_then(|value| value.top_p),
        app_data_dir.as_ref(),
        None,
    )
    .await?;
    let reply = assistant_reply(&completion.content)?;

    // Only commit the conversation once the model has answered, avoiding orphaned failed turns.
    save_chat_message(pool, &meeting_id, "user", message).await?;
    save_chat_message(pool, &meeting_id, "assistant", &reply).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_enhanced_notes_from_the_stored_summary_object() {
        assert_eq!(
            enhanced_notes_markdown(r##"{"markdown":"# Notes\n- One","summary_json":[]}"##),
            "# Notes\n- One"
        );
    }

    #[test]
    fn falls_back_to_the_stored_string_when_it_is_not_a_summary_object() {
        assert_eq!(enhanced_notes_markdown("  # Plain notes  "), "# Plain notes");
        assert_eq!(
            enhanced_notes_markdown(r#"{"status":"running"}"#),
            r#"{"status":"running"}"#
        );
        assert_eq!(enhanced_notes_markdown("   "), "");
    }

    #[test]
    fn strips_reasoning_envelopes_from_the_reply() {
        assert_eq!(
            assistant_reply("<think>private</think>\nThe notes cover the budget.").unwrap(),
            "The notes cover the budget."
        );
    }

    #[test]
    fn keeps_the_raw_reply_when_cleaning_leaves_nothing() {
        assert_eq!(
            assistant_reply("  <think>only reasoning</think>  ").unwrap(),
            "<think>only reasoning</think>"
        );
    }

    #[test]
    fn rejects_an_empty_reply() {
        assert!(assistant_reply("   ").is_err());
    }
}
