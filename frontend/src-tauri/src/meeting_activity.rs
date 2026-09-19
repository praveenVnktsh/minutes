use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::{LazyLock, Mutex};
use tauri::{AppHandle, Emitter, Runtime};

const TERMINAL_ACTIVITY_LIMIT: usize = 50;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActivityKind {
    Import,
    Recording,
    Retranscription,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActivityStatus {
    Starting,
    Queued,
    Recording,
    Paused,
    Saving,
    Transcribing,
    Ready,
    Failed,
    Cancelled,
}

impl ActivityStatus {
    fn is_terminal(&self) -> bool {
        matches!(self, Self::Ready | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecordingActivity {
    pub session_id: String,
    pub meeting_id: Option<String>,
    pub status: ActivityStatus,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MeetingActivity {
    pub task_id: String,
    pub meeting_id: Option<String>,
    pub kind: ActivityKind,
    pub status: ActivityStatus,
    pub title: String,
    pub stage: Option<String>,
    pub progress_percentage: Option<u32>,
    pub message: Option<String>,
    pub error: Option<String>,
    pub warning: Option<String>,
    pub controls_available: bool,
    pub revision: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MeetingActivitySnapshot {
    pub revision: u64,
    pub recording: Option<RecordingActivity>,
    pub activities: Vec<MeetingActivity>,
}

#[derive(Default)]
struct ActivityStore {
    revision: u64,
    recording: Option<RecordingActivity>,
    activities: VecDeque<MeetingActivity>,
}

impl ActivityStore {
    fn next_revision(&mut self) -> u64 {
        self.revision = self.revision.saturating_add(1);
        self.revision
    }

    fn snapshot(&self) -> MeetingActivitySnapshot {
        MeetingActivitySnapshot {
            revision: self.revision,
            recording: self.recording.clone(),
            activities: self.activities.iter().cloned().collect(),
        }
    }

    fn task_meeting_id(&self, task_id: &str) -> Option<String> {
        self.activities
            .iter()
            .find(|activity| activity.task_id == task_id)
            .and_then(|activity| activity.meeting_id.clone())
    }

    fn trim_terminal(&mut self) {
        while self
            .activities
            .iter()
            .filter(|activity| activity.status.is_terminal())
            .count()
            > TERMINAL_ACTIVITY_LIMIT
        {
            if let Some(index) = self
                .activities
                .iter()
                .position(|activity| activity.status.is_terminal())
            {
                self.activities.remove(index);
            }
        }
    }

    fn bind_recording(&mut self, session_id: &str, meeting_id: String) -> Result<(), String> {
        let recording = self
            .recording
            .as_ref()
            .ok_or_else(|| "No recording session is active".to_string())?;
        if recording.session_id != session_id {
            return Err("Recording session is stale".to_string());
        }
        if let Some(bound_id) = recording.meeting_id.as_deref() {
            return if bound_id == meeting_id {
                Ok(())
            } else {
                Err("Recording session is already bound to another meeting".to_string())
            };
        }

        self.next_revision();
        self.recording.as_mut().expect("checked above").meeting_id = Some(meeting_id);
        Ok(())
    }

    fn finish_recording(&mut self, session_id: &str, error: Option<String>) -> bool {
        if self
            .recording
            .as_ref()
            .map(|recording| recording.session_id.as_str())
            != Some(session_id)
        {
            return false;
        }
        let recording = self.recording.take().expect("checked above");
        let revision = self.next_revision();
        let terminal_error = error.or(recording.error);
        let failed = terminal_error.is_some();
        self.activities.push_back(MeetingActivity {
            task_id: recording.session_id,
            meeting_id: recording.meeting_id,
            kind: ActivityKind::Recording,
            status: if failed {
                ActivityStatus::Failed
            } else {
                ActivityStatus::Ready
            },
            title: "Recording".to_string(),
            stage: Some(if failed { "failed" } else { "complete" }.to_string()),
            progress_percentage: None,
            message: None,
            error: terminal_error,
            warning: None,
            controls_available: false,
            revision,
        });
        true
    }
}

static STORE: LazyLock<Mutex<ActivityStore>> =
    LazyLock::new(|| Mutex::new(ActivityStore::default()));

fn update<R: Runtime>(app: &AppHandle<R>, mutate: impl FnOnce(&mut ActivityStore)) {
    let snapshot = {
        let mut store = STORE.lock().unwrap_or_else(|error| error.into_inner());
        mutate(&mut store);
        store.trim_terminal();
        store.snapshot()
    };
    if let Err(error) = app.emit("meeting-activity-updated", snapshot) {
        log::warn!("Failed to emit meeting activity update: {error}");
    }
}

pub fn new_recording_session_id() -> String {
    format!("recording-{}", uuid::Uuid::new_v4())
}

pub fn start_recording<R: Runtime>(app: &AppHandle<R>, session_id: String) {
    update(app, |store| {
        store.next_revision();
        store.recording = Some(RecordingActivity {
            session_id: session_id.clone(),
            meeting_id: None,
            status: ActivityStatus::Starting,
            error: None,
        });
    });
}

pub fn set_recording_status<R: Runtime>(
    app: &AppHandle<R>,
    session_id: &str,
    status: ActivityStatus,
) {
    update(app, |store| {
        if store
            .recording
            .as_ref()
            .map(|recording| recording.session_id.as_str())
            == Some(session_id)
        {
            store.next_revision();
            if let Some(recording) = store.recording.as_mut() {
                if recording.status == ActivityStatus::Failed && status != ActivityStatus::Failed {
                    return;
                }
                recording.status = status;
            }
        }
    });
}

pub fn fail_recording<R: Runtime>(app: &AppHandle<R>, session_id: &str, error: String) {
    update(app, |store| {
        if store
            .recording
            .as_ref()
            .map(|recording| recording.session_id.as_str())
            == Some(session_id)
        {
            store.next_revision();
            if let Some(recording) = store.recording.as_mut() {
                recording.status = ActivityStatus::Failed;
                recording.error = Some(error);
            }
        }
    });
}

pub fn finish_recording<R: Runtime>(app: &AppHandle<R>, session_id: &str, error: Option<String>) {
    update(app, |store| {
        store.finish_recording(session_id, error);
    });
}

pub fn recording_identity() -> Option<RecordingActivity> {
    STORE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .recording
        .clone()
}

pub fn task_meeting_id(task_id: &str) -> Option<String> {
    STORE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .task_meeting_id(task_id)
}

#[tauri::command]
pub fn bind_active_recording_meeting<R: Runtime>(
    app: AppHandle<R>,
    session_id: String,
    meeting_id: String,
) -> Result<MeetingActivitySnapshot, String> {
    if meeting_id.trim().is_empty() {
        return Err("Meeting ID cannot be empty".to_string());
    }

    let snapshot = {
        let mut store = STORE.lock().unwrap_or_else(|error| error.into_inner());
        store.bind_recording(&session_id, meeting_id)?;
        store.snapshot()
    };
    let _ = app.emit("meeting-activity-updated", &snapshot);
    Ok(snapshot)
}

pub fn register_task<R: Runtime>(
    app: &AppHandle<R>,
    task_id: String,
    meeting_id: Option<String>,
    kind: ActivityKind,
    title: String,
) {
    update(app, |store| {
        let revision = store.next_revision();
        store.activities.push_back(MeetingActivity {
            task_id,
            meeting_id,
            kind,
            status: ActivityStatus::Queued,
            title,
            stage: None,
            progress_percentage: None,
            message: None,
            error: None,
            warning: None,
            controls_available: true,
            revision,
        });
    });
}

pub fn set_task_controls_available<R: Runtime>(app: &AppHandle<R>, task_id: &str, available: bool) {
    update(app, |store| {
        let revision = store.next_revision();
        if let Some(activity) = store
            .activities
            .iter_mut()
            .find(|activity| activity.task_id == task_id && !activity.status.is_terminal())
        {
            activity.controls_available = available;
            activity.revision = revision;
        }
    });
}

pub fn set_task_warning<R: Runtime>(app: &AppHandle<R>, task_id: &str, warning: Option<String>) {
    update(app, |store| {
        let revision = store.next_revision();
        if let Some(activity) = store
            .activities
            .iter_mut()
            .find(|activity| activity.task_id == task_id)
        {
            activity.warning = warning;
            activity.revision = revision;
        }
    });
}

pub fn update_task<R: Runtime>(
    app: &AppHandle<R>,
    task_id: &str,
    status: ActivityStatus,
    meeting_id: Option<String>,
    stage: Option<String>,
    progress_percentage: Option<u32>,
    message: Option<String>,
    error: Option<String>,
) {
    update(app, |store| {
        let revision = store.next_revision();
        if let Some(activity) = store
            .activities
            .iter_mut()
            .find(|activity| activity.task_id == task_id)
        {
            if activity.status.is_terminal() && !status.is_terminal() {
                return;
            }
            if status.is_terminal() {
                activity.controls_available = false;
            }
            activity.status = status;
            if meeting_id.is_some() {
                activity.meeting_id = meeting_id;
            }
            activity.stage = stage;
            activity.progress_percentage = progress_percentage;
            activity.message = message;
            activity.error = error;
            activity.revision = revision;
        }
    });
}

#[tauri::command]
pub fn get_meeting_activity_snapshot() -> MeetingActivitySnapshot {
    STORE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .snapshot()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_recording_binding_is_rejected() {
        let mut store = ActivityStore::default();
        store.recording = Some(RecordingActivity {
            session_id: "current".to_string(),
            meeting_id: None,
            status: ActivityStatus::Recording,
            error: None,
        });
        assert_eq!(
            store.bind_recording("stale", "meeting-1".to_string()),
            Err("Recording session is stale".to_string())
        );
        assert_eq!(store.recording.unwrap().meeting_id, None);
    }

    #[test]
    fn recording_cannot_be_rebound_to_a_different_meeting() {
        let mut store = ActivityStore::default();
        store.recording = Some(RecordingActivity {
            session_id: "current".to_string(),
            meeting_id: None,
            status: ActivityStatus::Recording,
            error: None,
        });
        store
            .bind_recording("current", "meeting-1".to_string())
            .unwrap();
        assert!(store
            .bind_recording("current", "meeting-2".to_string())
            .is_err());
    }

    #[test]
    fn stale_stop_cannot_clear_a_new_recording() {
        let mut store = ActivityStore::default();
        store.recording = Some(RecordingActivity {
            session_id: "new-session".to_string(),
            meeting_id: Some("meeting-2".to_string()),
            status: ActivityStatus::Recording,
            error: None,
        });

        assert!(!store.finish_recording("old-session", None));
        assert_eq!(store.recording.unwrap().session_id, "new-session");
    }

    #[test]
    fn save_failure_is_retained_as_terminal_recording_activity() {
        let mut store = ActivityStore::default();
        store.recording = Some(RecordingActivity {
            session_id: "session-1".to_string(),
            meeting_id: Some("meeting-1".to_string()),
            status: ActivityStatus::Saving,
            error: None,
        });

        assert!(store.finish_recording("session-1", Some("disk full".to_string())));
        assert!(store.recording.is_none());
        let terminal = store.activities.back().unwrap();
        assert_eq!(terminal.status, ActivityStatus::Failed);
        assert_eq!(terminal.error.as_deref(), Some("disk full"));
    }

    #[test]
    fn task_lookup_uses_the_meeting_bound_after_import_persistence() {
        let mut store = ActivityStore::default();
        store.activities.push_back(MeetingActivity {
            task_id: "import-1".to_string(),
            meeting_id: Some("meeting-created".to_string()),
            kind: ActivityKind::Import,
            status: ActivityStatus::Saving,
            title: "Import".to_string(),
            stage: Some("saving".to_string()),
            progress_percentage: Some(90),
            message: None,
            error: None,
            warning: None,
            controls_available: false,
            revision: 1,
        });

        assert_eq!(
            store.task_meeting_id("import-1").as_deref(),
            Some("meeting-created")
        );
    }

    #[test]
    fn terminal_activity_retention_is_bounded_without_dropping_active_jobs() {
        let mut store = ActivityStore::default();
        for index in 0..=TERMINAL_ACTIVITY_LIMIT {
            store.activities.push_back(MeetingActivity {
                task_id: format!("terminal-{index}"),
                meeting_id: None,
                kind: ActivityKind::Import,
                status: ActivityStatus::Ready,
                title: "Import".to_string(),
                stage: None,
                progress_percentage: Some(100),
                message: None,
                error: None,
                warning: None,
                controls_available: false,
                revision: index as u64,
            });
        }
        store.activities.push_back(MeetingActivity {
            task_id: "active".to_string(),
            meeting_id: Some("meeting-1".to_string()),
            kind: ActivityKind::Retranscription,
            status: ActivityStatus::Transcribing,
            title: "Retranscribe".to_string(),
            stage: None,
            progress_percentage: None,
            message: None,
            error: None,
            warning: None,
            controls_available: true,
            revision: 100,
        });

        store.trim_terminal();
        assert_eq!(
            store
                .activities
                .iter()
                .filter(|activity| activity.status.is_terminal())
                .count(),
            TERMINAL_ACTIVITY_LIMIT
        );
        assert!(store
            .activities
            .iter()
            .any(|activity| activity.task_id == "active"));
    }
}
