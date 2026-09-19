// Transcription task queue - processes import/retranscribe jobs sequentially
// Prevents Whisper/Parakeet engine contention by ensuring only one task runs at a time

use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Listener, Runtime};
use tokio::sync::{Mutex, Notify};
use uuid::Uuid;

/// Type of transcription task
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TaskType {
    Import,
    Retranscribe,
}

/// Status of a queued task
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TaskStatus {
    Pending,
    Active,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

/// A transcription task in the queue
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptionTask {
    pub task_id: String,
    pub task_type: TaskType,
    pub title: String,
    pub status: TaskStatus,
    // Import-specific fields
    pub source_path: Option<String>,
    // Retranscribe-specific fields
    pub meeting_id: Option<String>,
    pub meeting_folder_path: Option<String>,
    // Shared fields
    pub language: Option<String>,
    pub model: Option<String>,
    pub provider: Option<String>,
}

/// Progress event emitted by the queue
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueProgressEvent {
    pub task_id: String,
    pub task_type: TaskType,
    pub title: String,
    pub meeting_id: Option<String>,
    pub stage: String,
    pub progress_percentage: u32,
    pub message: String,
    pub is_paused: bool,
    pub queue_position: Option<usize>,
    pub queue_total: Option<usize>,
}

/// Completion event emitted by the queue
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueCompleteEvent {
    pub task_id: String,
    pub task_type: TaskType,
    pub title: String,
    pub meeting_id: String,
    pub segments_count: usize,
    pub duration_seconds: f64,
    pub warning: Option<String>,
}

/// Error event emitted by the queue
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueErrorEvent {
    pub task_id: String,
    pub task_type: TaskType,
    pub title: String,
    pub meeting_id: Option<String>,
    pub status: TaskStatus,
    pub error: String,
}

/// Queue status for the frontend
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueStatus {
    pub tasks: Vec<QueueTaskInfo>,
    pub active_task_id: Option<String>,
    pub pending_count: usize,
}

/// Info about a single task in the queue status
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueTaskInfo {
    pub task_id: String,
    pub task_type: TaskType,
    pub title: String,
    pub meeting_id: Option<String>,
    pub status: TaskStatus,
    pub controls_available: bool,
}

/// Per-task control flags (cancel + pause)
struct TaskFlags {
    cancelled: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    controls_available: Arc<AtomicBool>,
}

static TASK_FLAGS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, TaskFlags>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// Global notify used to wake paused tasks when resume is called
static RESUME_NOTIFY: std::sync::LazyLock<Arc<Notify>> =
    std::sync::LazyLock::new(|| Arc::new(Notify::new()));

fn create_task_flags(task_id: &str) {
    let mut flags = TASK_FLAGS.lock().unwrap_or_else(|e| e.into_inner());
    flags.insert(
        task_id.to_string(),
        TaskFlags {
            cancelled: Arc::new(AtomicBool::new(false)),
            paused: Arc::new(AtomicBool::new(false)),
            controls_available: Arc::new(AtomicBool::new(true)),
        },
    );
}

fn task_flags(task_id: &str) -> Option<(Arc<AtomicBool>, Arc<AtomicBool>, Arc<AtomicBool>)> {
    TASK_FLAGS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(task_id)
        .map(|flags| {
            (
                flags.cancelled.clone(),
                flags.paused.clone(),
                flags.controls_available.clone(),
            )
        })
}

/// Remove flags for a completed/failed task
fn remove_cancel_flag(task_id: &str) {
    let mut flags = TASK_FLAGS.lock().unwrap_or_else(|e| e.into_inner());
    flags.remove(task_id);
}

/// Check if a task is cancelled
pub fn is_task_cancelled(task_id: &str) -> bool {
    let flags = TASK_FLAGS.lock().unwrap_or_else(|e| e.into_inner());
    flags
        .get(task_id)
        .map(|f| f.cancelled.load(Ordering::SeqCst))
        .unwrap_or(false)
}

/// Check if a task is paused
pub fn is_task_paused(task_id: &str) -> bool {
    let flags = TASK_FLAGS.lock().unwrap_or_else(|e| e.into_inner());
    flags
        .get(task_id)
        .map(|f| f.paused.load(Ordering::SeqCst))
        .unwrap_or(false)
}

/// Wait while the task is paused. Returns immediately if not paused.
/// Returns false if the task was cancelled while paused.
pub async fn wait_if_paused(task_id: &str) -> bool {
    loop {
        let notified = RESUME_NOTIFY.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if is_task_cancelled(task_id) {
            return false; // cancelled while paused
        }
        if !is_task_paused(task_id) {
            return true; // not paused, continue
        }
        notified.await;
    }
}

pub fn set_active_task_controls_available(available: bool) {
    let task_id = ACTIVE_TASK_ID
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    if let Some((_, _, controls)) = task_id.as_deref().and_then(task_flags) {
        controls.store(available, Ordering::SeqCst);
    }
}

/// ID of the currently active task (if any). Used by import/retranscription
/// to check pause state between segments.
static ACTIVE_TASK_ID: std::sync::LazyLock<std::sync::Mutex<Option<String>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(None));

fn set_active_task_id(task_id: Option<&str>) {
    let mut id = ACTIVE_TASK_ID.lock().unwrap_or_else(|e| e.into_inner());
    *id = task_id.map(|s| s.to_string());
}

/// Check if the currently active task is paused, and if so, block until resumed.
/// This is meant to be called from import/retranscription segment loops.
/// Returns false if cancelled while paused.
pub async fn check_pause() -> bool {
    let task_id = {
        let id = ACTIVE_TASK_ID.lock().unwrap_or_else(|e| e.into_inner());
        id.clone()
    };
    match task_id {
        Some(id) => wait_if_paused(&id).await,
        None => true,
    }
}

/// The transcription queue manager
pub struct TranscriptionQueue {
    tasks: Arc<Mutex<VecDeque<TranscriptionTask>>>,
    active_task: Arc<Mutex<Option<TranscriptionTask>>>,
    transition: Arc<Mutex<()>>,
    notify: Arc<Notify>,
}

impl TranscriptionQueue {
    pub fn new() -> Self {
        Self {
            tasks: Arc::new(Mutex::new(VecDeque::new())),
            active_task: Arc::new(Mutex::new(None)),
            transition: Arc::new(Mutex::new(())),
            notify: Arc::new(Notify::new()),
        }
    }

    async fn activate_next(&self) -> Option<TranscriptionTask> {
        let _transition = self.transition.lock().await;
        let mut tasks = self.tasks.lock().await;
        let task = tasks.pop_front();
        if let Some(task) = task.as_ref() {
            let mut active = self.active_task.lock().await;
            *active = Some(task.clone());
        }
        task
    }

    /// Add a task to the queue and return its task_id
    pub async fn enqueue<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        mut task: TranscriptionTask,
    ) -> String {
        if task.task_id.is_empty() {
            task.task_id = format!("task-{}", Uuid::new_v4());
        }
        task.status = TaskStatus::Pending;

        create_task_flags(&task.task_id);

        let task_id = task.task_id.clone();
        crate::meeting_activity::register_task(
            app,
            task_id.clone(),
            task.meeting_id.clone(),
            match &task.task_type {
                TaskType::Import => crate::meeting_activity::ActivityKind::Import,
                TaskType::Retranscribe => crate::meeting_activity::ActivityKind::Retranscription,
            },
            task.title.clone(),
        );
        {
            let mut queue = self.tasks.lock().await;
            queue.push_back(task);
        }

        // Wake the worker
        self.notify.notify_one();

        info!("Enqueued transcription task: {}", task_id);
        task_id
    }

    /// Cancel a task by ID (works for both pending and active tasks)
    pub async fn cancel_task(&self, task_id: &str) -> Option<(TranscriptionTask, bool)> {
        let _transition = self.transition.lock().await;
        let mut queue = self.tasks.lock().await;
        if let Some(pos) = queue.iter().position(|task| task.task_id == task_id) {
            let task = queue.remove(pos).expect("position checked");
            remove_cancel_flag(task_id);
            info!("Removed pending task from queue: {}", task_id);
            return Some((task, true));
        }
        drop(queue);

        let active = self.active_task.lock().await;
        if let Some(task) = active.as_ref().filter(|task| task.task_id == task_id) {
            if let Some((cancelled, _, controls)) = task_flags(task_id) {
                if !controls.load(Ordering::SeqCst) {
                    return None;
                }
                cancelled.store(true, Ordering::SeqCst);
                RESUME_NOTIFY.notify_waiters();
                info!("Cancellation flag set for active task: {}", task_id);
                return Some((task.clone(), false));
            }
        }

        warn!("Task not found for cancellation: {}", task_id);
        None
    }

    pub async fn set_paused(&self, task_id: &str, paused: bool) -> bool {
        let active = self.active_task.lock().await;
        if active.as_ref().map(|task| task.task_id.as_str()) != Some(task_id) {
            return false;
        }
        let Some((_, pause_flag, controls)) = task_flags(task_id) else {
            return false;
        };
        if !controls.load(Ordering::SeqCst) {
            return false;
        }
        pause_flag.store(paused, Ordering::SeqCst);
        if !paused {
            RESUME_NOTIFY.notify_waiters();
        }
        true
    }

    /// Get current queue status
    pub async fn get_status(&self) -> QueueStatus {
        let queue = self.tasks.lock().await;
        let active = self.active_task.lock().await;

        let mut tasks: Vec<QueueTaskInfo> = Vec::new();

        if let Some(ref task) = *active {
            let status = if is_task_paused(&task.task_id) {
                TaskStatus::Paused
            } else {
                TaskStatus::Active
            };
            tasks.push(QueueTaskInfo {
                task_id: task.task_id.clone(),
                task_type: task.task_type.clone(),
                title: task.title.clone(),
                meeting_id: task.meeting_id.clone(),
                status,
                controls_available: task_flags(&task.task_id)
                    .map(|(_, _, controls)| controls.load(Ordering::SeqCst))
                    .unwrap_or(false),
            });
        }

        for task in queue.iter() {
            tasks.push(QueueTaskInfo {
                task_id: task.task_id.clone(),
                task_type: task.task_type.clone(),
                title: task.title.clone(),
                meeting_id: task.meeting_id.clone(),
                status: TaskStatus::Pending,
                controls_available: true,
            });
        }

        let pending_count = queue.len();
        let active_task_id = active.as_ref().map(|t| t.task_id.clone());

        QueueStatus {
            tasks,
            active_task_id,
            pending_count,
        }
    }

    /// Check if the queue has an active task
    pub async fn has_active_task(&self) -> bool {
        self.active_task.lock().await.is_some()
    }

    /// Start the queue worker loop
    pub fn start_worker<R: Runtime>(self: Arc<Self>, app: AppHandle<R>) {
        let queue = self.clone();
        tauri::async_runtime::spawn(async move {
            info!("Transcription queue worker started");
            loop {
                // Wait for a notification that a task is available
                queue.notify.notified().await;

                // Process all available tasks
                loop {
                    let task = queue.activate_next().await;

                    let task = match task {
                        Some(t) => t,
                        None => break, // No more tasks
                    };

                    set_active_task_id(Some(&task.task_id));
                    crate::meeting_activity::update_task(
                        &app,
                        &task.task_id,
                        crate::meeting_activity::ActivityStatus::Transcribing,
                        task.meeting_id.clone(),
                        Some("starting".to_string()),
                        None,
                        Some("Starting transcription".to_string()),
                        None,
                    );

                    // Emit queue status update
                    emit_queue_status(&app, &queue).await;

                    // Execute the task
                    info!("Processing task: {} ({})", task.task_id, task.title);
                    match task.task_type {
                        TaskType::Import => {
                            process_import_task(&app, &task).await;
                        }
                        TaskType::Retranscribe => {
                            process_retranscribe_task(&app, &task).await;
                        }
                    }
                    set_active_task_controls_available(false);

                    // Clear active task
                    set_active_task_id(None);
                    {
                        let _transition = queue.transition.lock().await;
                        let mut active = queue.active_task.lock().await;
                        *active = None;
                    }

                    // Clean up cancel flag
                    remove_cancel_flag(&task.task_id);

                    // Emit updated queue status
                    emit_queue_status(&app, &queue).await;
                }
            }
        });
    }
}

/// Emit queue status to the frontend
async fn emit_queue_status<R: Runtime>(app: &AppHandle<R>, queue: &TranscriptionQueue) {
    let status = queue.get_status().await;
    let _ = app.emit("transcription-queue-status", &status);
}

/// Process an import task
async fn process_import_task<R: Runtime>(app: &AppHandle<R>, task: &TranscriptionTask) {
    let source_path = match &task.source_path {
        Some(p) => p.clone(),
        None => {
            emit_error(app, task, "Missing source path for import task");
            return;
        }
    };

    // Set up cancellation bridge: the import module checks IMPORT_CANCELLED,
    // so we need to bridge our per-task flag to it
    use super::import::IMPORT_CANCELLED;
    IMPORT_CANCELLED.store(false, Ordering::SeqCst);

    // Set up a cancellation watcher
    let task_id = task.task_id.clone();
    let cancel_flag = task_flags(&task_id)
        .map(|(cancelled, _, _)| cancelled)
        .expect("active import task must have control flags");
    let cancel_watcher = tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(250)).await;
            if cancel_flag.load(Ordering::SeqCst) {
                IMPORT_CANCELLED.store(true, Ordering::SeqCst);
                break;
            }
        }
    });

    // Bridge import-progress events to queue events
    let app_for_bridge = app.clone();
    let task_clone = task.clone();
    let progress_bridge = app.listen("import-progress", move |event| {
        if let Ok(progress) = serde_json::from_str::<super::import::ImportProgress>(event.payload())
        {
            let _ = app_for_bridge.emit(
                "transcription-queue-progress",
                QueueProgressEvent {
                    task_id: task_clone.task_id.clone(),
                    task_type: task_clone.task_type.clone(),
                    title: task_clone.title.clone(),
                    meeting_id: task_clone.meeting_id.clone(),
                    stage: progress.stage.clone(),
                    progress_percentage: progress.progress_percentage,
                    message: progress.message.clone(),
                    is_paused: is_task_paused(&task_clone.task_id),
                    queue_position: None,
                    queue_total: None,
                },
            );
            crate::meeting_activity::update_task(
                &app_for_bridge,
                &task_clone.task_id,
                if is_task_paused(&task_clone.task_id) {
                    crate::meeting_activity::ActivityStatus::Paused
                } else {
                    crate::meeting_activity::ActivityStatus::Transcribing
                },
                task_clone.meeting_id.clone(),
                Some(progress.stage),
                Some(progress.progress_percentage),
                Some(progress.message),
                None,
            );
        }
    });

    // Run the import
    let result = super::import::run_import_for_queue(
        app.clone(),
        source_path,
        task.title.clone(),
        task.language.clone(),
        task.model.clone(),
        task.provider.clone(),
        task.task_id.clone(),
    )
    .await;

    // Clean up
    cancel_watcher.abort();
    app.unlisten(progress_bridge);

    match result {
        Ok(import_result) => {
            info!(
                "Import task {} completed: {} segments",
                task.task_id, import_result.segments_count
            );

            let _ = app.emit(
                "transcription-queue-complete",
                QueueCompleteEvent {
                    task_id: task.task_id.clone(),
                    task_type: task.task_type.clone(),
                    title: task.title.clone(),
                    meeting_id: import_result.meeting_id.clone(),
                    segments_count: import_result.segments_count,
                    duration_seconds: import_result.duration_seconds,
                    warning: None,
                },
            );
            crate::meeting_activity::update_task(
                app,
                &task.task_id,
                crate::meeting_activity::ActivityStatus::Ready,
                Some(import_result.meeting_id.clone()),
                Some("complete".to_string()),
                Some(100),
                Some("Import complete".to_string()),
                None,
            );

            if let Err(error) =
                crate::webhooks::enqueue_transcription_complete(app, &import_result.meeting_id)
                    .await
            {
                warn!("Failed to enqueue import completion webhook: {}", error);
            }

            // Also emit the original import-complete event for sidebar refresh
            let _ = app.emit(
                "import-complete",
                serde_json::json!({
                    "meeting_id": import_result.meeting_id,
                    "title": import_result.title,
                    "segments_count": import_result.segments_count,
                    "duration_seconds": import_result.duration_seconds
                }),
            );
        }
        Err(e) => {
            let error_msg = e.to_string();
            if error_msg.contains("cancelled") {
                info!("Import task {} was cancelled", task.task_id);
                let _ = app.emit(
                    "transcription-queue-error",
                    QueueErrorEvent {
                        task_id: task.task_id.clone(),
                        task_type: task.task_type.clone(),
                        title: task.title.clone(),
                        meeting_id: task.meeting_id.clone(),
                        status: TaskStatus::Cancelled,
                        error: "Import cancelled".to_string(),
                    },
                );
                crate::meeting_activity::update_task(
                    app,
                    &task.task_id,
                    crate::meeting_activity::ActivityStatus::Cancelled,
                    task.meeting_id.clone(),
                    Some("cancelled".to_string()),
                    None,
                    Some("Import cancelled".to_string()),
                    None,
                );
            } else {
                error!("Import task {} failed: {}", task.task_id, error_msg);
                emit_error(app, task, &error_msg);
            }
        }
    }
}

/// Process a retranscribe task
async fn process_retranscribe_task<R: Runtime>(app: &AppHandle<R>, task: &TranscriptionTask) {
    let meeting_id = match &task.meeting_id {
        Some(id) => id.clone(),
        None => {
            emit_error(app, task, "Missing meeting_id for retranscribe task");
            return;
        }
    };

    let meeting_folder_path = match &task.meeting_folder_path {
        Some(p) => p.clone(),
        None => {
            emit_error(
                app,
                task,
                "Missing meeting_folder_path for retranscribe task",
            );
            return;
        }
    };

    // Bridge cancellation flag
    use super::retranscription::RETRANSCRIPTION_CANCELLED;
    RETRANSCRIPTION_CANCELLED.store(false, Ordering::SeqCst);

    let task_id = task.task_id.clone();
    let cancel_flag = task_flags(&task_id)
        .map(|(cancelled, _, _)| cancelled)
        .expect("active retranscription task must have control flags");
    let cancel_watcher = tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(250)).await;
            if cancel_flag.load(Ordering::SeqCst) {
                RETRANSCRIPTION_CANCELLED.store(true, Ordering::SeqCst);
                break;
            }
        }
    });

    // Bridge retranscription-progress events to queue events
    let app_for_bridge = app.clone();
    let task_clone = task.clone();
    let progress_bridge = app.listen("retranscription-progress", move |event| {
        if let Ok(progress) =
            serde_json::from_str::<super::retranscription::RetranscriptionProgress>(event.payload())
        {
            if progress.meeting_id == task_clone.meeting_id.as_deref().unwrap_or("") {
                if matches!(progress.stage.as_str(), "diarizing" | "diarization_warning") {
                    crate::meeting_activity::set_task_controls_available(
                        &app_for_bridge,
                        &task_clone.task_id,
                        false,
                    );
                }
                let _ = app_for_bridge.emit(
                    "transcription-queue-progress",
                    QueueProgressEvent {
                        task_id: task_clone.task_id.clone(),
                        task_type: task_clone.task_type.clone(),
                        title: task_clone.title.clone(),
                        meeting_id: task_clone.meeting_id.clone(),
                        stage: progress.stage.clone(),
                        progress_percentage: progress.progress_percentage,
                        message: progress.message.clone(),
                        is_paused: is_task_paused(&task_clone.task_id),
                        queue_position: None,
                        queue_total: None,
                    },
                );
                crate::meeting_activity::update_task(
                    &app_for_bridge,
                    &task_clone.task_id,
                    if is_task_paused(&task_clone.task_id) {
                        crate::meeting_activity::ActivityStatus::Paused
                    } else {
                        crate::meeting_activity::ActivityStatus::Transcribing
                    },
                    task_clone.meeting_id.clone(),
                    Some(progress.stage),
                    Some(progress.progress_percentage),
                    Some(progress.message),
                    None,
                );
            }
        }
    });

    // Run retranscription
    let result = super::retranscription::run_retranscription_for_queue(
        app.clone(),
        meeting_id.clone(),
        meeting_folder_path,
        task.language.clone(),
        task.model.clone(),
        task.provider.clone(),
    )
    .await;

    // Clean up
    cancel_watcher.abort();
    app.unlisten(progress_bridge);

    match result {
        Ok(retranscription_result) => {
            info!(
                "Retranscribe task {} completed: {} segments",
                task.task_id, retranscription_result.segments_count
            );

            let _ = app.emit(
                "transcription-queue-complete",
                QueueCompleteEvent {
                    task_id: task.task_id.clone(),
                    task_type: task.task_type.clone(),
                    title: task.title.clone(),
                    meeting_id: retranscription_result.meeting_id.clone(),
                    segments_count: retranscription_result.segments_count,
                    duration_seconds: retranscription_result.duration_seconds,
                    warning: retranscription_result.warning.clone(),
                },
            );
            crate::meeting_activity::update_task(
                app,
                &task.task_id,
                crate::meeting_activity::ActivityStatus::Ready,
                Some(retranscription_result.meeting_id.clone()),
                Some("complete".to_string()),
                Some(100),
                retranscription_result
                    .warning
                    .clone()
                    .or_else(|| Some("Retranscription complete".to_string())),
                None,
            );

            if let Err(error) = crate::webhooks::enqueue_transcription_complete(
                app,
                &retranscription_result.meeting_id,
            )
            .await
            {
                warn!(
                    "Failed to enqueue retranscription completion webhook: {}",
                    error
                );
            }

            // Also emit the original retranscription-complete event
            let _ = app.emit(
                "retranscription-complete",
                serde_json::json!({
                    "meeting_id": retranscription_result.meeting_id,
                    "segments_count": retranscription_result.segments_count,
                    "duration_seconds": retranscription_result.duration_seconds,
                    "language": retranscription_result.language,
                    "warning": retranscription_result.warning
                }),
            );
        }
        Err(e) => {
            let error_msg = e.to_string();
            if error_msg.contains("cancelled") {
                info!("Retranscribe task {} was cancelled", task.task_id);
                let _ = app.emit(
                    "transcription-queue-error",
                    QueueErrorEvent {
                        task_id: task.task_id.clone(),
                        task_type: task.task_type.clone(),
                        title: task.title.clone(),
                        meeting_id: task.meeting_id.clone(),
                        status: TaskStatus::Cancelled,
                        error: "Retranscription cancelled".to_string(),
                    },
                );
                crate::meeting_activity::update_task(
                    app,
                    &task.task_id,
                    crate::meeting_activity::ActivityStatus::Cancelled,
                    task.meeting_id.clone(),
                    Some("cancelled".to_string()),
                    None,
                    Some("Retranscription cancelled".to_string()),
                    None,
                );
            } else {
                error!("Retranscribe task {} failed: {}", task.task_id, error_msg);
                emit_error(app, task, &error_msg);
            }
        }
    }
}

/// Emit an error event for a task
fn emit_error<R: Runtime>(app: &AppHandle<R>, task: &TranscriptionTask, error: &str) {
    let _ = app.emit(
        "transcription-queue-error",
        QueueErrorEvent {
            task_id: task.task_id.clone(),
            task_type: task.task_type.clone(),
            title: task.title.clone(),
            meeting_id: task.meeting_id.clone(),
            status: TaskStatus::Failed,
            error: error.to_string(),
        },
    );
    crate::meeting_activity::update_task(
        app,
        &task.task_id,
        crate::meeting_activity::ActivityStatus::Failed,
        task.meeting_id.clone(),
        Some("failed".to_string()),
        None,
        None,
        Some(error.to_string()),
    );
}

// ============================================================================
// Global queue instance
// ============================================================================

static TRANSCRIPTION_QUEUE: std::sync::LazyLock<Arc<TranscriptionQueue>> =
    std::sync::LazyLock::new(|| Arc::new(TranscriptionQueue::new()));

/// Get the global transcription queue
pub fn get_queue() -> Arc<TranscriptionQueue> {
    TRANSCRIPTION_QUEUE.clone()
}

/// Initialize the queue worker (call once on app startup)
pub fn init_queue_worker<R: Runtime>(app: &AppHandle<R>) {
    let queue = get_queue();
    queue.start_worker(app.clone());
    info!("Transcription queue worker initialized");
}

// ============================================================================
// Tauri commands
// ============================================================================

/// Get the current queue status
#[tauri::command]
pub async fn get_transcription_queue_status() -> QueueStatus {
    get_queue().get_status().await
}

/// Cancel a transcription task by ID
#[tauri::command]
pub async fn cancel_transcription_task<R: Runtime>(
    app: AppHandle<R>,
    task_id: String,
) -> Result<bool, String> {
    let queue = get_queue();
    let cancellation = queue.cancel_task(&task_id).await;
    if let Some((task, true)) = cancellation.as_ref() {
        crate::meeting_activity::update_task(
            &app,
            &task_id,
            crate::meeting_activity::ActivityStatus::Cancelled,
            None,
            Some("cancelled".to_string()),
            None,
            Some("Transcription cancelled".to_string()),
            None,
        );
        let _ = app.emit(
            "transcription-queue-error",
            QueueErrorEvent {
                task_id,
                task_type: task.task_type.clone(),
                title: task.title.clone(),
                meeting_id: task.meeting_id.clone(),
                status: TaskStatus::Cancelled,
                error: "Transcription cancelled".to_string(),
            },
        );
    }
    Ok(cancellation.is_some())
}

/// Pause an active transcription task (pauses between segments)
#[tauri::command]
pub async fn pause_transcription_task<R: Runtime>(
    app: AppHandle<R>,
    task_id: String,
) -> Result<bool, String> {
    if !get_queue().set_paused(&task_id, true).await {
        return Ok(false);
    }
    info!("Pause requested for task: {}", task_id);
    crate::meeting_activity::update_task(
        &app,
        &task_id,
        crate::meeting_activity::ActivityStatus::Paused,
        None,
        Some("paused".to_string()),
        None,
        Some("Transcription paused".to_string()),
        None,
    );
    Ok(true)
}

/// Resume a paused transcription task
#[tauri::command]
pub async fn resume_transcription_task<R: Runtime>(
    app: AppHandle<R>,
    task_id: String,
) -> Result<bool, String> {
    if !get_queue().set_paused(&task_id, false).await {
        return Ok(false);
    }
    info!("Resume requested for task: {}", task_id);
    crate::meeting_activity::update_task(
        &app,
        &task_id,
        crate::meeting_activity::ActivityStatus::Transcribing,
        None,
        Some("transcribing".to_string()),
        None,
        Some("Transcription resumed".to_string()),
        None,
    );
    Ok(true)
}

/// Check if the queue has an active task (used to determine if live transcription should be disabled)
#[tauri::command]
pub async fn is_transcription_queue_active() -> bool {
    get_queue().has_active_task().await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str) -> TranscriptionTask {
        TranscriptionTask {
            task_id: id.to_string(),
            task_type: TaskType::Retranscribe,
            title: "test".to_string(),
            status: TaskStatus::Pending,
            source_path: None,
            meeting_id: Some("meeting-1".to_string()),
            meeting_folder_path: Some("folder".to_string()),
            language: None,
            model: None,
            provider: None,
        }
    }

    #[tokio::test]
    async fn cancelling_a_paused_task_wakes_its_waiter() {
        let id = "paused-cancel-test";
        create_task_flags(id);
        let (cancelled, paused, _) = task_flags(id).unwrap();
        paused.store(true, Ordering::SeqCst);
        let waiter = tokio::spawn(wait_if_paused(id));
        tokio::task::yield_now().await;
        cancelled.store(true, Ordering::SeqCst);
        RESUME_NOTIFY.notify_waiters();

        assert!(
            !tokio::time::timeout(tokio::time::Duration::from_secs(1), waiter)
                .await
                .unwrap()
                .unwrap()
        );
        remove_cancel_flag(id);
    }

    #[tokio::test]
    async fn dequeue_and_cancel_have_no_unowned_transition() {
        let queue = Arc::new(TranscriptionQueue::new());
        let item = task("transition-test");
        create_task_flags(&item.task_id);
        queue.tasks.lock().await.push_back(item);

        let (activated, cancelled) =
            tokio::join!(queue.activate_next(), queue.cancel_task("transition-test"));
        assert!(activated.is_some() || cancelled.is_some());
        assert!(cancelled.is_some() || is_task_cancelled("transition-test"));
        remove_cancel_flag("transition-test");
    }

    #[tokio::test]
    async fn controls_reject_unknown_tasks_without_allocating_flags() {
        let queue = TranscriptionQueue::new();
        assert!(!queue.set_paused("unknown", true).await);
        assert!(queue.cancel_task("unknown").await.is_none());
        assert!(task_flags("unknown").is_none());
    }
}
