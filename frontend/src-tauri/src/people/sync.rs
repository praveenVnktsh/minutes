//! Linear and Notion connections for the people directory: their stored
//! credentials and settings, a sync that feeds [`upsert_from_source`], and a
//! background worker that syncs once a day.
//!
//! Tokens live in the app's local database (`people_integrations`), like the
//! LLM provider API keys in `settings`. They are sent only to their own
//! source's API and are never logged. Syncs send the token and read-only
//! queries; no meeting content ever leaves the Mac.
//!
//! [`upsert_from_source`]: super::upsert_from_source

use std::sync::LazyLock;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};
use tauri::{AppHandle, Emitter, Manager, Runtime};

use super::http::{ReqwestTransport, Transport};
use super::linear::LinearClient;
use super::notion::{NotionClient, NotionOverrides};
use super::{SOURCE_LINEAR, SOURCE_NOTION};
use crate::state::AppState;

/// Sources that sync over the network, in display order.
pub const SYNC_SOURCES: [&str; 2] = [SOURCE_LINEAR, SOURCE_NOTION];

/// Emitted with the new [`SourceStatus`] list after any sync or change.
pub const STATUS_EVENT: &str = "people-sync-updated";

/// How often a connected source syncs on its own.
const SYNC_EVERY_HOURS: i64 = 24;
/// After a failed scheduled sync, wait this long before trying again.
const RETRY_AFTER_HOURS: i64 = 1;
/// The worker waits this long after app start, then checks hourly.
const WORKER_START_DELAY: StdDuration = StdDuration::from_secs(90);
const WORKER_TICK: StdDuration = StdDuration::from_secs(60 * 60);

/// One sync at a time, whether started by the user or the worker.
static SYNC_LOCK: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));

/// What a connection test found.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionTest {
    pub workspace: Option<String>,
    /// Linear: active users. Notion: rows in the chosen database, when one is
    /// chosen.
    pub user_count: Option<usize>,
}

/// Notion's settings: the chosen database and the property overrides.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotionConfig {
    #[serde(default)]
    pub database_id: Option<String>,
    #[serde(default)]
    pub database_title: Option<String>,
    #[serde(default)]
    pub mapping: NotionOverrides,
}

/// A source's connection, as settings shows it. Never carries the token.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceStatus {
    pub source: String,
    pub connected: bool,
    pub workspace: Option<String>,
    pub last_synced_at: Option<String>,
    pub last_attempt_at: Option<String>,
    pub last_error: Option<String>,
    pub last_count: Option<i64>,
    /// Notion only.
    pub notion: Option<NotionConfig>,
}

/// How one source's sync went.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSyncResult {
    pub source: String,
    /// People added or updated.
    pub synced: usize,
    /// People the store refused (e.g. no name).
    pub failed: usize,
    pub error: Option<String>,
}

struct Integration {
    token: String,
    config: String,
    last_synced_at: Option<String>,
    last_attempt_at: Option<String>,
}

impl Integration {
    fn notion(&self) -> NotionConfig {
        serde_json::from_str(&self.config).unwrap_or_default()
    }
}

pub fn check_source(source: &str) -> Result<&'static str, String> {
    SYNC_SOURCES
        .iter()
        .copied()
        .find(|s| *s == source)
        .ok_or_else(|| format!("Unknown people source \"{source}\""))
}

fn now() -> String {
    Utc::now().to_rfc3339()
}

async fn load(pool: &SqlitePool, source: &str) -> Result<Option<Integration>, String> {
    let row = sqlx::query(
        "SELECT token, config, last_synced_at, last_attempt_at FROM people_integrations WHERE source = ?",
    )
    .bind(source)
    .fetch_optional(pool)
    .await
    .map_err(|e| e.to_string())?;
    Ok(row.map(|row| Integration {
        token: row.get("token"),
        config: row.get("config"),
        last_synced_at: row.get("last_synced_at"),
        last_attempt_at: row.get("last_attempt_at"),
    }))
}

/// The stored token for a source, if connected.
pub async fn token(pool: &SqlitePool, source: &str) -> Result<Option<String>, String> {
    Ok(load(pool, source).await?.map(|i| i.token))
}

/// The stored Notion settings (default when not connected).
pub async fn notion_config(pool: &SqlitePool) -> Result<NotionConfig, String> {
    Ok(load(pool, SOURCE_NOTION)
        .await?
        .map(|i| i.notion())
        .unwrap_or_default())
}

/// Stores a token (and the workspace it belongs to). Reconnecting keeps the
/// source's other settings.
pub async fn save_token(
    pool: &SqlitePool,
    source: &str,
    token: &str,
    workspace: Option<&str>,
) -> Result<(), String> {
    let source = check_source(source)?;
    let token = token.trim();
    if token.is_empty() {
        return Err("Paste a key or token first".to_string());
    }
    let stamp = now();
    sqlx::query(
        "INSERT INTO people_integrations (source, token, workspace, created_at, updated_at) VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(source) DO UPDATE SET token = excluded.token, workspace = excluded.workspace,
           last_error = NULL, updated_at = excluded.updated_at",
    )
    .bind(source)
    .bind(token)
    .bind(workspace)
    .bind(&stamp)
    .bind(&stamp)
    .execute(pool)
    .await
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Saves the chosen Notion database and property overrides.
pub async fn save_notion_config(pool: &SqlitePool, config: &NotionConfig) -> Result<(), String> {
    let config_json = serde_json::to_string(config).map_err(|e| e.to_string())?;
    let result =
        sqlx::query("UPDATE people_integrations SET config = ?, updated_at = ? WHERE source = ?")
            .bind(config_json)
            .bind(now())
            .bind(SOURCE_NOTION)
            .execute(pool)
            .await
            .map_err(|e| e.to_string())?;
    if result.rows_affected() == 0 {
        return Err("Connect Notion first".to_string());
    }
    Ok(())
}

/// Forgets a source: its token, settings and every link to its people.
/// Returns how many people were removed with it.
pub async fn disconnect(pool: &SqlitePool, source: &str) -> Result<u64, String> {
    let source = check_source(source)?;
    sqlx::query("DELETE FROM people_integrations WHERE source = ?")
        .bind(source)
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?;
    super::forget_source(pool, source).await
}

/// Every sync source's status, connected or not.
pub async fn statuses(pool: &SqlitePool) -> Result<Vec<SourceStatus>, String> {
    let mut out = Vec::new();
    for source in SYNC_SOURCES {
        let row = sqlx::query(
            "SELECT config, workspace, last_synced_at, last_attempt_at, last_error, last_count
             FROM people_integrations WHERE source = ?",
        )
        .bind(source)
        .fetch_optional(pool)
        .await
        .map_err(|e| e.to_string())?;
        out.push(match row {
            Some(row) => SourceStatus {
                source: source.to_string(),
                connected: true,
                workspace: row.get("workspace"),
                last_synced_at: row.get("last_synced_at"),
                last_attempt_at: row.get("last_attempt_at"),
                last_error: row.get("last_error"),
                last_count: row.get("last_count"),
                notion: (source == SOURCE_NOTION).then(|| {
                    serde_json::from_str(&row.get::<String, _>("config")).unwrap_or_default()
                }),
            },
            None => SourceStatus {
                source: source.to_string(),
                ..Default::default()
            },
        });
    }
    Ok(out)
}

async fn record(pool: &SqlitePool, result: &SourceSyncResult) -> Result<(), String> {
    let stamp = now();
    match &result.error {
        None => sqlx::query(
            "UPDATE people_integrations SET last_synced_at = ?, last_attempt_at = ?, last_error = NULL, last_count = ? WHERE source = ?",
        )
        .bind(&stamp)
        .bind(&stamp)
        .bind(result.synced as i64)
        .bind(&result.source),
        Some(error) => sqlx::query(
            "UPDATE people_integrations SET last_attempt_at = ?, last_error = ? WHERE source = ?",
        )
        .bind(&stamp)
        .bind(error)
        .bind(&result.source),
    }
    .execute(pool)
    .await
    .map_err(|e| e.to_string())?;
    Ok(())
}

async fn fetch<T: Transport>(
    transport: &T,
    source: &str,
    integration: &Integration,
) -> Result<Vec<super::PersonInput>, String> {
    match source {
        SOURCE_LINEAR => {
            LinearClient::new(transport, &integration.token)
                .fetch_people()
                .await
        }
        SOURCE_NOTION => {
            let config = integration.notion();
            let database = config
                .database_id
                .as_deref()
                .ok_or_else(|| "Choose a Notion database to sync people from".to_string())?;
            NotionClient::new(transport, &integration.token)
                .fetch_people(database, &config.mapping)
                .await
        }
        other => Err(format!("Unknown people source \"{other}\"")),
    }
}

/// Syncs one source into the people store and records the outcome. Network
/// and API failures land in the result, never as an `Err`.
pub async fn sync_source<T: Transport>(
    pool: &SqlitePool,
    transport: &T,
    source: &str,
) -> SourceSyncResult {
    let _guard = SYNC_LOCK.lock().await;
    sync_source_locked(pool, transport, source).await
}

async fn sync_source_locked<T: Transport>(
    pool: &SqlitePool,
    transport: &T,
    source: &str,
) -> SourceSyncResult {
    let mut result = SourceSyncResult {
        source: source.to_string(),
        ..Default::default()
    };
    let integration = match load(pool, source).await {
        Ok(Some(integration)) => integration,
        Ok(None) => {
            result.error = Some(format!("{} is not connected", display_name(source)));
            return result;
        }
        Err(error) => {
            result.error = Some(error);
            return result;
        }
    };
    match fetch(transport, source, &integration).await {
        Ok(people) => {
            for person in &people {
                match super::upsert_from_source(pool, person).await {
                    Ok(_) => result.synced += 1,
                    Err(error) => {
                        result.failed += 1;
                        log::warn!("People sync ({source}): skipped one person: {error}");
                    }
                }
            }
            log::info!(
                "People sync ({source}): {} synced, {} skipped",
                result.synced,
                result.failed
            );
        }
        Err(error) => {
            log::warn!("People sync ({source}) failed: {error}");
            result.error = Some(error);
        }
    }
    if let Err(error) = record(pool, &result).await {
        log::warn!("Could not record the people sync result: {error}");
    }
    result
}

/// Syncs `only` that source, or every connected source.
pub async fn sync_all<T: Transport>(
    pool: &SqlitePool,
    transport: &T,
    only: Option<&str>,
) -> Result<Vec<SourceSyncResult>, String> {
    let sources: Vec<&str> = match only {
        Some(source) => vec![check_source(source)?],
        None => {
            let mut connected = Vec::new();
            for source in SYNC_SOURCES {
                if load(pool, source).await?.is_some() {
                    connected.push(source);
                }
            }
            connected
        }
    };
    let _guard = SYNC_LOCK.lock().await;
    let mut results = Vec::new();
    for source in sources {
        results.push(sync_source_locked(pool, transport, source).await);
    }
    Ok(results)
}

pub fn display_name(source: &str) -> &str {
    match source {
        SOURCE_LINEAR => "Linear",
        SOURCE_NOTION => "Notion",
        other => other,
    }
}

fn parse(stamp: Option<&str>) -> Option<DateTime<Utc>> {
    stamp
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc))
}

/// Whether the worker should sync a source now: no success in the last day,
/// and no attempt in the last hour (so a failure isn't retried every tick).
pub fn is_due(
    last_synced_at: Option<&str>,
    last_attempt_at: Option<&str>,
    now: DateTime<Utc>,
) -> bool {
    let stale = parse(last_synced_at)
        .map(|at| now - at >= Duration::hours(SYNC_EVERY_HOURS))
        .unwrap_or(true);
    let resting = parse(last_attempt_at)
        .map(|at| now - at < Duration::hours(RETRY_AFTER_HOURS))
        .unwrap_or(false);
    stale && !resting
}

/// Emits the current statuses so open settings pages refresh.
pub async fn emit_status<R: Runtime>(app: &AppHandle<R>, pool: &SqlitePool) {
    match statuses(pool).await {
        Ok(statuses) => {
            let _ = app.emit(STATUS_EVENT, &statuses);
        }
        Err(error) => log::warn!("Could not read people sync status: {error}"),
    }
}

/// Syncs each connected source once a day, starting shortly after launch.
/// Failures are recorded on the source and never stop the worker.
pub fn init_worker<R: Runtime>(app: &AppHandle<R>) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(WORKER_START_DELAY).await;
        let mut interval = tokio::time::interval(WORKER_TICK);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            let Some(state) = app.try_state::<AppState>() else {
                continue;
            };
            let pool = state.db_manager.pool().clone();
            let mut due = Vec::new();
            for source in SYNC_SOURCES {
                match load(&pool, source).await {
                    Ok(Some(integration))
                        if is_due(
                            integration.last_synced_at.as_deref(),
                            integration.last_attempt_at.as_deref(),
                            Utc::now(),
                        ) =>
                    {
                        due.push(source)
                    }
                    Ok(_) => {}
                    Err(error) => log::warn!("People sync worker: {error}"),
                }
            }
            if due.is_empty() {
                continue;
            }
            let transport = match ReqwestTransport::new() {
                Ok(transport) => transport,
                Err(error) => {
                    log::warn!("People sync worker: {error}");
                    continue;
                }
            };
            for source in due {
                sync_source(&pool, &transport, source).await;
            }
            emit_status(&app, &pool).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::people::http::fake::FakeTransport;
    use crate::people::http::HttpError;
    use crate::people::{list_people, update_person, PersonEdit};
    use serde_json::json;

    async fn pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query("PRAGMA foreign_keys = ON")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    fn linear_responses(users: serde_json::Value) -> Vec<serde_json::Value> {
        vec![
            json!({ "data": { "users": { "nodes": users, "pageInfo": { "hasNextPage": false, "endCursor": null } } } }),
            json!({ "data": { "projects": { "nodes": [], "pageInfo": { "hasNextPage": false, "endCursor": null } } } }),
        ]
    }

    #[tokio::test]
    async fn linear_sync_upserts_people_and_records_status() {
        let pool = pool().await;
        save_token(&pool, "linear", "  lin_api_key ", Some("Acme"))
            .await
            .unwrap();
        assert_eq!(
            token(&pool, "linear").await.unwrap().as_deref(),
            Some("lin_api_key")
        );

        let transport = FakeTransport::ok(linear_responses(json!([
            { "id": "u1", "name": "Priya Shah", "displayName": "priya", "email": "priya@example.com", "active": true,
              "teams": { "nodes": [{ "name": "Growth" }] }, "assignedIssues": { "nodes": [] } },
            { "id": "u2", "name": "  ", "email": null, "active": true }
        ])));
        let results = sync_all(&pool, &transport, None).await.unwrap();
        assert_eq!(
            results,
            vec![SourceSyncResult {
                source: "linear".into(),
                synced: 1,
                failed: 1,
                error: None
            }]
        );
        let people = list_people(&pool).await.unwrap();
        assert_eq!(people.len(), 1);
        assert_eq!(people[0].team.as_deref(), Some("Growth"));
        assert_eq!(people[0].aliases, vec!["priya".to_string()]);

        let status = statuses(&pool).await.unwrap();
        assert!(status[0].connected);
        assert_eq!(status[0].workspace.as_deref(), Some("Acme"));
        assert_eq!(status[0].last_count, Some(1));
        assert!(status[0].last_synced_at.is_some());
        assert_eq!(status[0].last_error, None);
        assert!(!status[1].connected, "Notion is not connected");
        let serialized = serde_json::to_string(&status).unwrap();
        assert!(
            !serialized.contains("lin_api_key"),
            "status never carries the token"
        );
    }

    #[tokio::test]
    async fn a_failed_sync_records_the_error_and_keeps_people() {
        let pool = pool().await;
        save_token(&pool, "linear", "k", None).await.unwrap();
        let ok = FakeTransport::ok(linear_responses(json!([
            { "id": "u1", "name": "Priya Shah", "email": "priya@example.com", "active": true }
        ])));
        sync_source(&pool, &ok, "linear").await;
        let first = statuses(&pool).await.unwrap()[0].last_synced_at.clone();

        let offline = FakeTransport::new(vec![Err(HttpError {
            status: None,
            message: "could not connect".into(),
        })]);
        let result = sync_source(&pool, &offline, "linear").await;
        assert_eq!(
            result.error.as_deref(),
            Some("Could not reach Linear: could not connect")
        );
        let status = &statuses(&pool).await.unwrap()[0];
        assert_eq!(status.last_error, result.error);
        assert_eq!(
            status.last_synced_at, first,
            "a failure keeps the last success"
        );
        assert_eq!(list_people(&pool).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn sync_respects_manual_edits() {
        let pool = pool().await;
        save_token(&pool, "linear", "k", None).await.unwrap();
        let user = json!([{ "id": "u1", "name": "Priya Shah", "email": "priya@example.com", "active": true,
                            "teams": { "nodes": [{ "name": "Growth" }] } }]);
        sync_source(
            &pool,
            &FakeTransport::ok(linear_responses(user.clone())),
            "linear",
        )
        .await;
        let id = list_people(&pool).await.unwrap()[0].id.clone();
        update_person(
            &pool,
            &id,
            &PersonEdit {
                team: Some("Design".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        sync_source(&pool, &FakeTransport::ok(linear_responses(user)), "linear").await;
        assert_eq!(
            list_people(&pool).await.unwrap()[0].team.as_deref(),
            Some("Design")
        );
    }

    #[tokio::test]
    async fn notion_needs_a_database_then_syncs_it() {
        let pool = pool().await;
        assert!(save_notion_config(&pool, &NotionConfig::default())
            .await
            .is_err());
        save_token(&pool, "notion", "secret_x", Some("Acme"))
            .await
            .unwrap();

        let result = sync_source(&pool, &FakeTransport::default(), "notion").await;
        assert!(result.error.unwrap().contains("Choose a Notion database"));

        save_notion_config(
            &pool,
            &NotionConfig {
                database_id: Some("db01".into()),
                database_title: Some("Team".into()),
                mapping: NotionOverrides::default(),
            },
        )
        .await
        .unwrap();
        // Reconnecting keeps the chosen database.
        save_token(&pool, "notion", "secret_y", Some("Acme"))
            .await
            .unwrap();
        assert_eq!(
            notion_config(&pool).await.unwrap().database_id.as_deref(),
            Some("db01")
        );

        let transport = FakeTransport::ok(vec![
            json!({ "object": "database", "id": "db01", "title": [], "properties": {
                "Name": { "name": "Name", "type": "title" },
                "Email": { "name": "Email", "type": "email" } } }),
            json!({ "results": [
                { "id": "aa01", "properties": {
                    "Name": { "type": "title", "title": [{ "plain_text": "Dev Patel" }] },
                    "Email": { "type": "email", "email": "dev@example.com" } } }
            ], "has_more": false, "next_cursor": null }),
        ]);
        let result = sync_source(&pool, &transport, "notion").await;
        assert_eq!(result.error, None);
        assert_eq!(result.synced, 1);
        let status = &statuses(&pool).await.unwrap()[1];
        assert_eq!(
            status.notion.as_ref().unwrap().database_title.as_deref(),
            Some("Team")
        );
    }

    #[tokio::test]
    async fn disconnect_forgets_the_token_and_the_people() {
        let pool = pool().await;
        save_token(&pool, "linear", "k", None).await.unwrap();
        let users = json!([{ "id": "u1", "name": "Priya Shah", "email": "priya@example.com", "active": true }]);
        sync_source(&pool, &FakeTransport::ok(linear_responses(users)), "linear").await;
        assert_eq!(disconnect(&pool, "linear").await.unwrap(), 1);
        assert_eq!(token(&pool, "linear").await.unwrap(), None);
        assert!(list_people(&pool).await.unwrap().is_empty());
        assert!(!statuses(&pool).await.unwrap()[0].connected);
    }

    #[tokio::test]
    async fn rejects_unknown_sources_and_blank_tokens() {
        let pool = pool().await;
        assert!(save_token(&pool, "calendar", "k", None).await.is_err());
        assert!(save_token(&pool, "linear", "   ", None).await.is_err());
        assert!(sync_all(&pool, &FakeTransport::default(), Some("jira"))
            .await
            .is_err());
        let result = sync_source(&pool, &FakeTransport::default(), "linear").await;
        assert_eq!(result.error.as_deref(), Some("Linear is not connected"));
        assert!(sync_all(&pool, &FakeTransport::default(), None)
            .await
            .unwrap()
            .is_empty());
    }

    #[test]
    fn due_once_a_day_with_an_hour_between_failed_attempts() {
        let now = Utc::now();
        let ago = |hours: i64| Some((now - Duration::hours(hours)).to_rfc3339());
        assert!(is_due(None, None, now));
        assert!(!is_due(ago(2).as_deref(), ago(2).as_deref(), now));
        assert!(is_due(ago(25).as_deref(), ago(25).as_deref(), now));
        // Failed half an hour ago: wait.
        let half = Some((now - Duration::minutes(30)).to_rfc3339());
        assert!(!is_due(ago(25).as_deref(), half.as_deref(), now));
        assert!(!is_due(None, half.as_deref(), now));
        assert!(is_due(None, ago(2).as_deref(), now));
    }
}
