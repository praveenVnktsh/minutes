use super::encode::encode_single_audio;
use super::recording_state::AudioChunk;
use anyhow::{anyhow, Result};
use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use super::ffmpeg::find_ffmpeg_path;

/// Temp output used when new audio is appended to an existing audio.mp4.
/// FFmpeg cannot read and write the same file, so the merge goes here first
/// and is renamed over audio.mp4 only once it succeeded.
const RESUMED_AUDIO_TEMP_FILE: &str = ".audio.resumed.mp4";

/// Build the FFmpeg concat demuxer list: the prior audio (if any) first,
/// then the checkpoint chunks in the order given.
///
/// Paths should be absolute (required with `-safe 0`). Single quotes are
/// escaped the way the concat demuxer expects (`'` -> `'\''`), since meeting
/// folder names may contain apostrophes.
fn build_concat_list(prior_audio: Option<&Path>, chunk_paths: &[PathBuf]) -> String {
    prior_audio
        .into_iter()
        .chain(chunk_paths.iter().map(PathBuf::as_path))
        .map(|path| {
            format!(
                "file '{}'\n",
                path.display().to_string().replace('\'', "'\\''")
            )
        })
        .collect()
}

/// Whether an existing audio.mp4 was written at or after the newest checkpoint,
/// i.e. a finalize already merged these checkpoints into it and only the
/// checkpoint cleanup failed. Recovery must not prepend it again in that case.
fn audio_already_contains_checkpoints(
    audio_modified: std::time::SystemTime,
    newest_checkpoint_modified: std::time::SystemTime,
) -> bool {
    audio_modified >= newest_checkpoint_modified
}

/// Run the FFmpeg concat demuxer over `list_file` into `output` without re-encoding.
fn run_ffmpeg_concat(list_file: &Path, output: &Path) -> Result<()> {
    let ffmpeg_path = find_ffmpeg_path()
        .ok_or_else(|| anyhow!("FFmpeg not found. Please install FFmpeg to merge audio."))?;
    info!("Using FFmpeg at: {:?}", ffmpeg_path);

    let mut command = std::process::Command::new(ffmpeg_path);

    command.args([
        "-f",
        "concat", // Use concat demuxer
        "-safe",
        "0", // Allow absolute paths
        "-i",
        list_file
            .to_str()
            .ok_or_else(|| anyhow!("Invalid concat list path"))?,
        "-c",
        "copy", // Copy codec - no re-encoding!
        "-y",   // Overwrite output file
        output
            .to_str()
            .ok_or_else(|| anyhow!("Invalid output path"))?,
    ]);

    // Hide console window on Windows to prevent CMD popup during finalization
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let ffmpeg_output = command
        .output()
        .map_err(|e| anyhow!("Failed to run FFmpeg: {}", e))?;

    if !ffmpeg_output.status.success() {
        let stderr = String::from_utf8_lossy(&ffmpeg_output.stderr);
        error!("FFmpeg concat failed: {}", stderr);
        return Err(anyhow!("FFmpeg concat failed: {}", stderr));
    }

    // Verify output file was created
    if !output.exists() {
        return Err(anyhow!(
            "Merged audio file was not created: {}",
            output.display()
        ));
    }

    Ok(())
}

/// Merge `chunk_paths` (after `prior_audio`, when given) into `final_path`.
///
/// With prior audio the merge is written to a temp file in the same folder and
/// renamed over `final_path` only after FFmpeg succeeded, so a failed merge
/// leaves the existing audio untouched.
fn concat_into(
    list_file: &Path,
    prior_audio: Option<&Path>,
    chunk_paths: &[PathBuf],
    final_path: &Path,
) -> Result<()> {
    std::fs::write(list_file, build_concat_list(prior_audio, chunk_paths))?;

    if prior_audio.is_none() {
        return run_ffmpeg_concat(list_file, final_path);
    }

    let temp_path = final_path.with_file_name(RESUMED_AUDIO_TEMP_FILE);
    if temp_path.exists() {
        warn!("Removing stale temp audio file: {}", temp_path.display());
        std::fs::remove_file(&temp_path)?;
    }

    if let Err(e) = run_ffmpeg_concat(list_file, &temp_path) {
        // Leave the original audio.mp4 and checkpoints alone; just drop partial output
        let _ = std::fs::remove_file(&temp_path);
        return Err(e);
    }

    std::fs::rename(&temp_path, final_path).map_err(|e| {
        anyhow!(
            "Failed to replace {} with merged audio: {}",
            final_path.display(),
            e
        )
    })?;

    Ok(())
}

/// Audio data without device type (we only store mixed audio)
#[derive(Clone)]
struct AudioData {
    data: Vec<f32>,
    // sample_rate: u32,
}

/// Incremental audio saver that writes checkpoints every 30 seconds
/// to minimize memory usage and enable crash recovery
pub struct IncrementalAudioSaver {
    checkpoint_buffer: Vec<AudioData>,
    checkpoint_interval_samples: usize, // 30s at 48kHz = 1,440,000 samples
    checkpoint_count: u32,
    checkpoints_dir: PathBuf,
    meeting_folder: PathBuf,
    sample_rate: u32,
    /// Existing audio.mp4 of a resumed meeting; new audio is appended after it on finalize
    prior_audio: Option<PathBuf>,
}

impl IncrementalAudioSaver {
    /// Create a new incremental saver
    ///
    /// # Arguments
    /// * `meeting_folder` - Path to the meeting folder (contains .checkpoints/)
    /// * `sample_rate` - Sample rate of audio (typically 48000)
    pub fn new(meeting_folder: PathBuf, sample_rate: u32) -> Result<Self> {
        let checkpoints_dir = meeting_folder.join(".checkpoints");

        // Verify checkpoints directory exists
        if !checkpoints_dir.exists() {
            return Err(anyhow!(
                "Checkpoints directory does not exist: {}",
                checkpoints_dir.display()
            ));
        }

        Ok(Self {
            checkpoint_buffer: Vec::new(),
            checkpoint_interval_samples: sample_rate as usize * 30, // 30 seconds
            checkpoint_count: 0,
            checkpoints_dir,
            meeting_folder,
            sample_rate,
            prior_audio: None,
        })
    }

    /// Create a saver that records into an existing meeting folder
    ///
    /// Same as [`IncrementalAudioSaver::new`], but if the folder already has an
    /// audio.mp4 it is kept as the prior audio and the new recording is appended
    /// after it on finalize.
    pub fn new_resuming(meeting_folder: PathBuf, sample_rate: u32) -> Result<Self> {
        let mut saver = Self::new(meeting_folder, sample_rate)?;

        let existing_audio = saver.meeting_folder.join("audio.mp4");
        if existing_audio.is_file() {
            info!(
                "Resuming meeting: new audio will be appended to {}",
                existing_audio.display()
            );
            saver.prior_audio = Some(existing_audio);
        } else {
            info!(
                "Resuming meeting without existing audio in {}",
                saver.meeting_folder.display()
            );
        }

        Ok(saver)
    }

    /// Add an audio chunk to the buffer
    /// Automatically saves a checkpoint when buffer reaches 30 seconds
    pub fn add_chunk(&mut self, chunk: AudioChunk) -> Result<()> {
        let audio_data = AudioData {
            data: chunk.data,
            // sample_rate: chunk.sample_rate,
        };

        self.checkpoint_buffer.push(audio_data);

        // Calculate total samples in buffer
        let total_samples: usize = self.checkpoint_buffer.iter().map(|c| c.data.len()).sum();

        // Save checkpoint when buffer reaches threshold (30 seconds)
        if total_samples >= self.checkpoint_interval_samples {
            self.save_checkpoint()?;
            self.checkpoint_buffer.clear();
        }

        Ok(())
    }

    /// Save current buffer as a checkpoint file
    fn save_checkpoint(&mut self) -> Result<()> {
        // Concatenate all chunks in buffer
        let audio_data: Vec<f32> = self
            .checkpoint_buffer
            .iter()
            .flat_map(|c| &c.data)
            .cloned()
            .collect();

        if audio_data.is_empty() {
            warn!("Attempted to save empty checkpoint, skipping");
            return Ok(());
        }

        // Generate checkpoint filename
        let checkpoint_path = self
            .checkpoints_dir
            .join(format!("audio_chunk_{:03}.mp4", self.checkpoint_count));

        // Encode and save checkpoint
        encode_single_audio(
            bytemuck::cast_slice(&audio_data),
            self.sample_rate,
            1, // mono
            &checkpoint_path,
        )?;

        let duration_seconds = audio_data.len() as f32 / self.sample_rate as f32;
        self.checkpoint_count += 1;

        info!(
            "Saved checkpoint {}: {:.2}s of audio ({} samples)",
            self.checkpoint_count,
            duration_seconds,
            audio_data.len()
        );

        Ok(())
    }

    /// Finalize the recording: save final checkpoint, merge all checkpoints, cleanup
    ///
    /// Returns the path to the final merged audio.mp4 file
    pub async fn finalize(&mut self) -> Result<PathBuf> {
        info!("Finalizing incremental recording...");

        // Save final buffer if not empty
        if !self.checkpoint_buffer.is_empty() {
            info!(
                "Saving final checkpoint with remaining {} chunks",
                self.checkpoint_buffer.len()
            );
            self.save_checkpoint()?;
            self.checkpoint_buffer.clear();
        }

        if self.checkpoint_count == 0 {
            // A resumed meeting that captured nothing new still has its earlier audio
            if let Some(prior_audio) = &self.prior_audio {
                info!(
                    "No new audio captured while resuming; keeping {}",
                    prior_audio.display()
                );
                if let Err(e) = std::fs::remove_dir_all(&self.checkpoints_dir) {
                    warn!("Failed to clean up checkpoints directory: {}", e);
                }
                return Ok(prior_audio.clone());
            }

            return Err(anyhow!(
                "No audio checkpoints to merge - recording may have failed"
            ));
        }

        // Merge all checkpoints (after any prior audio) using FFmpeg concat
        let final_audio_path = self.meeting_folder.join("audio.mp4");
        self.merge_checkpoints(&final_audio_path).await?;

        // Clean up checkpoints directory
        info!("Cleaning up {} checkpoint files", self.checkpoint_count);
        if let Err(e) = std::fs::remove_dir_all(&self.checkpoints_dir) {
            warn!("Failed to clean up checkpoints directory: {}", e);
            // Non-fatal - user can manually delete
        }

        info!("Finalized recording: {}", final_audio_path.display());

        Ok(final_audio_path)
    }

    /// Merge all checkpoint files into final audio.mp4 using FFmpeg concat
    /// Uses concat demuxer for fast merging without re-encoding.
    /// When resuming, the prior audio.mp4 goes first in the list.
    async fn merge_checkpoints(&self, output: &Path) -> Result<()> {
        info!(
            "Merging {} checkpoints into final audio file...",
            self.checkpoint_count
        );

        let mut chunk_paths = Vec::with_capacity(self.checkpoint_count as usize);
        for i in 0..self.checkpoint_count {
            let checkpoint_path = self
                .checkpoints_dir
                .join(format!("audio_chunk_{:03}.mp4", i));

            // Verify checkpoint exists
            if !checkpoint_path.exists() {
                return Err(anyhow!(
                    "Checkpoint file missing: {}",
                    checkpoint_path.display()
                ));
            }

            // Use absolute path for FFmpeg (required for safe mode)
            chunk_paths.push(checkpoint_path.canonicalize()?);
        }

        let prior_audio = match &self.prior_audio {
            Some(path) => Some(
                path.canonicalize()
                    .map_err(|e| anyhow!("Prior audio missing: {} ({})", path.display(), e))?,
            ),
            None => None,
        };

        let list_file = self.checkpoints_dir.join("concat_list.txt");
        concat_into(&list_file, prior_audio.as_deref(), &chunk_paths, output)?;

        info!(
            "Successfully merged {} checkpoints{} → {}",
            self.checkpoint_count,
            if prior_audio.is_some() {
                " after existing audio"
            } else {
                ""
            },
            output.display()
        );

        Ok(())
    }

    /// Get the meeting folder path
    pub fn get_meeting_folder(&self) -> &PathBuf {
        &self.meeting_folder
    }

    /// Get current checkpoint count
    pub fn get_checkpoint_count(&self) -> u32 {
        self.checkpoint_count
    }
}

/// Audio recovery status for transcript recovery feature
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioRecoveryStatus {
    pub status: String, // "success" | "partial" | "failed" | "none"
    pub chunk_count: u32,
    pub estimated_duration_seconds: f64,
    pub audio_file_path: Option<String>,
    pub message: String,
}

/// Recover audio from checkpoint files
/// This is called by the transcript recovery system to merge audio chunks after a crash
#[tauri::command]
pub async fn recover_audio_from_checkpoints(
    meeting_folder: String,
    _sample_rate: u32,
) -> Result<AudioRecoveryStatus, String> {
    info!("Starting audio recovery for folder: {}", meeting_folder);

    let folder_path = PathBuf::from(&meeting_folder);
    let checkpoints_dir = folder_path.join(".checkpoints");

    // Check if checkpoints directory exists
    if !checkpoints_dir.exists() {
        info!(
            "No checkpoints directory found at: {}",
            checkpoints_dir.display()
        );
        return Ok(AudioRecoveryStatus {
            status: "none".to_string(),
            chunk_count: 0,
            estimated_duration_seconds: 0.0,
            audio_file_path: None,
            message: "No audio checkpoints found".to_string(),
        });
    }

    // Scan for checkpoint files
    let mut checkpoint_files: Vec<_> = std::fs::read_dir(&checkpoints_dir)
        .map_err(|e| format!("Failed to read checkpoints directory: {}", e))?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().extension().and_then(|s| s.to_str()) == Some("mp4"))
        .collect();

    if checkpoint_files.is_empty() {
        info!(
            "No checkpoint files found in: {}",
            checkpoints_dir.display()
        );
        return Ok(AudioRecoveryStatus {
            status: "none".to_string(),
            chunk_count: 0,
            estimated_duration_seconds: 0.0,
            audio_file_path: None,
            message: "No audio checkpoint files found".to_string(),
        });
    }

    // Sort by filename (audio_chunk_000.mp4, audio_chunk_001.mp4, etc.)
    checkpoint_files.sort_by_key(|entry| entry.path());

    let chunk_count = checkpoint_files.len() as u32;
    let estimated_duration = (chunk_count as f64) * 30.0; // 30 seconds per chunk

    info!(
        "Found {} checkpoint files, estimated duration: {:.2}s",
        chunk_count, estimated_duration
    );

    // Use absolute paths for FFmpeg (required for safe mode)
    let chunk_paths = checkpoint_files
        .iter()
        .map(|entry| entry.path().canonicalize())
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(|e| format!("Failed to canonicalize path: {}", e))?;

    let output_path = folder_path.join("audio.mp4");
    let output_path_str = output_path
        .to_str()
        .ok_or("Invalid output path")?
        .to_string();

    // A resumed meeting that crashed already has audio.mp4 from its earlier part(s).
    // Prepend it so recovery never overwrites the audio recorded before the resume.
    let mut prior_audio = None;
    if output_path.is_file() {
        let newest_checkpoint = checkpoint_files
            .iter()
            .filter_map(|entry| entry.metadata().ok()?.modified().ok())
            .max();
        let audio_modified = std::fs::metadata(&output_path)
            .and_then(|m| m.modified())
            .ok();

        // Finalize already merged these checkpoints and only the cleanup failed:
        // prepending again would duplicate the audio.
        if let (Some(audio_modified), Some(newest_checkpoint)) = (audio_modified, newest_checkpoint)
        {
            if audio_already_contains_checkpoints(audio_modified, newest_checkpoint) {
                info!(
                    "Existing audio already contains the checkpoints: {}",
                    output_path_str
                );
                return Ok(AudioRecoveryStatus {
                    status: "success".to_string(),
                    chunk_count,
                    estimated_duration_seconds: estimated_duration,
                    audio_file_path: Some(output_path_str),
                    message: "Audio was already merged".to_string(),
                });
            }
        }

        info!(
            "Recovering resumed meeting: appending checkpoints to existing {}",
            output_path_str
        );
        prior_audio = Some(
            output_path
                .canonicalize()
                .map_err(|e| format!("Failed to canonicalize path: {}", e))?,
        );
    }

    // Merge chunks with FFmpeg concat
    let concat_file_path = checkpoints_dir.join("concat_list.txt");

    match concat_into(
        &concat_file_path,
        prior_audio.as_deref(),
        &chunk_paths,
        &output_path,
    ) {
        Ok(()) => {
            // Clean up concat file
            let _ = std::fs::remove_file(concat_file_path);

            info!("Successfully recovered audio: {}", output_path_str);

            Ok(AudioRecoveryStatus {
                status: "success".to_string(),
                chunk_count,
                estimated_duration_seconds: estimated_duration,
                audio_file_path: Some(output_path_str),
                message: format!("Successfully recovered {} audio chunks", chunk_count),
            })
        }
        Err(e) => {
            error!("Audio recovery failed: {}", e);
            Ok(AudioRecoveryStatus {
                status: "failed".to_string(),
                chunk_count,
                estimated_duration_seconds: estimated_duration,
                audio_file_path: None,
                message: e.to_string(),
            })
        }
    }
}

/// Clean up checkpoint files after successful recording or recovery
/// This command is called by the frontend after successful save to clean up checkpoint files
#[tauri::command]
pub async fn cleanup_checkpoints(meeting_folder: String) -> Result<(), String> {
    info!("Cleaning up checkpoints for folder: {}", meeting_folder);

    let folder_path = PathBuf::from(&meeting_folder);
    let checkpoints_dir = folder_path.join(".checkpoints");

    if checkpoints_dir.exists() {
        std::fs::remove_dir_all(&checkpoints_dir)
            .map_err(|e| format!("Failed to remove checkpoints directory: {}", e))?;
        info!("Successfully cleaned up checkpoints directory");
    } else {
        info!("No checkpoints directory to clean up");
    }

    Ok(())
}

/// Check if a meeting folder has audio checkpoint files
/// Returns true if .checkpoints/ directory exists and contains .mp4 files
#[tauri::command]
pub async fn has_audio_checkpoints(meeting_folder: String) -> Result<bool, String> {
    let folder_path = PathBuf::from(&meeting_folder);
    let checkpoints_dir = folder_path.join(".checkpoints");

    // Check if checkpoints directory exists
    if !checkpoints_dir.exists() {
        return Ok(false);
    }

    // Scan for .mp4 checkpoint files
    let has_mp4_files = std::fs::read_dir(&checkpoints_dir)
        .map_err(|e| format!("Failed to read checkpoints directory: {}", e))?
        .filter_map(|entry| entry.ok())
        .any(|entry| entry.path().extension().and_then(|s| s.to_str()) == Some("mp4"));

    Ok(has_mp4_files)
}

#[cfg(test)]
mod tests {
    use super::super::recording_state::DeviceType;
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_checkpoint_creation() {
        // Create temp meeting folder
        let temp_dir = tempdir().unwrap();
        let meeting_folder = temp_dir.path().join("Test_Meeting");
        std::fs::create_dir_all(&meeting_folder).unwrap();
        std::fs::create_dir_all(meeting_folder.join(".checkpoints")).unwrap();

        let mut saver = IncrementalAudioSaver::new(meeting_folder.clone(), 48000).unwrap();

        // Add 60 seconds worth of audio (should create 2 checkpoints)
        for i in 0..120 {
            // 120 chunks of 0.5s each
            let chunk = AudioChunk {
                data: vec![0.5f32; 24000], // 0.5s at 48kHz
                sample_rate: 48000,
                timestamp: i as f64 * 0.5, // timestamp in seconds
                chunk_id: i as u64,
                device_type: DeviceType::Microphone,
            };
            saver.add_chunk(chunk).unwrap();
        }

        // Verify 2 checkpoints created
        assert_eq!(saver.checkpoint_count, 2);

        // Finalize and verify merge
        let final_path = saver.finalize().await.unwrap();
        assert!(final_path.exists());

        // Verify checkpoints directory deleted
        assert!(!meeting_folder.join(".checkpoints").exists());
    }

    #[tokio::test]
    async fn test_empty_recording() {
        let temp_dir = tempdir().unwrap();
        let meeting_folder = temp_dir.path().join("Empty_Test");
        std::fs::create_dir_all(&meeting_folder).unwrap();
        std::fs::create_dir_all(meeting_folder.join(".checkpoints")).unwrap();

        let mut saver = IncrementalAudioSaver::new(meeting_folder.clone(), 48000).unwrap();

        // Try to finalize without adding any chunks
        let result = saver.finalize().await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("No audio checkpoints"));
    }

    fn silent_chunk(i: u64) -> AudioChunk {
        AudioChunk {
            data: vec![0.0f32; 24000], // 0.5s at 48kHz
            sample_rate: 48000,
            timestamp: i as f64 * 0.5,
            chunk_id: i,
            device_type: DeviceType::Microphone,
        }
    }

    #[test]
    fn concat_list_puts_chunks_in_order_without_prior_audio() {
        let chunks = vec![
            PathBuf::from("/m/.checkpoints/audio_chunk_000.mp4"),
            PathBuf::from("/m/.checkpoints/audio_chunk_001.mp4"),
        ];

        assert_eq!(
            build_concat_list(None, &chunks),
            "file '/m/.checkpoints/audio_chunk_000.mp4'\n\
             file '/m/.checkpoints/audio_chunk_001.mp4'\n"
        );
    }

    #[test]
    fn concat_list_puts_prior_audio_first() {
        let chunks = vec![
            PathBuf::from("/m/.checkpoints/audio_chunk_000.mp4"),
            PathBuf::from("/m/.checkpoints/audio_chunk_001.mp4"),
        ];

        assert_eq!(
            build_concat_list(Some(Path::new("/m/audio.mp4")), &chunks),
            "file '/m/audio.mp4'\n\
             file '/m/.checkpoints/audio_chunk_000.mp4'\n\
             file '/m/.checkpoints/audio_chunk_001.mp4'\n"
        );
    }

    #[test]
    fn concat_list_escapes_single_quotes() {
        let chunks = vec![PathBuf::from(
            "/Bob's Sync/.checkpoints/audio_chunk_000.mp4",
        )];

        assert_eq!(
            build_concat_list(Some(Path::new("/Bob's Sync/audio.mp4")), &chunks),
            "file '/Bob'\\''s Sync/audio.mp4'\n\
             file '/Bob'\\''s Sync/.checkpoints/audio_chunk_000.mp4'\n"
        );
    }

    #[test]
    fn existing_audio_newer_than_checkpoints_is_already_merged() {
        let checkpoint = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000);
        let later = checkpoint + std::time::Duration::from_secs(5);
        let earlier = checkpoint - std::time::Duration::from_secs(600);

        assert!(audio_already_contains_checkpoints(later, checkpoint));
        assert!(audio_already_contains_checkpoints(checkpoint, checkpoint));
        assert!(!audio_already_contains_checkpoints(earlier, checkpoint));
    }

    #[test]
    fn new_resuming_detects_existing_audio() {
        let temp_dir = tempdir().unwrap();
        let meeting_folder = temp_dir.path().join("Resumed_Meeting");
        std::fs::create_dir_all(meeting_folder.join(".checkpoints")).unwrap();
        std::fs::write(meeting_folder.join("audio.mp4"), b"prior").unwrap();

        let saver = IncrementalAudioSaver::new_resuming(meeting_folder.clone(), 48000).unwrap();
        assert_eq!(saver.prior_audio, Some(meeting_folder.join("audio.mp4")));

        // A plain new() never treats existing audio as prior audio
        let saver = IncrementalAudioSaver::new(meeting_folder.clone(), 48000).unwrap();
        assert_eq!(saver.prior_audio, None);
    }

    #[test]
    fn new_resuming_without_existing_audio_has_no_prior() {
        let temp_dir = tempdir().unwrap();
        let meeting_folder = temp_dir.path().join("Resumed_Meeting");
        std::fs::create_dir_all(meeting_folder.join(".checkpoints")).unwrap();

        let saver = IncrementalAudioSaver::new_resuming(meeting_folder.clone(), 48000).unwrap();
        assert_eq!(saver.prior_audio, None);
    }

    #[test]
    fn new_resuming_requires_checkpoints_dir() {
        let temp_dir = tempdir().unwrap();
        let meeting_folder = temp_dir.path().join("Resumed_Meeting");
        std::fs::create_dir_all(&meeting_folder).unwrap();
        std::fs::write(meeting_folder.join("audio.mp4"), b"prior").unwrap();

        assert!(IncrementalAudioSaver::new_resuming(meeting_folder, 48000).is_err());
    }

    #[tokio::test]
    async fn resumed_finalize_without_new_audio_keeps_existing_audio() {
        let temp_dir = tempdir().unwrap();
        let meeting_folder = temp_dir.path().join("Resumed_Meeting");
        std::fs::create_dir_all(meeting_folder.join(".checkpoints")).unwrap();
        std::fs::write(meeting_folder.join("audio.mp4"), b"prior").unwrap();

        let mut saver = IncrementalAudioSaver::new_resuming(meeting_folder.clone(), 48000).unwrap();
        let final_path = saver.finalize().await.unwrap();

        assert_eq!(final_path, meeting_folder.join("audio.mp4"));
        assert_eq!(std::fs::read(&final_path).unwrap(), b"prior");
        assert!(!meeting_folder.join(".checkpoints").exists());
    }

    #[tokio::test]
    async fn resumed_finalize_appends_new_audio_after_existing_audio() {
        if find_ffmpeg_path().is_none() {
            eprintln!("FFmpeg not available, skipping");
            return;
        }

        let temp_dir = tempdir().unwrap();
        let meeting_folder = temp_dir.path().join("Bob's Resumed Meeting");
        std::fs::create_dir_all(meeting_folder.join(".checkpoints")).unwrap();

        // First session: 1s of audio
        let mut first = IncrementalAudioSaver::new(meeting_folder.clone(), 48000).unwrap();
        for i in 0..2 {
            first.add_chunk(silent_chunk(i)).unwrap();
        }
        let audio_path = first.finalize().await.unwrap();
        let first_size = std::fs::metadata(&audio_path).unwrap().len();

        // Resumed session: another 1s appended to the same audio.mp4
        std::fs::create_dir_all(meeting_folder.join(".checkpoints")).unwrap();
        let mut resumed =
            IncrementalAudioSaver::new_resuming(meeting_folder.clone(), 48000).unwrap();
        for i in 0..2 {
            resumed.add_chunk(silent_chunk(i)).unwrap();
        }
        let resumed_path = resumed.finalize().await.unwrap();

        assert_eq!(resumed_path, audio_path);
        assert!(std::fs::metadata(&resumed_path).unwrap().len() > first_size);
        assert!(!meeting_folder.join(RESUMED_AUDIO_TEMP_FILE).exists());
        assert!(!meeting_folder.join(".checkpoints").exists());
    }
}
