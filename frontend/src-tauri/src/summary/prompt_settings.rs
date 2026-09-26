//! User-editable system prompt for the final meeting-notes call.
//!
//! The override lives in the shared `store.json` under `summaryNotesPrompt`.
//! When the key is missing or blank, summary generation uses the built-in
//! prompt from `processor::default_notes_system_prompt`.

use serde::Serialize;
use tauri::{AppHandle, Runtime};
use tauri_plugin_store::StoreExt;

use crate::summary::processor::default_notes_system_prompt;

const STORE_FILE: &str = "store.json";
const PROMPT_KEY: &str = "summaryNotesPrompt";

/// What the Settings screen needs to show and edit the notes prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryPromptSettings {
    /// The prompt summaries will actually use: the override, or the default.
    pub prompt: String,
    pub default_prompt: String,
    pub is_custom: bool,
}

/// Turn a prompt the user submitted into the value to store.
/// Blank text, or text equal to the default, means "no override".
fn normalize_prompt(proposed: &str, default_prompt: &str) -> Option<String> {
    let trimmed = proposed.trim();
    if trimmed.is_empty() || trimmed == default_prompt.trim() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Keep a stored value only if it is a non-blank string.
fn parse_stored(value: Option<&serde_json::Value>) -> Option<String> {
    value
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn build_settings(custom: Option<String>, default_prompt: String) -> SummaryPromptSettings {
    match custom {
        Some(prompt) => SummaryPromptSettings {
            prompt,
            default_prompt,
            is_custom: true,
        },
        None => SummaryPromptSettings {
            prompt: default_prompt.clone(),
            default_prompt,
            is_custom: false,
        },
    }
}

/// The saved override, or `None` when the user has not customised the prompt.
pub fn load_notes_prompt<R: Runtime>(app: &AppHandle<R>) -> Option<String> {
    let store = match app.store(STORE_FILE) {
        Ok(store) => store,
        Err(e) => {
            log::warn!("summary prompt: could not open {}: {}", STORE_FILE, e);
            return None;
        }
    };
    parse_stored(store.get(PROMPT_KEY).as_ref())
}

fn write_notes_prompt<R: Runtime>(
    app: &AppHandle<R>,
    prompt: Option<String>,
) -> Result<(), String> {
    let store = app
        .store(STORE_FILE)
        .map_err(|e| format!("Failed to open settings store: {}", e))?;
    match prompt {
        Some(prompt) => store.set(PROMPT_KEY, serde_json::Value::String(prompt)),
        None => {
            store.delete(PROMPT_KEY);
        }
    }
    store
        .save()
        .map_err(|e| format!("Failed to save settings store: {}", e))
}

fn current_settings<R: Runtime>(app: &AppHandle<R>) -> SummaryPromptSettings {
    build_settings(load_notes_prompt(app), default_notes_system_prompt())
}

#[tauri::command]
pub async fn api_get_summary_prompt<R: Runtime>(
    app: AppHandle<R>,
) -> Result<SummaryPromptSettings, String> {
    Ok(current_settings(&app))
}

#[tauri::command]
pub async fn api_save_summary_prompt<R: Runtime>(
    app: AppHandle<R>,
    prompt: String,
) -> Result<SummaryPromptSettings, String> {
    let value = normalize_prompt(&prompt, &default_notes_system_prompt());
    log::info!(
        "summary prompt: saving {}",
        if value.is_some() {
            "custom prompt"
        } else {
            "default (clearing override)"
        }
    );
    write_notes_prompt(&app, value)?;
    Ok(current_settings(&app))
}

#[tauri::command]
pub async fn api_reset_summary_prompt<R: Runtime>(
    app: AppHandle<R>,
) -> Result<SummaryPromptSettings, String> {
    log::info!("summary prompt: resetting to default");
    write_notes_prompt(&app, None)?;
    Ok(current_settings(&app))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const DEFAULT: &str = "Default prompt text";

    #[test]
    fn normalize_keeps_custom_prompt_trimmed() {
        assert_eq!(
            normalize_prompt("  My prompt \n", DEFAULT),
            Some("My prompt".to_string())
        );
    }

    #[test]
    fn normalize_clears_blank_prompt() {
        assert_eq!(normalize_prompt("", DEFAULT), None);
        assert_eq!(normalize_prompt(" \n\t ", DEFAULT), None);
    }

    #[test]
    fn normalize_clears_prompt_equal_to_default() {
        assert_eq!(normalize_prompt(DEFAULT, DEFAULT), None);
        assert_eq!(normalize_prompt("\n Default prompt text  ", DEFAULT), None);
    }

    #[test]
    fn normalize_clears_the_real_default() {
        let default_prompt = default_notes_system_prompt();
        assert_eq!(normalize_prompt(&default_prompt, &default_prompt), None);
    }

    #[test]
    fn parse_stored_accepts_non_blank_string() {
        assert_eq!(
            parse_stored(Some(&json!("  Saved prompt "))),
            Some("Saved prompt".to_string())
        );
    }

    #[test]
    fn parse_stored_rejects_missing_blank_and_non_string() {
        assert_eq!(parse_stored(None), None);
        assert_eq!(parse_stored(Some(&json!("   "))), None);
        assert_eq!(parse_stored(Some(&json!(42))), None);
        assert_eq!(parse_stored(Some(&json!(null))), None);
        assert_eq!(parse_stored(Some(&json!({ "prompt": "x" }))), None);
    }

    #[test]
    fn build_settings_with_override_is_custom() {
        let settings = build_settings(Some("Custom".to_string()), DEFAULT.to_string());
        assert_eq!(
            settings,
            SummaryPromptSettings {
                prompt: "Custom".to_string(),
                default_prompt: DEFAULT.to_string(),
                is_custom: true,
            }
        );
    }

    #[test]
    fn build_settings_without_override_uses_default() {
        let settings = build_settings(None, DEFAULT.to_string());
        assert_eq!(settings.prompt, DEFAULT);
        assert_eq!(settings.default_prompt, DEFAULT);
        assert!(!settings.is_custom);
    }

    #[test]
    fn settings_serialize_in_camel_case() {
        let value = serde_json::to_value(build_settings(None, DEFAULT.to_string())).unwrap();
        assert_eq!(
            value,
            json!({ "prompt": DEFAULT, "defaultPrompt": DEFAULT, "isCustom": false })
        );
    }
}
