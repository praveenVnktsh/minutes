use log::{info, warn};
use tauri::{AppHandle, Emitter, Manager};

use super::manager::DatabaseManager;
use crate::state::AppState;

/// Initialize database on app startup
/// Handles first launch detection and conditional initialization
pub async fn initialize_database_on_startup(app: &AppHandle) -> Result<(), String> {
    // Check if this is the first launch (no database exists yet)
    let is_first_launch = DatabaseManager::is_first_launch(app)
        .await
        .map_err(|e| format!("Failed to check first launch status: {}", e))?;

    if is_first_launch {
        info!("First launch detected - will notify window when ready");

        // Delay event emission to ensure window is ready and React listeners are registered
        let app_handle = app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            match app_handle.emit("first-launch-detected", ()) {
                Ok(()) => info!("Emitted first-launch-detected after delay"),
                Err(e) => warn!("Failed to emit first-launch-detected event: {}", e),
            }
        });
    } else {
        // Normal flow - initialize database immediately
        let db_manager = DatabaseManager::new_from_app_handle(app)
            .await
            .map_err(|e| format!("Failed to initialize database manager: {}", e))?;

        // Before AppState is managed, so no command can read a stale row.
        recover_interrupted_summaries(db_manager.pool()).await;

        app.manage(AppState { db_manager });
        info!("Database initialized successfully");
    }

    Ok(())
}

/// Summary jobs live in memory, so a summary still pending at startup died with
/// the previous run (quit or crash mid-generation) and would otherwise show
/// "Generating summary" forever. Fail it, restoring any previous summary, so
/// the meeting offers a retry. An interrupted auto-summary is not retried:
/// doing it on every launch could repeat whatever killed the app.
async fn recover_interrupted_summaries(pool: &sqlx::SqlitePool) {
    use super::repositories::summary::SummaryProcessesRepository;
    use crate::summary::service::INTERRUPTED_SUMMARY_ERROR;

    match SummaryProcessesRepository::fail_all_pending(pool, INTERRUPTED_SUMMARY_ERROR).await {
        Ok(0) => {}
        Ok(count) => warn!(
            "Marked {} summary job(s) interrupted by the last shutdown as failed",
            count
        ),
        Err(e) => log::error!("Failed to recover interrupted summary jobs: {}", e),
    }
}
