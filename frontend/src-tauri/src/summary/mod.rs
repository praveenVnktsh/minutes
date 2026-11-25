/// Summary module - handles all meeting summary generation functionality
///
/// This module contains:
/// - LLM client for communicating with various AI providers (OpenAI, Claude, Groq, Ollama, OpenRouter, CustomOpenAI)
/// - Service layer for the AI notes job, written from the meeting's live notes
///   (`live_enhanced`, `live_outline`)
/// - Markdown cleaning (`markdown`) and meeting titles (`title`)
/// - Tauri commands for frontend integration
use serde::{Deserialize, Serialize};

/// Custom OpenAI-compatible endpoint configuration
/// Stored as JSON in the database and used for connecting to any OpenAI-compatible API server
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomOpenAIConfig {
    /// Base URL of the OpenAI-compatible API endpoint (e.g., "http://localhost:8000/v1")
    pub endpoint: String,
    /// API key for authentication (optional if server doesn't require it)
    #[serde(rename = "apiKey")]
    pub api_key: Option<String>,
    /// Model identifier to use (e.g., "gpt-4", "llama-3-70b", "mistral-7b")
    pub model: String,
    /// Maximum tokens for completion (optional)
    #[serde(rename = "maxTokens")]
    pub max_tokens: Option<i32>,
    /// Temperature parameter (0.0-2.0, optional)
    pub temperature: Option<f32>,
    /// Top-P sampling parameter (0.0-1.0, optional)
    #[serde(rename = "topP")]
    pub top_p: Option<f32>,
}

pub(crate) mod budget;
pub mod commands;
#[cfg(test)]
mod enhanced_eval;
pub(crate) mod live_enhanced;
pub(crate) mod live_outline;
pub mod llm_client;
pub(crate) mod markdown;
pub mod service;
pub mod summary_engine;
pub mod template_commands;
pub mod templates;
pub(crate) mod tidy;
pub(crate) mod title;

// Re-export Tauri commands (with their generated __cmd__ variants)
pub use commands::{
    __cmd__api_cancel_summary, __cmd__api_get_summary, __cmd__api_process_transcript,
    __cmd__api_save_meeting_summary, __tauri_command_name_api_cancel_summary,
    __tauri_command_name_api_get_summary, __tauri_command_name_api_process_transcript,
    __tauri_command_name_api_save_meeting_summary, api_cancel_summary, api_get_summary,
    api_process_transcript, api_save_meeting_summary,
};

// Re-export template commands
pub use template_commands::{
    __cmd__api_list_templates, __tauri_command_name_api_list_templates, api_list_templates,
};

// Re-export commonly used items
pub use llm_client::LLMProvider;
pub use service::SummaryService;
