use anyhow::Result;
use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Runtime};
use tokio::sync::mpsc;
use tokio::sync::Mutex as AsyncMutex;

use super::audio_processing::create_meeting_folder;
use super::incremental_saver::IncrementalAudioSaver;
use super::recording_state::AudioChunk;

/// Structured transcript segment for JSON export
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptSegment {
    pub id: String,
    pub text: String,
    pub audio_start_time: f64, // Seconds from recording start
    pub audio_end_time: f64,   // Seconds from recording start
    pub duration: f64,         // Segment duration in seconds
    pub display_time: String,  // Formatted time for display like "[02:15]"
    pub confidence: f32,
    pub sequence_id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
}

/// Meeting metadata structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeetingMetadata {
    pub version: String,
    pub meeting_id: Option<String>,
    pub meeting_name: Option<String>,
    pub created_at: String,
    pub completed_at: Option<String>,
    pub duration_seconds: Option<f64>,
    pub devices: DeviceInfo,
    pub audio_file: String,
    pub transcript_file: String,
    pub sample_rate: u32,
    pub status: String, // "recording", "completed", "error"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub microphone: Option<String>,
    pub system_audio: Option<String>,
}

/// An already-saved meeting that a new recording session continues into.
///
/// When set, the saver reopens `meeting_folder` instead of creating a new one,
/// so the new audio and transcripts are appended to the existing meeting.
#[derive(Debug, Clone)]
pub struct ResumeTarget {
    pub meeting_folder: PathBuf,   // existing meeting folder on disk
    pub audio_offset_seconds: f64, // length of audio already recorded for this meeting
}

/// New recording saver using incremental saving strategy
pub struct RecordingSaver {
    incremental_saver: Option<Arc<AsyncMutex<IncrementalAudioSaver>>>,
    meeting_folder: Option<PathBuf>,
    meeting_name: Option<String>,
    metadata: Option<MeetingMetadata>,
    transcript_segments: Arc<Mutex<Vec<TranscriptSegment>>>,
    is_saving: Arc<Mutex<bool>>,
    resume_target: Option<ResumeTarget>,
    // Segments already in transcripts.json of a resumed meeting. Written ahead of
    // the new ones but never returned to callers (they are already in the DB).
    prior_segments: Vec<TranscriptSegment>,
    // Added to every new segment's sequence_id so it cannot collide with prior ones.
    sequence_offset: u64,
}

impl RecordingSaver {
    pub fn new() -> Self {
        Self {
            incremental_saver: None,
            meeting_folder: None,
            meeting_name: None,
            metadata: None,
            transcript_segments: Arc::new(Mutex::new(Vec::new())),
            is_saving: Arc::new(Mutex::new(false)),
            resume_target: None,
            prior_segments: Vec::new(),
            sequence_offset: 0,
        }
    }

    /// Continue an existing meeting instead of starting a new one.
    /// Must be called before `start_accumulation`.
    pub fn set_resume_target(&mut self, target: Option<ResumeTarget>) {
        self.resume_target = target;
    }

    /// Set the meeting name for this recording session
    pub fn set_meeting_name(&mut self, name: Option<String>) {
        self.meeting_name = name;
    }

    /// Set device information in metadata
    pub fn set_device_info(&mut self, mic_name: Option<String>, sys_name: Option<String>) {
        if let Some(ref mut metadata) = self.metadata {
            metadata.devices.microphone = mic_name;
            metadata.devices.system_audio = sys_name;

            // Write updated metadata to disk if folder exists
            if let Some(folder) = &self.meeting_folder {
                let metadata_clone = metadata.clone();
                if let Err(e) = self.write_metadata(folder, &metadata_clone) {
                    warn!("Failed to update metadata with device info: {}", e);
                }
            }
        }
    }

    /// Add or update a structured transcript segment (upserts based on sequence_id)
    /// Also saves incrementally to disk
    pub fn add_transcript_segment(&self, mut segment: TranscriptSegment) {
        // Resumed meeting: move the new session's numbering past the prior segments.
        // Audio times are already shifted by the transcription worker.
        if self.resume_target.is_some() {
            segment.sequence_id += self.sequence_offset;
            segment.id = format!("seg_{}", segment.sequence_id);
        }

        if let Ok(mut segments) = self.transcript_segments.lock() {
            // Check if segment with same sequence_id exists (update it)
            if let Some(existing) = segments
                .iter_mut()
                .find(|s| s.sequence_id == segment.sequence_id)
            {
                *existing = segment.clone();
                info!(
                    "Updated transcript segment {} (seq: {}) - total segments: {}",
                    segment.id,
                    segment.sequence_id,
                    segments.len()
                );
            } else {
                // New segment, add it
                segments.push(segment.clone());
                info!(
                    "Added new transcript segment {} (seq: {}) - total segments: {}",
                    segment.id,
                    segment.sequence_id,
                    segments.len()
                );
            }
        } else {
            error!(
                "Failed to lock transcript segments for adding segment {}",
                segment.id
            );
        }

        // NEW: Save incrementally to disk
        if let Some(folder) = &self.meeting_folder {
            if let Err(e) = self.write_transcripts_json(folder) {
                warn!("Failed to write incremental transcript update: {}", e);
            }
        }
    }

    /// Legacy method for backward compatibility - converts text to basic segment
    pub fn add_transcript_chunk(&self, text: String) {
        let segment = TranscriptSegment {
            id: format!("seg_{}", chrono::Utc::now().timestamp_millis()),
            text,
            audio_start_time: 0.0,
            audio_end_time: 0.0,
            duration: 0.0,
            display_time: "[00:00]".to_string(),
            confidence: 1.0,
            sequence_id: 0,
            speaker: None,
        };
        self.add_transcript_segment(segment);
    }

    /// Start accumulation with optional incremental saving
    ///
    /// # Arguments
    /// * `auto_save` - If true, creates checkpoints and enables saving. If false, audio chunks are discarded.
    pub fn start_accumulation(
        &mut self,
        auto_save: bool,
        mut receiver: mpsc::UnboundedReceiver<AudioChunk>,
    ) {
        if auto_save {
            info!("Initializing incremental audio saver for recording (auto-save ENABLED)");
        } else {
            info!(
                "Starting recording without audio saving (auto-save DISABLED - transcripts only)"
            );
        }

        // Initialize meeting folder and incremental saver ONLY if auto_save is enabled
        if let Some(target) = self.resume_target.clone() {
            match self.reopen_meeting_folder(&target, auto_save) {
                Ok(()) => info!(
                    "Reopened meeting folder to resume recording: {}",
                    target.meeting_folder.display()
                ),
                Err(e) => error!(
                    "Failed to reopen meeting folder {}: {}",
                    target.meeting_folder.display(),
                    e
                ),
            }
        } else if auto_save {
            if let Some(name) = self.meeting_name.clone() {
                match self.initialize_meeting_folder(&name, true) {
                    Ok(()) => info!("Successfully initialized meeting folder with checkpoints"),
                    Err(e) => {
                        error!("Failed to initialize meeting folder: {}", e);
                        // Continue anyway - will use fallback flat structure
                    }
                }
            }
        } else {
            // When auto_save is false, still create meeting folder for transcripts/metadata
            // but skip .checkpoints directory
            if let Some(name) = self.meeting_name.clone() {
                match self.initialize_meeting_folder(&name, false) {
                    Ok(()) => info!("Successfully initialized meeting folder (transcripts only)"),
                    Err(e) => {
                        error!("Failed to initialize meeting folder: {}", e);
                    }
                }
            }
        }

        // Start accumulation task
        let is_saving_clone = self.is_saving.clone();
        let incremental_saver_arc = self.incremental_saver.clone();
        let save_audio = auto_save;

        tokio::spawn(async move {
            info!(
                "Recording saver accumulation task started (save_audio: {})",
                save_audio
            );

            while let Some(chunk) = receiver.recv().await {
                // Check if we should continue
                let should_continue = if let Ok(is_saving) = is_saving_clone.lock() {
                    *is_saving
                } else {
                    false
                };

                if !should_continue {
                    break;
                }

                // Only process audio chunks if auto_save is enabled
                if save_audio {
                    // Add chunk to incremental saver
                    if let Some(saver_arc) = &incremental_saver_arc {
                        let mut saver_guard = saver_arc.lock().await;
                        if let Err(e) = saver_guard.add_chunk(chunk) {
                            error!("Failed to add chunk to incremental saver: {}", e);
                        }
                    } else {
                        error!("Incremental saver not available while accumulating");
                    }
                } else {
                    // auto_save is false: discard audio chunk (no-op)
                    // Transcription already happened in the pipeline before this point
                }
            }

            info!("Recording saver accumulation task ended");
        });

        // Set saving flag
        if let Ok(mut is_saving) = self.is_saving.lock() {
            *is_saving = true;
        }
    }

    /// Initialize meeting folder structure and metadata
    ///
    /// # Arguments
    /// * `meeting_name` - Name of the meeting
    /// * `create_checkpoints` - Whether to create .checkpoints/ directory and IncrementalAudioSaver
    fn initialize_meeting_folder(
        &mut self,
        meeting_name: &str,
        create_checkpoints: bool,
    ) -> Result<()> {
        // Load preferences to get base recordings folder
        let base_folder = super::recording_preferences::get_default_recordings_folder();

        // Create meeting folder structure (with or without .checkpoints/ subdirectory)
        let meeting_folder = create_meeting_folder(&base_folder, meeting_name, create_checkpoints)?;

        // Only initialize incremental saver if checkpoints are needed (auto_save is true)
        if create_checkpoints {
            let incremental_saver = IncrementalAudioSaver::new(meeting_folder.clone(), 48000)?;
            self.incremental_saver = Some(Arc::new(AsyncMutex::new(incremental_saver)));
            info!(
                "✅ Incremental audio saver initialized for meeting: {}",
                meeting_name
            );
        } else {
            info!("⚠️  Skipped incremental audio saver (auto-save disabled)");
        }

        // Create initial metadata
        let metadata = MeetingMetadata {
            version: "1.0".to_string(),
            meeting_id: None, // Will be set by backend
            meeting_name: Some(meeting_name.to_string()),
            created_at: chrono::Utc::now().to_rfc3339(),
            completed_at: None,
            duration_seconds: None,
            devices: DeviceInfo {
                microphone: None, // Could be enhanced to store actual device names
                system_audio: None,
            },
            audio_file: if create_checkpoints {
                "audio.mp4".to_string()
            } else {
                "".to_string()
            },
            transcript_file: "transcripts.json".to_string(),
            sample_rate: 48000,
            status: "recording".to_string(),
        };

        // Write initial metadata.json
        self.write_metadata(&meeting_folder, &metadata)?;

        self.meeting_folder = Some(meeting_folder);
        self.metadata = Some(metadata);

        Ok(())
    }

    /// Reopen an existing meeting folder so a new session records into it
    ///
    /// Loads the prior transcripts.json segments and metadata.json, and (when
    /// `create_checkpoints` is true) sets up an incremental saver that appends
    /// the new audio to the existing audio.mp4 on finalize.
    fn reopen_meeting_folder(
        &mut self,
        target: &ResumeTarget,
        create_checkpoints: bool,
    ) -> Result<()> {
        let meeting_folder = target.meeting_folder.clone();
        if !meeting_folder.exists() {
            warn!(
                "Resume folder {} does not exist - recreating it",
                meeting_folder.display()
            );
            std::fs::create_dir_all(&meeting_folder)?;
        }

        if create_checkpoints {
            std::fs::create_dir_all(meeting_folder.join(".checkpoints"))?;
            let incremental_saver =
                IncrementalAudioSaver::new_resuming(meeting_folder.clone(), 48000)?;
            self.incremental_saver = Some(Arc::new(AsyncMutex::new(incremental_saver)));
            info!("✅ Incremental audio saver initialized to resume meeting");
        } else {
            info!("⚠️  Skipped incremental audio saver (auto-save disabled) - existing audio left as-is");
        }

        self.prior_segments = load_prior_segments(&meeting_folder);
        self.sequence_offset = next_sequence_offset(&self.prior_segments);
        info!(
            "Resuming with {} prior transcript segments (sequence offset {}, audio offset {:.2}s)",
            self.prior_segments.len(),
            self.sequence_offset,
            target.audio_offset_seconds
        );

        let metadata = resumed_metadata(
            &meeting_folder,
            self.meeting_name.as_deref(),
            create_checkpoints,
        );
        self.write_metadata(&meeting_folder, &metadata)?;

        self.meeting_folder = Some(meeting_folder);
        self.metadata = Some(metadata);

        Ok(())
    }

    /// Write metadata.json to disk (atomic write with temp file)
    fn write_metadata(&self, folder: &Path, metadata: &MeetingMetadata) -> Result<()> {
        let metadata_path = folder.join("metadata.json");
        let temp_path = folder.join(".metadata.json.tmp");

        let json_string = serde_json::to_string_pretty(metadata)?;
        std::fs::write(&temp_path, json_string)?;
        std::fs::rename(&temp_path, &metadata_path)?; // Atomic

        Ok(())
    }

    /// Write transcripts.json to disk (atomic write with temp file and validation)
    fn write_transcripts_json(&self, folder: &Path) -> Result<()> {
        // Clone segments to avoid holding lock during I/O
        let segments_clone = if let Ok(segments) = self.transcript_segments.lock() {
            if self.prior_segments.is_empty() {
                segments.clone()
            } else {
                let mut all = self.prior_segments.clone();
                all.extend(segments.iter().cloned());
                all
            }
        } else {
            error!("Failed to lock transcript segments for writing");
            return Err(anyhow::anyhow!("Failed to lock transcript segments"));
        };

        info!(
            "Writing {} transcript segments to JSON",
            segments_clone.len()
        );

        let transcript_path = folder.join("transcripts.json");
        let temp_path = folder.join(".transcripts.json.tmp");

        // Create JSON structure
        let json = serde_json::json!({
            "version": "1.0",
            "segments": segments_clone,
            "last_updated": chrono::Utc::now().to_rfc3339(),
            "total_segments": segments_clone.len()
        });

        // Serialize to pretty JSON string
        let json_string = serde_json::to_string_pretty(&json).map_err(|e| {
            error!("Failed to serialize transcripts to JSON: {}", e);
            anyhow::anyhow!("JSON serialization failed: {}", e)
        })?;

        // Write to temp file with error handling
        std::fs::write(&temp_path, &json_string).map_err(|e| {
            error!(
                "Failed to write transcript temp file to {}: {}",
                temp_path.display(),
                e
            );
            anyhow::anyhow!("Failed to write temp file: {}", e)
        })?;

        // Verify temp file was written correctly
        if !temp_path.exists() {
            error!(
                "Temp transcript file does not exist after write: {}",
                temp_path.display()
            );
            return Err(anyhow::anyhow!("Temp file verification failed"));
        }

        // Atomic rename
        std::fs::rename(&temp_path, &transcript_path).map_err(|e| {
            error!(
                "Failed to rename transcript file from {} to {}: {}",
                temp_path.display(),
                transcript_path.display(),
                e
            );
            anyhow::anyhow!("Failed to rename transcript file: {}", e)
        })?;

        info!(
            "✅ Successfully wrote transcripts.json with {} segments",
            segments_clone.len()
        );
        Ok(())
    }

    // in frontend/src-tauri/src/audio/recording_saver.rs
    pub fn get_stats(&self) -> (usize, u32) {
        if let Some(ref saver) = self.incremental_saver {
            if let Ok(guard) = saver.try_lock() {
                (guard.get_checkpoint_count() as usize, 48000)
            } else {
                (0, 48000)
            }
        } else {
            (0, 48000)
        }
    }

    /// Stop and save using incremental saving approach
    ///
    /// # Arguments
    /// * `app` - Tauri app handle for emitting events
    /// * `recording_duration` - Actual recording duration in seconds (from RecordingState)
    pub async fn stop_and_save<R: Runtime>(
        &mut self,
        app: &AppHandle<R>,
        recording_duration: Option<f64>,
    ) -> Result<(Option<String>, Vec<TranscriptSegment>), String> {
        info!("Stopping recording saver");

        // Stop accumulation
        if let Ok(mut is_saving) = self.is_saving.lock() {
            *is_saving = false;
        }

        // Give time for final chunks
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

        // Check if incremental saver exists (indicates auto_save was enabled)
        let should_save_audio = self.incremental_saver.is_some();

        if !should_save_audio {
            info!("⚠️  No audio saver initialized (auto-save was disabled) - skipping audio finalization");
            info!("✅ Transcripts and metadata already saved incrementally");
            return Ok((None, self.get_transcript_segments()));
        }

        // Finalize incremental saver (merge checkpoints into final audio.mp4)
        let final_audio_path = if let Some(saver_arc) = &self.incremental_saver {
            let mut saver = saver_arc.lock().await;
            match saver.finalize().await {
                Ok(path) => {
                    info!("✅ Successfully finalized audio: {}", path.display());
                    path
                }
                Err(e) => {
                    error!("❌ Failed to finalize incremental saver: {}", e);
                    return Err(format!("Failed to finalize audio: {}", e));
                }
            }
        } else {
            error!("No incremental saver initialized - cannot save recording");
            return Err("No incremental saver initialized".to_string());
        };

        // Save final transcripts.json with validation
        if let Some(folder) = &self.meeting_folder {
            if let Err(e) = self.write_transcripts_json(folder) {
                error!("❌ Failed to write final transcripts: {}", e);
                return Err(format!("Failed to save transcripts: {}", e));
            }

            // Verify transcripts were written correctly
            let transcript_path = folder.join("transcripts.json");
            if !transcript_path.exists() {
                error!(
                    "❌ Transcript file was not created at: {}",
                    transcript_path.display()
                );
                return Err("Transcript file verification failed".to_string());
            }
            info!(
                "✅ Transcripts saved and verified at: {}",
                transcript_path.display()
            );
        }

        // Update metadata to completed status with actual recording duration
        if let (Some(folder), Some(mut metadata)) = (&self.meeting_folder, self.metadata.clone()) {
            metadata.status = "completed".to_string();
            metadata.completed_at = Some(chrono::Utc::now().to_rfc3339());

            // Use actual recording duration from RecordingState (more accurate than transcript segments)
            // Falls back to last transcript segment if duration not provided
            // A resumed meeting's duration covers the audio recorded before it too.
            // Segment times are already shifted by that offset, so the fallback isn't.
            let audio_offset = self
                .resume_target
                .as_ref()
                .map_or(0.0, |target| target.audio_offset_seconds);
            metadata.duration_seconds = recording_duration
                .map(|duration| audio_offset + duration)
                .or_else(|| {
                    if let Ok(segments) = self.transcript_segments.lock() {
                        segments.last().map(|seg| seg.audio_end_time)
                    } else {
                        None
                    }
                })
                .or_else(|| self.prior_segments.last().map(|seg| seg.audio_end_time));

            if let Err(e) = self.write_metadata(folder, &metadata) {
                error!("❌ Failed to update metadata to completed: {}", e);
                return Err(format!("Failed to update metadata: {}", e));
            }

            info!(
                "✅ Metadata updated with duration: {:?}s",
                metadata.duration_seconds
            );
        }

        // Emit save event with audio and transcript paths
        let save_event = serde_json::json!({
            "audio_file": final_audio_path.to_string_lossy(),
            "transcript_file": self.meeting_folder.as_ref()
                .map(|f| f.join("transcripts.json").to_string_lossy().to_string()),
            "meeting_name": self.meeting_name,
            "meeting_folder": self.meeting_folder.as_ref()
                .map(|f| f.to_string_lossy().to_string())
        });

        if let Err(e) = app.emit("recording-saved", &save_event) {
            warn!("Failed to emit recording-saved event: {}", e);
        }

        // Move the exact saved segments out so the caller can retain immutable
        // completed-session history without reading the now-cleared saver.
        let completed_transcripts = self.take_transcript_segments();

        Ok((
            Some(final_audio_path.to_string_lossy().to_string()),
            completed_transcripts,
        ))
    }

    /// Get the meeting folder path (for passing to backend)
    pub fn get_meeting_folder(&self) -> Option<&PathBuf> {
        self.meeting_folder.as_ref()
    }

    /// Get accumulated transcript segments (for reload sync)
    pub fn get_transcript_segments(&self) -> Vec<TranscriptSegment> {
        if let Ok(segments) = self.transcript_segments.lock() {
            segments.clone()
        } else {
            Vec::new()
        }
    }

    fn take_transcript_segments(&self) -> Vec<TranscriptSegment> {
        self.transcript_segments
            .lock()
            .map(|mut segments| std::mem::take(&mut *segments))
            .unwrap_or_default()
    }

    /// Get meeting name (for reload sync)
    pub fn get_meeting_name(&self) -> Option<String> {
        self.meeting_name.clone()
    }
}

/// Read the segments of an existing transcripts.json (`{"segments": [...]}`).
/// A missing or unparseable file yields no prior segments.
fn load_prior_segments(folder: &Path) -> Vec<TranscriptSegment> {
    #[derive(Deserialize)]
    struct TranscriptsFile {
        segments: Vec<TranscriptSegment>,
    }

    let path = folder.join("transcripts.json");
    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(e) => {
            warn!("No prior transcripts loaded from {}: {}", path.display(), e);
            return Vec::new();
        }
    };

    match serde_json::from_str::<TranscriptsFile>(&contents) {
        Ok(file) => file.segments,
        Err(e) => {
            warn!(
                "Could not parse prior transcripts at {}: {}",
                path.display(),
                e
            );
            Vec::new()
        }
    }
}

/// First sequence_id free for a new session after the prior segments.
fn next_sequence_offset(prior: &[TranscriptSegment]) -> u64 {
    prior
        .iter()
        .map(|seg| seg.sequence_id + 1)
        .max()
        .unwrap_or(0)
}

/// Metadata for a resumed meeting: the existing metadata.json marked as recording
/// again, or fresh metadata when there is none to reuse.
fn resumed_metadata(
    folder: &Path,
    meeting_name: Option<&str>,
    saves_audio: bool,
) -> MeetingMetadata {
    let path = folder.join("metadata.json");
    let existing = std::fs::read_to_string(&path)
        .map_err(anyhow::Error::from)
        .and_then(|contents| {
            serde_json::from_str::<MeetingMetadata>(&contents).map_err(anyhow::Error::from)
        });

    let mut metadata = match existing {
        Ok(metadata) => metadata,
        Err(e) => {
            warn!(
                "Could not reuse metadata at {} ({}) - writing fresh metadata",
                path.display(),
                e
            );
            MeetingMetadata {
                version: "1.0".to_string(),
                meeting_id: None,
                meeting_name: meeting_name.map(str::to_string),
                created_at: chrono::Utc::now().to_rfc3339(),
                completed_at: None,
                duration_seconds: None,
                devices: DeviceInfo {
                    microphone: None,
                    system_audio: None,
                },
                audio_file: if folder.join("audio.mp4").exists() {
                    "audio.mp4".to_string()
                } else {
                    "".to_string()
                },
                transcript_file: "transcripts.json".to_string(),
                sample_rate: 48000,
                status: "recording".to_string(),
            }
        }
    };

    metadata.status = "recording".to_string();
    metadata.completed_at = None;
    if saves_audio {
        metadata.audio_file = "audio.mp4".to_string();
    }
    metadata
}

impl Default for RecordingSaver {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        load_prior_segments, next_sequence_offset, resumed_metadata, RecordingSaver, ResumeTarget,
        TranscriptSegment,
    };
    use std::path::PathBuf;

    fn segment(sequence_id: u64, text: &str, start: f64) -> TranscriptSegment {
        TranscriptSegment {
            id: format!("seg_{}", sequence_id),
            text: text.to_string(),
            audio_start_time: start,
            audio_end_time: start + 1.0,
            duration: 1.0,
            display_time: "[00:00]".to_string(),
            confidence: 0.9,
            sequence_id,
            speaker: None,
        }
    }

    fn temp_meeting_folder(label: &str) -> PathBuf {
        let unique = format!(
            "meetily-recording-saver-{}-{}-{}",
            label,
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        );
        let folder = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&folder).unwrap();
        folder
    }

    fn write_prior_transcripts(folder: &std::path::Path, segments: &[TranscriptSegment]) {
        let json = serde_json::json!({
            "version": "1.0",
            "segments": segments,
            "last_updated": "2026-09-22T10:00:00Z",
            "total_segments": segments.len()
        });
        std::fs::write(
            folder.join("transcripts.json"),
            serde_json::to_string_pretty(&json).unwrap(),
        )
        .unwrap();
    }

    fn read_written_transcripts(folder: &std::path::Path) -> serde_json::Value {
        let contents = std::fs::read_to_string(folder.join("transcripts.json")).unwrap();
        serde_json::from_str(&contents).unwrap()
    }

    fn resuming_saver(folder: &std::path::Path) -> RecordingSaver {
        let target = ResumeTarget {
            meeting_folder: folder.to_path_buf(),
            audio_offset_seconds: 90.0,
        };
        let mut saver = RecordingSaver::new();
        saver.set_meeting_name(Some("Standup".to_string()));
        saver.set_resume_target(Some(target.clone()));
        saver.reopen_meeting_folder(&target, false).unwrap();
        saver
    }

    #[test]
    fn next_sequence_offset_starts_after_the_highest_prior_sequence_id() {
        assert_eq!(next_sequence_offset(&[]), 0);
        let prior = vec![
            segment(4, "b", 4.0),
            segment(0, "a", 0.0),
            segment(2, "c", 2.0),
        ];
        assert_eq!(next_sequence_offset(&prior), 5);
    }

    #[test]
    fn resumed_segments_are_shifted_past_prior_ones_and_still_upsert() {
        let folder = temp_meeting_folder("shift");
        write_prior_transcripts(
            &folder,
            &[segment(0, "old a", 0.0), segment(1, "old b", 1.0)],
        );
        let saver = resuming_saver(&folder);

        saver.add_transcript_segment(segment(0, "partial", 90.0));
        saver.add_transcript_segment(segment(1, "next", 91.0));
        // Same raw sequence_id again: must replace, not duplicate.
        saver.add_transcript_segment(segment(0, "final", 90.0));

        let new_segments = saver.get_transcript_segments();
        assert_eq!(new_segments.len(), 2);
        assert_eq!(new_segments[0].sequence_id, 2);
        assert_eq!(new_segments[0].id, "seg_2");
        assert_eq!(new_segments[0].text, "final");
        assert_eq!(new_segments[1].sequence_id, 3);
        assert_eq!(new_segments[1].id, "seg_3");
        // Audio times are left alone; the transcription worker already shifted them.
        assert_eq!(new_segments[0].audio_start_time, 90.0);

        std::fs::remove_dir_all(&folder).ok();
    }

    #[test]
    fn resumed_transcripts_json_keeps_prior_segments_ahead_of_new_ones() {
        let folder = temp_meeting_folder("order");
        write_prior_transcripts(
            &folder,
            &[segment(0, "old a", 0.0), segment(1, "old b", 1.0)],
        );
        let saver = resuming_saver(&folder);

        saver.add_transcript_segment(segment(0, "new a", 90.0));

        let written = read_written_transcripts(&folder);
        let texts: Vec<&str> = written["segments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|seg| seg["text"].as_str().unwrap())
            .collect();
        assert_eq!(texts, vec!["old a", "old b", "new a"]);
        assert_eq!(written["total_segments"], 3);
        // Only the new session's segments go back to the caller for the DB append.
        let taken = saver.take_transcript_segments();
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].text, "new a");

        std::fs::remove_dir_all(&folder).ok();
    }

    #[test]
    fn reopening_a_meeting_folder_loads_prior_transcripts_and_metadata() {
        let folder = temp_meeting_folder("reopen");
        write_prior_transcripts(
            &folder,
            &[segment(0, "old a", 0.0), segment(3, "old b", 3.0)],
        );
        std::fs::write(
            folder.join("metadata.json"),
            r#"{
                "version": "1.0",
                "meeting_id": "meeting-42",
                "meeting_name": "Original name",
                "created_at": "2026-09-22T09:00:00Z",
                "completed_at": "2026-09-22T09:30:00Z",
                "duration_seconds": 90.0,
                "devices": { "microphone": "Built-in", "system_audio": null },
                "audio_file": "audio.mp4",
                "transcript_file": "transcripts.json",
                "sample_rate": 48000,
                "status": "completed"
            }"#,
        )
        .unwrap();

        let saver = resuming_saver(&folder);

        assert_eq!(saver.prior_segments.len(), 2);
        assert_eq!(saver.sequence_offset, 4);
        assert!(saver.get_transcript_segments().is_empty());
        assert_eq!(saver.get_meeting_folder(), Some(&folder));

        let metadata = saver.metadata.clone().unwrap();
        assert_eq!(metadata.meeting_id.as_deref(), Some("meeting-42"));
        assert_eq!(metadata.meeting_name.as_deref(), Some("Original name"));
        assert_eq!(metadata.created_at, "2026-09-22T09:00:00Z");
        assert_eq!(metadata.devices.microphone.as_deref(), Some("Built-in"));
        assert_eq!(metadata.status, "recording");
        assert!(metadata.completed_at.is_none());
        // Written back to disk as well.
        let on_disk: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(folder.join("metadata.json")).unwrap())
                .unwrap();
        assert_eq!(on_disk["status"], "recording");
        assert_eq!(on_disk["meeting_id"], "meeting-42");

        std::fs::remove_dir_all(&folder).ok();
    }

    #[test]
    fn missing_or_corrupt_prior_files_fall_back_to_empty_and_fresh() {
        let folder = temp_meeting_folder("corrupt");
        assert!(load_prior_segments(&folder).is_empty());

        std::fs::write(folder.join("transcripts.json"), "{ not json").unwrap();
        std::fs::write(folder.join("metadata.json"), "{ not json").unwrap();
        assert!(load_prior_segments(&folder).is_empty());

        let metadata = resumed_metadata(&folder, Some("Standup"), true);
        assert_eq!(metadata.meeting_name.as_deref(), Some("Standup"));
        assert_eq!(metadata.audio_file, "audio.mp4");
        assert_eq!(metadata.status, "recording");

        std::fs::remove_dir_all(&folder).ok();
    }

    #[test]
    fn successful_autosave_cleanup_keeps_the_captured_transcript_snapshot() {
        let saver = RecordingSaver::new();
        saver.add_transcript_segment(TranscriptSegment {
            id: "segment-7".to_string(),
            text: "final words".to_string(),
            audio_start_time: 7.0,
            audio_end_time: 8.5,
            duration: 1.5,
            display_time: "00:07".to_string(),
            confidence: 0.9,
            sequence_id: 7,
            speaker: Some("microphone".to_string()),
        });

        let completed = saver.take_transcript_segments();

        assert!(saver.get_transcript_segments().is_empty());
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].sequence_id, 7);
        assert_eq!(completed[0].speaker.as_deref(), Some("microphone"));
        assert_eq!(completed[0].audio_start_time, 7.0);
        assert_eq!(completed[0].audio_end_time, 8.5);
    }
}
