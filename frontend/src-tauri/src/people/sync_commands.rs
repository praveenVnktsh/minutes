//! Tauri commands for the Linear and Notion people syncs. Tokens come in from
//! settings and are never returned or logged.

use serde::Serialize;
use tauri::{AppHandle, Runtime};

use super::http::ReqwestTransport;
use super::linear::LinearClient;
use super::notion::{NotionClient, NotionDatabase, NotionOverrides};
use super::sync::{self, ConnectionTest, NotionConfig, SourceStatus, SourceSyncResult};
use super::{SOURCE_LINEAR, SOURCE_NOTION};
use crate::state::AppState;

#[derive(Debug, Clone, Serialize)]
pub struct ConnectResult {
    pub test: ConnectionTest,
    pub statuses: Vec<SourceStatus>,
}

async fn test_with(
    source: &str,
    token: &str,
    database_id: Option<&str>,
) -> Result<ConnectionTest, String> {
    let transport = ReqwestTransport::new()?;
    match source {
        SOURCE_LINEAR => LinearClient::new(&transport, token).test_connection().await,
        SOURCE_NOTION => {
            NotionClient::new(&transport, token)
                .test_connection(database_id)
                .await
        }
        other => Err(format!("Unknown people source \"{other}\"")),
    }
}

async fn stored_token(state: &AppState, source: &str) -> Result<String, String> {
    sync::token(state.db_manager.pool(), source)
        .await?
        .ok_or_else(|| format!("{} is not connected", sync::display_name(source)))
}

/// Mirrors the "Linear & Notion people sync" beta flag from the frontend.
/// Turning it off stops all requests but keeps tokens and people.
#[tauri::command]
pub async fn set_people_integrations_enabled(enabled: bool) -> Result<(), String> {
    log::info!("Linear & Notion people sync (beta) enabled: {enabled}");
    sync::set_integrations_enabled(enabled);
    Ok(())
}

fn ensure_enabled() -> Result<(), String> {
    sync::ensure_enabled(sync::integrations_enabled())
}

#[tauri::command]
pub async fn people_sync_status(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<SourceStatus>, String> {
    sync::statuses(state.db_manager.pool()).await
}

/// Tests a key or token and, when it works, stores it.
#[tauri::command]
pub async fn people_connect<R: Runtime>(
    app: AppHandle<R>,
    state: tauri::State<'_, AppState>,
    source: String,
    token: String,
) -> Result<ConnectResult, String> {
    ensure_enabled()?;
    let source = sync::check_source(&source)?;
    let token = token.trim().to_string();
    if token.is_empty() {
        return Err("Paste a key or token first".to_string());
    }
    let pool = state.db_manager.pool();
    let database = if source == SOURCE_NOTION {
        sync::notion_config(pool).await?.database_id
    } else {
        None
    };
    let test = test_with(source, &token, database.as_deref()).await?;
    sync::save_token(pool, source, &token, test.workspace.as_deref()).await?;
    log::info!("People source connected: {source}");
    sync::emit_status(&app, pool).await;
    Ok(ConnectResult {
        test,
        statuses: sync::statuses(pool).await?,
    })
}

/// Tests the given token, or the stored one.
#[tauri::command]
pub async fn people_test_connection(
    state: tauri::State<'_, AppState>,
    source: String,
    token: Option<String>,
) -> Result<ConnectionTest, String> {
    ensure_enabled()?;
    let source = sync::check_source(&source)?;
    let token = match token
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
    {
        Some(token) => token,
        None => stored_token(&state, source).await?,
    };
    let database = if source == SOURCE_NOTION {
        sync::notion_config(state.db_manager.pool())
            .await?
            .database_id
    } else {
        None
    };
    test_with(source, &token, database.as_deref()).await
}

/// Databases the connected Notion integration can see.
#[tauri::command]
pub async fn people_notion_databases(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<NotionDatabase>, String> {
    ensure_enabled()?;
    let token = stored_token(&state, SOURCE_NOTION).await?;
    let transport = ReqwestTransport::new()?;
    NotionClient::new(&transport, &token).list_databases().await
}

/// Saves the Notion database to sync and the property overrides.
#[tauri::command]
pub async fn people_notion_save_mapping<R: Runtime>(
    app: AppHandle<R>,
    state: tauri::State<'_, AppState>,
    database_id: String,
    database_title: Option<String>,
    mapping: NotionOverrides,
) -> Result<Vec<SourceStatus>, String> {
    let database_id = database_id.trim().to_string();
    if database_id.is_empty() {
        return Err("Choose a Notion database".to_string());
    }
    let pool = state.db_manager.pool();
    sync::save_notion_config(
        pool,
        &NotionConfig {
            database_id: Some(database_id),
            database_title,
            mapping,
        },
    )
    .await?;
    sync::emit_status(&app, pool).await;
    sync::statuses(pool).await
}

/// Syncs one source, or every connected one.
#[tauri::command]
pub async fn people_sync_now<R: Runtime>(
    app: AppHandle<R>,
    state: tauri::State<'_, AppState>,
    source: Option<String>,
) -> Result<Vec<SourceSyncResult>, String> {
    ensure_enabled()?;
    let pool = state.db_manager.pool();
    let transport = ReqwestTransport::new()?;
    let results = sync::sync_all(pool, &transport, source.as_deref()).await?;
    sync::emit_status(&app, pool).await;
    Ok(results)
}

/// Forgets a source's token and settings, and the people only it knew.
#[tauri::command]
pub async fn people_disconnect<R: Runtime>(
    app: AppHandle<R>,
    state: tauri::State<'_, AppState>,
    source: String,
) -> Result<u64, String> {
    let pool = state.db_manager.pool();
    let removed = sync::disconnect(pool, &source).await?;
    log::info!("People source disconnected: {source} ({removed} people removed)");
    sync::emit_status(&app, pool).await;
    Ok(removed)
}
