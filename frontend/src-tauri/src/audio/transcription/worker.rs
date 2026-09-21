// audio/transcription/worker.rs
//
// Parallel transcription worker pool and chunk processing logic.

use super::dedup::{DedupVerdict, TranscriptDeduper};
use super::engine::TranscriptionEngine;
use super::provider::TranscriptionError;
use crate::audio::{AudioChunk, RecordingDeviceType};
use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Runtime};

// Sequence counter for transcript updates
static SEQUENCE_COUNTER: AtomicU64 = AtomicU64::new(0);

// Speech detection flag - reset per recording session
static SPEECH_DETECTED_EMITTED: AtomicBool = AtomicBool::new(false);

/// Reset the speech detected flag for a new recording session
pub fn reset_speech_detected_flag() {
    SPEECH_DETECTED_EMITTED.store(false, Ordering::SeqCst);
    info!(
        "🔍 SPEECH_DETECTED_EMITTED reset to: {}",
        SPEECH_DETECTED_EMITTED.load(Ordering::SeqCst)
    );
}

/// Returns true if the transcript text is non-trivial and should be emitted.
/// Filters empty/whitespace-only text; no confidence gating is applied.
fn should_emit_transcript(text: &str) -> bool {
    !text.trim().is_empty()
}

fn source_label(device_type: &RecordingDeviceType) -> &'static str {
    match device_type {
        RecordingDeviceType::Microphone => "mic",
        RecordingDeviceType::System => "system",
    }
}

/// Consult the session's duplicate detector for one transcribed candidate and,
/// only when it is genuinely new, draw the next sequence id for it.
///
/// This is the whole emit-vs-skip decision pulled out of the async worker loop
/// so it can be unit tested without the worker pool: `Some(id)` means "build
/// and emit a `TranscriptUpdate` with this sequence id"; `None` means the
/// candidate was a duplicate, which this function has already logged (with the
/// text, its window and the source it came from) and recorded nothing new for
/// - the caller must not burn a sequence id or emit anything for it.
fn next_sequence_id_if_not_duplicate(
    deduper: &mut TranscriptDeduper,
    sequence_counter: &AtomicU64,
    text: &str,
    source: &str,
    audio_start_time: f64,
    audio_end_time: f64,
) -> Option<u64> {
    match deduper.check_and_record(text, audio_start_time, audio_end_time) {
        DedupVerdict::Emit => Some(sequence_counter.fetch_add(1, Ordering::SeqCst)),
        DedupVerdict::Duplicate {
            previous_start,
            previous_end,
        } => {
            info!(
                "🔁 Skipping duplicate transcript from {} source: \"{}\" [{:.2}s-{:.2}s] overlaps already-emitted [{:.2}s-{:.2}s]",
                source, text, audio_start_time, audio_end_time, previous_start, previous_end
            );
            None
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TranscriptUpdate {
    pub text: String,
    pub timestamp: String, // Wall-clock time for reference (e.g., "14:30:05")
    pub source: String,
    pub sequence_id: u64,
    pub chunk_start_time: f64, // Legacy field, kept for compatibility
    pub is_partial: bool,
    pub confidence: f32,
    // NEW: Recording-relative timestamps for playback sync
    pub audio_start_time: f64, // Seconds from recording start (e.g., 125.3)
    pub audio_end_time: f64,   // Seconds from recording start (e.g., 128.6)
    pub duration: f64,         // Segment duration in seconds (e.g., 3.3)
}

// NOTE: get_transcript_history and get_recording_meeting_name functions
// have been moved to recording_commands.rs where they have access to RECORDING_MANAGER

/// Optimized parallel transcription task ensuring ZERO chunk loss
pub fn start_transcription_task<R: Runtime>(
    app: AppHandle<R>,
    transcription_receiver: tokio::sync::mpsc::UnboundedReceiver<AudioChunk>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        info!("🚀 Starting optimized parallel transcription task - guaranteeing zero chunk loss");

        // Initialize transcription engine (Whisper or Parakeet based on config)
        let transcription_engine = match super::engine::get_or_init_transcription_engine(&app).await
        {
            Ok(engine) => engine,
            Err(e) => {
                error!("Failed to initialize transcription engine: {}", e);
                let _ = app.emit("transcription-error", serde_json::json!({
                    "error": e,
                    "userMessage": "Recording failed: Unable to initialize speech recognition. Please check your model settings.",
                    "actionable": true,
                    "phase": "active"
                }));
                return;
            }
        };

        // Create parallel workers for faster processing while preserving ALL chunks
        const NUM_WORKERS: usize = 1; // Serial processing ensures transcripts emit in chronological order
        let (work_sender, work_receiver) = tokio::sync::mpsc::unbounded_channel::<AudioChunk>();
        let work_receiver = Arc::new(tokio::sync::Mutex::new(work_receiver));

        // Track completion: AtomicU64 for chunks queued, AtomicU64 for chunks completed
        let chunks_queued = Arc::new(AtomicU64::new(0));
        let chunks_completed = Arc::new(AtomicU64::new(0));
        let input_finished = Arc::new(AtomicBool::new(false));

        // Session-scoped duplicate detector. `start_transcription_task` is
        // called exactly once per recording session (from both start_recording
        // paths and from the enable-live-transcription-mid-recording path in
        // recording_commands.rs), so a fresh instance created here starts every
        // session on a clean timeline with no explicit reset wiring needed.
        // Wrapped for sharing across the worker pool below; the lock is only
        // ever held for the duration of one `check_and_record` call, never
        // across an `.await` that transcribes.
        let deduper = Arc::new(tokio::sync::Mutex::new(TranscriptDeduper::new()));

        info!(
            "📊 Starting {} transcription worker{} (serial mode for ordered emission)",
            NUM_WORKERS,
            if NUM_WORKERS == 1 { "" } else { "s" }
        );

        // Spawn worker tasks
        let mut worker_handles = Vec::new();
        for worker_id in 0..NUM_WORKERS {
            let engine_clone = match &transcription_engine {
                TranscriptionEngine::Whisper(e) => TranscriptionEngine::Whisper(e.clone()),
                TranscriptionEngine::Parakeet(e) => TranscriptionEngine::Parakeet(e.clone()),
                TranscriptionEngine::Provider(p) => TranscriptionEngine::Provider(p.clone()),
            };
            let app_clone = app.clone();
            let work_receiver_clone = work_receiver.clone();
            let chunks_completed_clone = chunks_completed.clone();
            let input_finished_clone = input_finished.clone();
            let chunks_queued_clone = chunks_queued.clone();
            let deduper_clone = deduper.clone();

            let worker_handle = tokio::spawn(async move {
                info!("👷 Worker {} started", worker_id);

                // PRE-VALIDATE model state to avoid repeated async calls per chunk
                let initial_model_loaded = engine_clone.is_model_loaded().await;
                let current_model = engine_clone
                    .get_current_model()
                    .await
                    .unwrap_or_else(|| "unknown".to_string());

                let engine_name = engine_clone.provider_name();

                if initial_model_loaded {
                    info!(
                        "✅ Worker {} pre-validation: {} model '{}' is loaded and ready",
                        worker_id, engine_name, current_model
                    );
                } else {
                    warn!(
                        "⚠️ Worker {} pre-validation: {} model not loaded - chunks may be skipped",
                        worker_id, engine_name
                    );
                }

                loop {
                    // Try to get a chunk to process
                    let chunk = {
                        let mut receiver = work_receiver_clone.lock().await;
                        receiver.recv().await
                    };

                    match chunk {
                        Some(chunk) => {
                            // PERFORMANCE OPTIMIZATION: Reduce logging in hot path
                            // Only log every 10th chunk per worker to reduce I/O overhead
                            let should_log_this_chunk = chunk.chunk_id % 10 == 0;

                            if should_log_this_chunk {
                                info!(
                                    "👷 Worker {} processing chunk {} with {} samples",
                                    worker_id,
                                    chunk.chunk_id,
                                    chunk.data.len()
                                );
                            }

                            // Check if model is still loaded before processing
                            if !engine_clone.is_model_loaded().await {
                                warn!("⚠️ Worker {}: Model unloaded, but continuing to preserve chunk {}", worker_id, chunk.chunk_id);
                                // Still count as completed even if we can't process
                                chunks_completed_clone.fetch_add(1, Ordering::SeqCst);
                                continue;
                            }

                            let chunk_timestamp = chunk.timestamp;
                            let chunk_duration = chunk.data.len() as f64 / chunk.sample_rate as f64;
                            let source = source_label(&chunk.device_type);

                            // Transcribe with provider-agnostic approach
                            match transcribe_chunk_with_provider(&engine_clone, chunk, &app_clone)
                                .await
                            {
                                Ok((transcript, confidence_opt, is_partial)) => {
                                    let confidence_str = match confidence_opt {
                                        Some(c) => format!("{:.2}", c),
                                        None => "N/A".to_string(),
                                    };

                                    info!("🔍 Worker {} transcription result: text='{}', confidence={}, partial={}",
                worker_id, transcript, confidence_str, is_partial);

                                    if should_emit_transcript(&transcript) {
                                        // PERFORMANCE: Only log transcription results, not every processing step
                                        info!("✅ Worker {} transcribed: {} (confidence: {}, partial: {})",
                                              worker_id, transcript, confidence_str, is_partial);

                                        // Emit speech-detected event for frontend UX (only on first detection per session)
                                        // This is lightweight and provides better user feedback.
                                        //
                                        // Deliberately runs ahead of the duplicate check below and is never
                                        // gated on its result: a duplicate segment is still real speech (an
                                        // echo of speech that reached both the mic and system paths, not
                                        // silence), so suppressing this latch on a duplicate would regress the
                                        // UX whenever the very first utterance of a session happens to arrive
                                        // twice.
                                        let current_flag =
                                            SPEECH_DETECTED_EMITTED.load(Ordering::SeqCst);
                                        info!("🔍 Checking speech-detected flag: current={}, will_emit={}", current_flag, !current_flag);

                                        if !current_flag {
                                            SPEECH_DETECTED_EMITTED.store(true, Ordering::SeqCst);
                                            match app_clone.emit("speech-detected", serde_json::json!({
                                                "message": "Speech activity detected"
                                            })) {
                                                Ok(_) => info!("🎤 ✅ First speech detected - successfully emitted speech-detected event"),
                                                Err(e) => error!("🎤 ❌ Failed to emit speech-detected event: {}", e),
                                            }
                                        } else {
                                            info!("🔍 Speech already detected in this session, not re-emitting");
                                        }

                                        // Calculate timestamps FIRST - the duplicate check below needs the
                                        // audio window before a sequence id is ever drawn for it.
                                        let audio_start_time = chunk_timestamp; // Already in seconds from recording start
                                        let audio_end_time = chunk_timestamp + chunk_duration;

                                        // Consult the session-scoped duplicate detector before allocating a
                                        // sequence id or emitting anything. The mixing pipeline runs one VAD
                                        // per capture source over the same aligned windows, so the same speech
                                        // reaching both the mic and system paths (or an overlapping VAD
                                        // force-flush on one source) can transcribe to matching text on
                                        // overlapping windows. Skip that echo entirely here - no event, no
                                        // sequence id - rather than let it through and rely on the UI/recording
                                        // saver's sequence-id-only dedup, which cannot tell it apart from a
                                        // genuinely new segment.
                                        let sequence_id = {
                                            let mut deduper = deduper_clone.lock().await;
                                            next_sequence_id_if_not_duplicate(
                                                &mut deduper,
                                                &SEQUENCE_COUNTER,
                                                &transcript,
                                                source,
                                                audio_start_time,
                                                audio_end_time,
                                            )
                                        };

                                        if let Some(sequence_id) = sequence_id {
                                            // Save structured transcript segment to recording manager (only final results)
                                            // Save ALL segments (partial and final) to ensure complete JSON
                                            // Create structured segment with full timestamp data
                                            // NOTE: This is now handled via the transcript-update event emission below
                                            // The recording_commands module listens to these events and saves them
                                            // This decouples the transcription worker from direct RECORDING_MANAGER access

                                            // Emit transcript update with NEW recording-relative timestamps

                                            let update = TranscriptUpdate {
                                                text: transcript,
                                                timestamp: format_current_timestamp(), // Wall-clock for reference
                                                source: source.to_string(),
                                                sequence_id,
                                                chunk_start_time: chunk_timestamp, // Legacy compatibility
                                                is_partial,
                                                confidence: confidence_opt.unwrap_or(0.85), // Default for providers without confidence
                                                // NEW: Recording-relative timestamps for sync
                                                audio_start_time,
                                                audio_end_time,
                                                duration: chunk_duration,
                                            };

                                            if let Err(e) =
                                                app_clone.emit("transcript-update", &update)
                                            {
                                                error!(
                                                    "Worker {}: Failed to emit transcript update: {}",
                                                    worker_id, e
                                                );
                                            }
                                            // PERFORMANCE: Removed verbose logging of every emission
                                        }
                                        // else: duplicate segment. next_sequence_id_if_not_duplicate() already
                                        // logged it; nothing more to do. The chunk still falls through to the
                                        // chunks_completed increment below, so completed/queued accounting
                                        // stays exact and never trips transcript-chunk-loss-detected.
                                    }
                                }
                                Err(e) => {
                                    // Improved error handling with specific cases
                                    match e {
                                        TranscriptionError::AudioTooShort { .. } => {
                                            // Skip silently, this is expected for very short chunks
                                            info!("Worker {}: {}", worker_id, e);
                                            chunks_completed_clone.fetch_add(1, Ordering::SeqCst);
                                            continue;
                                        }
                                        TranscriptionError::ModelNotLoaded => {
                                            warn!(
                                                "Worker {}: Model unloaded during transcription",
                                                worker_id
                                            );
                                            chunks_completed_clone.fetch_add(1, Ordering::SeqCst);
                                            continue;
                                        }
                                        _ => {
                                            warn!(
                                                "Worker {}: Transcription failed: {}",
                                                worker_id, e
                                            );
                                            let _ = app_clone
                                                .emit("transcription-warning", e.to_string());
                                        }
                                    }
                                }
                            }

                            // Mark chunk as completed
                            let completed =
                                chunks_completed_clone.fetch_add(1, Ordering::SeqCst) + 1;
                            let queued = chunks_queued_clone.load(Ordering::SeqCst);

                            // PERFORMANCE: Only log progress every 5th chunk to reduce I/O overhead
                            if completed % 5 == 0 || should_log_this_chunk {
                                info!(
                                    "Worker {}: Progress {}/{} chunks ({:.1}%)",
                                    worker_id,
                                    completed,
                                    queued,
                                    (completed as f64 / queued.max(1) as f64 * 100.0)
                                );
                            }

                            // Emit progress event for frontend
                            let progress_percentage = if queued > 0 {
                                (completed as f64 / queued as f64 * 100.0) as u32
                            } else {
                                100
                            };

                            let _ = app_clone.emit("transcription-progress", serde_json::json!({
                                "worker_id": worker_id,
                                "chunks_completed": completed,
                                "chunks_queued": queued,
                                "progress_percentage": progress_percentage,
                                "message": format!("Worker {} processing... ({}/{})", worker_id, completed, queued)
                            }));
                        }
                        None => {
                            // No more chunks available
                            if input_finished_clone.load(Ordering::SeqCst) {
                                // Double-check that all queued chunks are actually completed
                                let final_queued = chunks_queued_clone.load(Ordering::SeqCst);
                                let final_completed = chunks_completed_clone.load(Ordering::SeqCst);

                                if final_completed >= final_queued {
                                    info!(
                                        "👷 Worker {} finishing - all {}/{} chunks processed",
                                        worker_id, final_completed, final_queued
                                    );
                                    break;
                                } else {
                                    warn!("👷 Worker {} detected potential chunk loss: {}/{} completed, waiting...", worker_id, final_completed, final_queued);
                                    // AGGRESSIVE POLLING: Reduced from 50ms to 5ms for faster chunk detection during shutdown
                                    tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
                                }
                            } else {
                                // AGGRESSIVE POLLING: Reduced from 10ms to 1ms for faster response during shutdown
                                tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
                            }
                        }
                    }
                }

                info!("👷 Worker {} completed", worker_id);
            });

            worker_handles.push(worker_handle);
        }

        // Main dispatcher: receive chunks and distribute to workers
        let mut receiver = transcription_receiver;
        while let Some(chunk) = receiver.recv().await {
            let queued = chunks_queued.fetch_add(1, Ordering::SeqCst) + 1;
            info!(
                "📥 Dispatching chunk {} to workers (total queued: {})",
                chunk.chunk_id, queued
            );

            if let Err(_) = work_sender.send(chunk) {
                error!("❌ Failed to send chunk to workers - this should not happen!");
                break;
            }
        }

        // Signal that input is finished
        input_finished.store(true, Ordering::SeqCst);
        drop(work_sender); // Close the channel to signal workers

        let total_chunks_queued = chunks_queued.load(Ordering::SeqCst);
        info!("📭 Input finished with {} total chunks queued. Waiting for all {} workers to complete...",
              total_chunks_queued, NUM_WORKERS);

        // Emit final chunk count to frontend
        let _ = app.emit("transcription-queue-complete", serde_json::json!({
            "total_chunks": total_chunks_queued,
            "message": format!("{} chunks queued for processing - waiting for completion", total_chunks_queued)
        }));

        // Wait for all workers to complete
        for (worker_id, handle) in worker_handles.into_iter().enumerate() {
            if let Err(e) = handle.await {
                error!("❌ Worker {} panicked: {:?}", worker_id, e);
            } else {
                info!("✅ Worker {} completed successfully", worker_id);
            }
        }

        // Final verification with retry logic to catch any stragglers
        let mut verification_attempts = 0;
        const MAX_VERIFICATION_ATTEMPTS: u32 = 10;

        loop {
            let final_queued = chunks_queued.load(Ordering::SeqCst);
            let final_completed = chunks_completed.load(Ordering::SeqCst);

            if final_queued == final_completed {
                info!(
                    "🎉 ALL {} chunks processed successfully - ZERO chunks lost!",
                    final_completed
                );
                break;
            } else if verification_attempts < MAX_VERIFICATION_ATTEMPTS {
                verification_attempts += 1;
                warn!("⚠️ Chunk count mismatch (attempt {}): {} queued, {} completed - waiting for stragglers...",
                     verification_attempts, final_queued, final_completed);

                // Wait a bit for any remaining chunks to be processed
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            } else {
                error!(
                    "❌ CRITICAL: After {} attempts, chunk loss detected: {} queued, {} completed",
                    MAX_VERIFICATION_ATTEMPTS, final_queued, final_completed
                );

                // Emit critical error event
                let _ = app.emit(
                    "transcript-chunk-loss-detected",
                    serde_json::json!({
                        "chunks_queued": final_queued,
                        "chunks_completed": final_completed,
                        "chunks_lost": final_queued - final_completed,
                        "message": "Some transcript chunks may have been lost during shutdown"
                    }),
                );
                break;
            }
        }

        info!("✅ Parallel transcription task completed - all workers finished, ready for model unload");
    })
}

/// Transcribe audio chunk using the appropriate provider (Whisper, Parakeet, or trait-based)
/// Returns: (text, confidence Option, is_partial)
async fn transcribe_chunk_with_provider<R: Runtime>(
    engine: &TranscriptionEngine,
    chunk: AudioChunk,
    app: &AppHandle<R>,
) -> std::result::Result<(String, Option<f32>, bool), TranscriptionError> {
    // Convert to 16kHz mono for transcription
    let transcription_data = if chunk.sample_rate != 16000 {
        crate::audio::audio_processing::resample_audio(&chunk.data, chunk.sample_rate, 16000)
    } else {
        chunk.data
    };

    // Skip VAD processing here since the pipeline already extracted speech using VAD
    let speech_samples = transcription_data;

    // Check for empty samples - improved error handling
    if speech_samples.is_empty() {
        warn!(
            "Audio chunk {} is empty, skipping transcription",
            chunk.chunk_id
        );
        return Err(TranscriptionError::AudioTooShort {
            samples: 0,
            minimum: 1600, // 100ms at 16kHz
        });
    }

    // Calculate energy for logging/monitoring only
    let energy: f32 =
        speech_samples.iter().map(|&x| x * x).sum::<f32>() / speech_samples.len() as f32;
    info!(
        "Processing speech audio chunk {} with {} samples (energy: {:.6})",
        chunk.chunk_id,
        speech_samples.len(),
        energy
    );

    // Transcribe using the appropriate engine (with improved error handling)
    match engine {
        TranscriptionEngine::Whisper(whisper_engine) => {
            // Get language preference from global state
            let language = crate::get_language_preference_internal();

            match whisper_engine
                .transcribe_audio_with_confidence(speech_samples, language)
                .await
            {
                Ok((text, confidence, is_partial)) => {
                    let cleaned_text = text.trim().to_string();
                    if cleaned_text.is_empty() {
                        return Ok((String::new(), Some(confidence), is_partial));
                    }

                    info!(
                        "Whisper transcription complete for chunk {}: '{}' (confidence: {:.2}, partial: {})",
                        chunk.chunk_id, cleaned_text, confidence, is_partial
                    );

                    Ok((cleaned_text, Some(confidence), is_partial))
                }
                Err(e) => {
                    error!(
                        "Whisper transcription failed for chunk {}: {}",
                        chunk.chunk_id, e
                    );

                    let transcription_error = TranscriptionError::EngineFailed(e.to_string());
                    let _ = app.emit(
                        "transcription-error",
                        &serde_json::json!({
                            "error": transcription_error.to_string(),
                            "userMessage": format!("Transcription failed: {}", transcription_error),
                            "actionable": false,
                            "phase": "active"
                        }),
                    );

                    Err(transcription_error)
                }
            }
        }
        TranscriptionEngine::Parakeet(parakeet_engine) => {
            match parakeet_engine.transcribe_audio(speech_samples).await {
                Ok(text) => {
                    let cleaned_text = text.trim().to_string();
                    if cleaned_text.is_empty() {
                        return Ok((String::new(), None, false));
                    }

                    info!(
                        "Parakeet transcription complete for chunk {}: '{}'",
                        chunk.chunk_id, cleaned_text
                    );

                    // Parakeet doesn't provide confidence or partial results
                    Ok((cleaned_text, None, false))
                }
                Err(e) => {
                    error!(
                        "Parakeet transcription failed for chunk {}: {}",
                        chunk.chunk_id, e
                    );

                    let transcription_error = TranscriptionError::EngineFailed(e.to_string());
                    let _ = app.emit(
                        "transcription-error",
                        &serde_json::json!({
                            "error": transcription_error.to_string(),
                            "userMessage": format!("Transcription failed: {}", transcription_error),
                            "actionable": false,
                            "phase": "active"
                        }),
                    );

                    Err(transcription_error)
                }
            }
        }
        TranscriptionEngine::Provider(provider) => {
            // NEW: Trait-based provider (clean, unified interface)
            let language = crate::get_language_preference_internal();

            match provider.transcribe(speech_samples, language).await {
                Ok(result) => {
                    let cleaned_text = result.text.trim().to_string();
                    if cleaned_text.is_empty() {
                        return Ok((String::new(), result.confidence, result.is_partial));
                    }

                    let confidence_str = match result.confidence {
                        Some(c) => format!("confidence: {:.2}", c),
                        None => "no confidence".to_string(),
                    };

                    info!(
                        "{} transcription complete for chunk {}: '{}' ({}, partial: {})",
                        provider.provider_name(),
                        chunk.chunk_id,
                        cleaned_text,
                        confidence_str,
                        result.is_partial
                    );

                    Ok((cleaned_text, result.confidence, result.is_partial))
                }
                Err(e) => {
                    error!(
                        "{} transcription failed for chunk {}: {}",
                        provider.provider_name(),
                        chunk.chunk_id,
                        e
                    );

                    let _ = app.emit(
                        "transcription-error",
                        &serde_json::json!({
                            "error": e.to_string(),
                            "userMessage": format!("Transcription failed: {}", e),
                            "actionable": false,
                            "phase": "active"
                        }),
                    );

                    Err(e)
                }
            }
        }
    }
}

/// Format current timestamp (wall-clock time)
fn format_current_timestamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();

    let hours = (now.as_secs() / 3600) % 24;
    let minutes = (now.as_secs() / 60) % 60;
    let seconds = now.as_secs() % 60;

    format!("{:02}:{:02}:{:02}", hours, minutes, seconds)
}

/// Format recording-relative time as [MM:SS]
#[allow(dead_code)]
fn format_recording_time(seconds: f64) -> String {
    let total_seconds = seconds.floor() as u64;
    let minutes = total_seconds / 60;
    let secs = total_seconds % 60;

    format!("[{:02}:{:02}]", minutes, secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_short_acknowledgements() {
        assert!(should_emit_transcript("Yes"));
        assert!(should_emit_transcript("ok"));
    }

    #[test]
    fn drops_empty_and_whitespace_only() {
        assert!(!should_emit_transcript(""));
        assert!(!should_emit_transcript("   "));
    }

    #[test]
    fn labels_capture_sources_for_first_layer_diarization() {
        assert_eq!(source_label(&RecordingDeviceType::Microphone), "mic");
        assert_eq!(source_label(&RecordingDeviceType::System), "system");
    }

    #[test]
    fn first_candidate_is_emitted_with_a_sequence_id() {
        let mut deduper = TranscriptDeduper::new();
        let counter = AtomicU64::new(0);

        let id = next_sequence_id_if_not_duplicate(
            &mut deduper,
            &counter,
            "let's get started",
            "mic",
            10.0,
            12.5,
        );

        assert_eq!(id, Some(0));
    }

    #[test]
    fn mic_and_system_echo_of_the_same_speech_is_skipped() {
        // The PRA-468 bug: mic and system VADs both see the same utterance
        // over the same aligned window and each hands the worker its own
        // segment. The second one must be skipped, not given a sequence id.
        let mut deduper = TranscriptDeduper::new();
        let counter = AtomicU64::new(0);

        let mic_id = next_sequence_id_if_not_duplicate(
            &mut deduper,
            &counter,
            "let's get started",
            "mic",
            10.0,
            12.5,
        );
        let system_id = next_sequence_id_if_not_duplicate(
            &mut deduper,
            &counter,
            "let's get started",
            "system",
            10.0,
            12.5,
        );

        assert_eq!(mic_id, Some(0));
        assert_eq!(system_id, None, "the overlapping echo must not be emitted");
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "a skipped duplicate must not consume a sequence id"
        );
    }

    #[test]
    fn identical_text_at_a_later_non_overlapping_window_still_emits() {
        // A phrase genuinely repeated later in the meeting is not a
        // duplicate - only overlapping windows are.
        let mut deduper = TranscriptDeduper::new();
        let counter = AtomicU64::new(0);

        let first = next_sequence_id_if_not_duplicate(
            &mut deduper,
            &counter,
            "sounds good",
            "mic",
            5.0,
            6.0,
        );
        let second = next_sequence_id_if_not_duplicate(
            &mut deduper,
            &counter,
            "sounds good",
            "mic",
            20.0,
            21.0,
        );

        assert_eq!(first, Some(0));
        assert_eq!(second, Some(1));
    }

    #[test]
    fn different_text_over_the_same_window_both_emit() {
        let mut deduper = TranscriptDeduper::new();
        let counter = AtomicU64::new(0);

        let mic = next_sequence_id_if_not_duplicate(
            &mut deduper,
            &counter,
            "hello there",
            "mic",
            4.0,
            6.0,
        );
        let system = next_sequence_id_if_not_duplicate(
            &mut deduper,
            &counter,
            "general kenobi",
            "system",
            4.0,
            6.0,
        );

        assert_eq!(mic, Some(0));
        assert_eq!(system, Some(1));
    }

    #[test]
    fn sequence_ids_are_not_reused_after_a_skip() {
        let mut deduper = TranscriptDeduper::new();
        let counter = AtomicU64::new(0);

        let a = next_sequence_id_if_not_duplicate(&mut deduper, &counter, "one", "mic", 0.0, 1.0);
        let dup =
            next_sequence_id_if_not_duplicate(&mut deduper, &counter, "one", "system", 0.0, 1.0);
        let b = next_sequence_id_if_not_duplicate(&mut deduper, &counter, "two", "mic", 2.0, 3.0);

        assert_eq!(a, Some(0));
        assert_eq!(dup, None);
        assert_eq!(b, Some(1), "the next real segment must not skip ahead to 2");
    }
}
