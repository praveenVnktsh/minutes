use super::encode::encode_single_audio;
use super::recording_state::AudioChunk;
use anyhow::{anyhow, Result};
use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use super::ffmpeg::find_ffmpeg_path;

/// Temp output for every merge into audio.mp4. FFmpeg cannot read and write
/// the same file (a resumed meeting reads the old audio.mp4), and writing here
/// first means audio.mp4 only ever holds a completed merge: it is replaced by
/// an atomic rename once FFmpeg succeeded, never written in place.
const MERGED_AUDIO_TEMP_FILE: &str = ".audio.merging.mp4";

/// Written into .checkpoints/ by a resumed session. Records which audio.mp4 the
/// checkpoints are to be appended to, so recovery can tell the untouched prior
/// audio (prepend it) from one that finalize already replaced (already merged).
/// Not an .mp4, so recovery never mistakes it for a checkpoint.
const PRIOR_AUDIO_MARKER_FILE: &str = "prior_audio.json";

/// Size and modification time of an audio file, used to recognise the exact
/// audio.mp4 a resumed session started from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct AudioFingerprint {
    len: u64,
    modified_nanos: u128,
}

impl AudioFingerprint {
    fn of(path: &Path) -> std::io::Result<Self> {
        let metadata = std::fs::metadata(path)?;
        let modified_nanos = metadata
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?
            .as_nanos();
        Ok(Self {
            len: metadata.len(),
            modified_nanos,
        })
    }
}

/// What recovery should do with an existing audio.mp4 next to the checkpoints.
#[derive(Debug, PartialEq, Eq)]
enum ExistingAudio {
    /// Not part of this recording (or a leftover); rebuild audio.mp4 from checkpoints alone
    Ignore,
    /// The resumed meeting's earlier audio; the checkpoints go after it
    Prepend,
    /// Finalize already replaced the prior audio with the merged result
    AlreadyMerged,
}

/// Decide how recovery treats audio.mp4, from the resume marker (if any) and
/// the current audio.mp4's fingerprint (if it exists).
///
/// Without a marker the recording was not a resume, so the checkpoints are the
/// whole recording and are always re-merged: any audio.mp4 there may be a
/// partial write from an older build and must not be trusted.
fn classify_existing_audio(
    marker: Option<&AudioFingerprint>,
    current: Option<&AudioFingerprint>,
) -> ExistingAudio {
    match (marker, current) {
        (Some(marker), Some(current)) if marker == current => ExistingAudio::Prepend,
        // Merges only ever replace audio.mp4 by an atomic rename, so a changed
        // file is a completed merge of the prior audio and these checkpoints.
        (Some(_), Some(_)) => ExistingAudio::AlreadyMerged,
        _ => ExistingAudio::Ignore,
    }
}

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
/// The merge is written to a temp file in the same folder and renamed over
/// `final_path` only after FFmpeg succeeded, so an interrupted or failed merge
/// never leaves a partial audio.mp4 and leaves any existing one untouched.
fn concat_into(
    list_file: &Path,
    prior_audio: Option<&Path>,
    chunk_paths: &[PathBuf],
    final_path: &Path,
) -> Result<()> {
    std::fs::write(list_file, build_concat_list(prior_audio, chunk_paths))?;

    let temp_path = final_path.with_file_name(MERGED_AUDIO_TEMP_FILE);
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
            // Recovery needs this to know the checkpoints belong after audio.mp4
            let fingerprint = AudioFingerprint::of(&existing_audio)?;
            std::fs::write(
                saver.checkpoints_dir.join(PRIOR_AUDIO_MARKER_FILE),
                serde_json::to_vec(&fingerprint)?,
            )?;
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
    // Its marker says so; prepend that audio so recovery never overwrites it.
    let marker_path = checkpoints_dir.join(PRIOR_AUDIO_MARKER_FILE);
    let marker = if marker_path.is_file() {
        let parsed = std::fs::read(&marker_path)
            .map_err(|e| e.to_string())
            .and_then(|bytes| {
                serde_json::from_slice::<AudioFingerprint>(&bytes).map_err(|e| e.to_string())
            });
        match parsed {
            Ok(marker) => Some(marker),
            Err(e) => {
                // Can't tell whether audio.mp4 must be kept: leave everything in place
                error!("Unreadable resume marker {}: {}", marker_path.display(), e);
                return Ok(AudioRecoveryStatus {
                    status: "failed".to_string(),
                    chunk_count,
                    estimated_duration_seconds: estimated_duration,
                    audio_file_path: None,
                    message: format!("Unreadable resume marker: {}", e),
                });
            }
        }
    } else {
        None
    };
    let current = if output_path.is_file() {
        Some(
            AudioFingerprint::of(&output_path)
                .map_err(|e| format!("Failed to read {}: {}", output_path_str, e))?,
        )
    } else {
        None
    };

    let mut prior_audio = None;
    match classify_existing_audio(marker.as_ref(), current.as_ref()) {
        ExistingAudio::AlreadyMerged => {
            // Finalize's rename landed and only the checkpoint cleanup failed
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
        ExistingAudio::Prepend => {
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
        ExistingAudio::Ignore => {
            if marker.is_some() {
                warn!(
                    "Resumed meeting's earlier audio is missing; recovering only the new audio into {}",
                    output_path_str
                );
            } else if current.is_some() {
                info!(
                    "Rebuilding {} from checkpoints (existing file not trusted)",
                    output_path_str
                );
            }
        }
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
    fn existing_audio_is_classified_by_resume_marker() {
        let prior = AudioFingerprint {
            len: 1_000,
            modified_nanos: 5,
        };
        let merged = AudioFingerprint {
            len: 2_000,
            modified_nanos: 9,
        };

        // Not a resume: never trust audio.mp4, however new it is
        assert_eq!(
            classify_existing_audio(None, Some(&merged)),
            ExistingAudio::Ignore
        );
        assert_eq!(classify_existing_audio(None, None), ExistingAudio::Ignore);
        // Resume, audio.mp4 untouched: append after it
        assert_eq!(
            classify_existing_audio(Some(&prior), Some(&prior)),
            ExistingAudio::Prepend
        );
        // Resume, finalize's rename already replaced it
        assert_eq!(
            classify_existing_audio(Some(&prior), Some(&merged)),
            ExistingAudio::AlreadyMerged
        );
        // Resume, earlier audio gone
        assert_eq!(
            classify_existing_audio(Some(&prior), None),
            ExistingAudio::Ignore
        );
    }

    #[test]
    fn new_resuming_detects_existing_audio() {
        let temp_dir = tempdir().unwrap();
        let meeting_folder = temp_dir.path().join("Resumed_Meeting");
        std::fs::create_dir_all(meeting_folder.join(".checkpoints")).unwrap();
        std::fs::write(meeting_folder.join("audio.mp4"), b"prior").unwrap();

        let saver = IncrementalAudioSaver::new_resuming(meeting_folder.clone(), 48000).unwrap();
        assert_eq!(saver.prior_audio, Some(meeting_folder.join("audio.mp4")));
        let marker: AudioFingerprint = serde_json::from_slice(
            &std::fs::read(
                meeting_folder
                    .join(".checkpoints")
                    .join(PRIOR_AUDIO_MARKER_FILE),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            marker,
            AudioFingerprint::of(&meeting_folder.join("audio.mp4")).unwrap()
        );

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
        assert!(!meeting_folder.join(MERGED_AUDIO_TEMP_FILE).exists());
        assert!(!meeting_folder.join(".checkpoints").exists());
    }

    /// Record `chunks` checkpoints into the folder's .checkpoints without finalizing,
    /// as if the app died mid-recording.
    fn record_without_finalize(saver: &mut IncrementalAudioSaver, chunks: u64) {
        saver.checkpoint_interval_samples = 24000; // one checkpoint per chunk
        for i in 0..chunks {
            saver.add_chunk(silent_chunk(i)).unwrap();
        }
    }

    #[tokio::test]
    async fn recovery_rebuilds_partial_audio_of_a_plain_meeting() {
        if find_ffmpeg_path().is_none() {
            eprintln!("FFmpeg not available, skipping");
            return;
        }

        let temp_dir = tempdir().unwrap();
        let meeting_folder = temp_dir.path().join("Plain Meeting");
        std::fs::create_dir_all(meeting_folder.join(".checkpoints")).unwrap();

        let mut saver = IncrementalAudioSaver::new(meeting_folder.clone(), 48000).unwrap();
        record_without_finalize(&mut saver, 3);
        // An interrupted finalize from an older build left a truncated audio.mp4,
        // newer than every checkpoint
        std::fs::write(meeting_folder.join("audio.mp4"), b"truncated").unwrap();

        let status =
            recover_audio_from_checkpoints(meeting_folder.to_string_lossy().to_string(), 48000)
                .await
                .unwrap();

        assert_eq!(status.status, "success");
        assert_ne!(status.message, "Audio was already merged");
        assert_ne!(
            std::fs::read(meeting_folder.join("audio.mp4")).unwrap(),
            b"truncated"
        );
        assert!(!meeting_folder.join(MERGED_AUDIO_TEMP_FILE).exists());
    }

    #[tokio::test]
    async fn recovery_appends_crashed_resume_after_prior_audio() {
        if find_ffmpeg_path().is_none() {
            eprintln!("FFmpeg not available, skipping");
            return;
        }

        let temp_dir = tempdir().unwrap();
        let meeting_folder = temp_dir.path().join("Resumed Meeting");
        std::fs::create_dir_all(meeting_folder.join(".checkpoints")).unwrap();

        let mut first = IncrementalAudioSaver::new(meeting_folder.clone(), 48000).unwrap();
        record_without_finalize(&mut first, 2);
        let audio_path = first.finalize().await.unwrap();
        let prior_size = std::fs::metadata(&audio_path).unwrap().len();

        // Resume, then crash before finalize
        std::fs::create_dir_all(meeting_folder.join(".checkpoints")).unwrap();
        let mut resumed =
            IncrementalAudioSaver::new_resuming(meeting_folder.clone(), 48000).unwrap();
        record_without_finalize(&mut resumed, 2);

        let status =
            recover_audio_from_checkpoints(meeting_folder.to_string_lossy().to_string(), 48000)
                .await
                .unwrap();

        assert_eq!(status.status, "success");
        assert_ne!(status.message, "Audio was already merged");
        assert!(std::fs::metadata(&audio_path).unwrap().len() > prior_size);
    }

    #[tokio::test]
    async fn recovery_keeps_resumed_audio_that_finalize_already_merged() {
        if find_ffmpeg_path().is_none() {
            eprintln!("FFmpeg not available, skipping");
            return;
        }

        let temp_dir = tempdir().unwrap();
        let meeting_folder = temp_dir.path().join("Resumed Meeting");
        std::fs::create_dir_all(meeting_folder.join(".checkpoints")).unwrap();

        let mut first = IncrementalAudioSaver::new(meeting_folder.clone(), 48000).unwrap();
        record_without_finalize(&mut first, 2);
        first.finalize().await.unwrap();

        std::fs::create_dir_all(meeting_folder.join(".checkpoints")).unwrap();
        let mut resumed =
            IncrementalAudioSaver::new_resuming(meeting_folder.clone(), 48000).unwrap();
        record_without_finalize(&mut resumed, 2);
        // Finalize's merge + rename landed, then the app died before cleanup
        resumed
            .merge_checkpoints(&meeting_folder.join("audio.mp4"))
            .await
            .unwrap();
        let merged = std::fs::read(meeting_folder.join("audio.mp4")).unwrap();

        let status =
            recover_audio_from_checkpoints(meeting_folder.to_string_lossy().to_string(), 48000)
                .await
                .unwrap();

        assert_eq!(status.status, "success");
        assert_eq!(status.message, "Audio was already merged");
        assert_eq!(
            std::fs::read(meeting_folder.join("audio.mp4")).unwrap(),
            merged
        );
    }

    #[tokio::test]
    async fn recovery_leaves_everything_when_resume_marker_is_unreadable() {
        let temp_dir = tempdir().unwrap();
        let meeting_folder = temp_dir.path().join("Resumed Meeting");
        let checkpoints = meeting_folder.join(".checkpoints");
        std::fs::create_dir_all(&checkpoints).unwrap();
        std::fs::write(meeting_folder.join("audio.mp4"), b"prior").unwrap();
        std::fs::write(checkpoints.join("audio_chunk_000.mp4"), b"chunk").unwrap();
        std::fs::write(checkpoints.join(PRIOR_AUDIO_MARKER_FILE), b"not json").unwrap();

        let status =
            recover_audio_from_checkpoints(meeting_folder.to_string_lossy().to_string(), 48000)
                .await
                .unwrap();

        assert_eq!(status.status, "failed");
        assert_eq!(
            std::fs::read(meeting_folder.join("audio.mp4")).unwrap(),
            b"prior"
        );
        assert!(checkpoints.join("audio_chunk_000.mp4").exists());
    }
}
