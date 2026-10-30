use anyhow::Result;
use chrono::Utc;
use log::{debug, info, warn};
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use std::path::{Path, PathBuf};

/// Sanitize a filename to be safe for filesystem use
pub fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect::<String>()
        .trim()
        .to_string()
}

/// Create a meeting folder with timestamp and return the path
/// Creates structure: base_path/MeetingName_YYYY-MM-DD_HH-MM/
///                    ├── .checkpoints/  (for incremental saves, optional)
///
/// # Arguments
/// * `base_path` - Base directory for meetings
/// * `meeting_name` - Name of the meeting
/// * `create_checkpoints_dir` - Whether to create .checkpoints/ subdirectory (only needed when auto_save is true)
pub fn create_meeting_folder(
    base_path: &Path,
    meeting_name: &str,
    create_checkpoints_dir: bool,
) -> Result<PathBuf> {
    let timestamp = Utc::now().format("%Y-%m-%d_%H-%M").to_string();
    let sanitized_name = sanitize_filename(meeting_name);
    let folder_name = format!("{}_{}", sanitized_name, timestamp);

    // Keep debug recordings out of the real recordings folder.
    let base = if crate::debug_mode::is_enabled() {
        base_path.join("debug")
    } else {
        base_path.to_path_buf()
    };
    let meeting_folder = base.join(folder_name);

    // Create main meeting folder
    std::fs::create_dir_all(&meeting_folder)?;

    // Only create .checkpoints subdirectory if requested (when auto_save is true)
    if create_checkpoints_dir {
        let checkpoints_dir = meeting_folder.join(".checkpoints");
        std::fs::create_dir_all(&checkpoints_dir)?;
        log::info!(
            "Created meeting folder with checkpoints: {}",
            meeting_folder.display()
        );
    } else {
        log::info!(
            "Created meeting folder without checkpoints: {}",
            meeting_folder.display()
        );
    }

    Ok(meeting_folder)
}

/// True peak limiter with lookahead buffer (prevents clipping)
struct TruePeakLimiter {
    lookahead_samples: usize,
    buffer: Vec<f32>,
    gain_reduction: Vec<f32>,
    current_position: usize,
}

impl TruePeakLimiter {
    fn new(sample_rate: u32) -> Self {
        const LIMITER_LOOKAHEAD_MS: usize = 10;
        let lookahead_samples = ((sample_rate as usize * LIMITER_LOOKAHEAD_MS) / 1000).max(1);

        Self {
            lookahead_samples,
            buffer: vec![0.0; lookahead_samples],
            gain_reduction: vec![1.0; lookahead_samples],
            current_position: 0,
        }
    }

    fn process(&mut self, sample: f32, true_peak_limit: f32) -> f32 {
        self.buffer[self.current_position] = sample;

        let sample_abs = sample.abs();
        if sample_abs > true_peak_limit {
            let reduction = true_peak_limit / sample_abs;
            self.gain_reduction[self.current_position] = reduction;
        } else {
            self.gain_reduction[self.current_position] = 1.0;
        }

        let output_position = (self.current_position + 1) % self.lookahead_samples;
        let output_sample = self.buffer[output_position] * self.gain_reduction[output_position];

        self.current_position = output_position;
        output_sample
    }
}

/// Professional loudness normalizer using EBU R128 standard
/// This is a STATEFUL normalizer that tracks cumulative loudness over time
///
/// EBU R128 is the broadcast industry standard for loudness normalization:
/// - Target: -23 LUFS (Loudness Units relative to Full Scale)
/// - Used by: Netflix, YouTube, Spotify, all professional broadcast
/// - Perceptually accurate (not just simple RMS)
///
pub struct LoudnessNormalizer {
    ebur128: ebur128::EbuR128,
    limiter: TruePeakLimiter,
    gain_linear: f32,
    loudness_buffer: Vec<f32>,
    true_peak_limit: f32,
}

impl LoudnessNormalizer {
    /// Create a new EBU R128 loudness normalizer
    ///
    /// # Arguments
    /// * `channels` - Number of audio channels (1 for mono, 2 for stereo)
    /// * `sample_rate` - Sample rate in Hz (e.g., 48000)
    pub fn new(channels: u32, sample_rate: u32) -> Result<Self> {
        const TRUE_PEAK_LIMIT: f64 = -1.0;
        const ANALYZE_CHUNK_SIZE: usize = 512;

        let ebur128 = ebur128::EbuR128::new(
            channels,
            sample_rate,
            ebur128::Mode::I | ebur128::Mode::TRUE_PEAK,
        )
        .map_err(|e| anyhow::anyhow!("Failed to create EBU R128 normalizer: {}", e))?;

        let true_peak_limit = 10_f32.powf(TRUE_PEAK_LIMIT as f32 / 20.0);

        Ok(Self {
            ebur128,
            limiter: TruePeakLimiter::new(sample_rate),
            gain_linear: 1.0,
            loudness_buffer: Vec::with_capacity(ANALYZE_CHUNK_SIZE),
            true_peak_limit,
        })
    }

    /// Normalize loudness using EBU R128 standard with true peak limiting
    ///
    /// This maintains cumulative loudness measurements across all processed audio,
    /// resulting in consistent normalization that sounds natural.
    ///
    /// Target: -23 LUFS (professional broadcast standard for speech/dialog)
    /// Applies sample-by-sample with 10ms lookahead limiter to prevent clipping
    pub fn normalize_loudness(&mut self, samples: &[f32]) -> Vec<f32> {
        if samples.is_empty() {
            return Vec::new();
        }

        const TARGET_LUFS: f64 = -23.0;
        const ANALYZE_CHUNK_SIZE: usize = 512;

        let mut normalized_samples = Vec::with_capacity(samples.len());

        for &sample in samples {
            // Accumulate samples for loudness analysis
            self.loudness_buffer.push(sample);

            // Analyze loudness every 512 samples
            if self.loudness_buffer.len() >= ANALYZE_CHUNK_SIZE {
                if let Err(e) = self.ebur128.add_frames_f32(&self.loudness_buffer) {
                    warn!("Failed to add frames to EBU R128: {}", e);
                } else {
                    // Update gain based on cumulative loudness
                    if let Ok(current_lufs) = self.ebur128.loudness_global() {
                        if current_lufs.is_finite() && current_lufs < 0.0 {
                            let gain_db = TARGET_LUFS - current_lufs;
                            self.gain_linear = 10_f32.powf(gain_db as f32 / 20.0);
                        }
                    }
                }
                self.loudness_buffer.clear();
            }

            // Apply gain and true peak limiting
            let amplified = sample * self.gain_linear;
            let limited = self.limiter.process(amplified, self.true_peak_limit);

            normalized_samples.push(limited);
        }

        normalized_samples
    }
}

/// High-pass filter to remove low-frequency rumble and noise
/// Removes frequencies below cutoff_hz (typically 80-100 Hz for speech)
pub struct HighPassFilter {
    #[allow(dead_code)]
    sample_rate: f32,
    #[allow(dead_code)]
    cutoff_hz: f32,
    // First-order IIR filter coefficients
    alpha: f32,
    prev_input: f32,
    prev_output: f32,
}

impl HighPassFilter {
    /// Create a new high-pass filter
    ///
    /// # Arguments
    /// * `sample_rate` - Audio sample rate in Hz
    /// * `cutoff_hz` - Cutoff frequency in Hz (typical: 80-100 Hz for speech)
    pub fn new(sample_rate: u32, cutoff_hz: f32) -> Self {
        let sample_rate_f = sample_rate as f32;
        let rc = 1.0 / (2.0 * std::f32::consts::PI * cutoff_hz);
        let dt = 1.0 / sample_rate_f;
        let alpha = rc / (rc + dt);

        info!(
            "Initializing high-pass filter: cutoff={}Hz @ {}Hz",
            cutoff_hz, sample_rate
        );

        Self {
            sample_rate: sample_rate_f,
            cutoff_hz,
            alpha,
            prev_input: 0.0,
            prev_output: 0.0,
        }
    }

    /// Apply high-pass filter to audio samples
    /// Uses first-order IIR (Infinite Impulse Response) filter
    pub fn process(&mut self, samples: &[f32]) -> Vec<f32> {
        let mut output = Vec::with_capacity(samples.len());

        for &sample in samples {
            // First-order high-pass IIR filter formula:
            // y[n] = alpha * (y[n-1] + x[n] - x[n-1])
            let filtered = self.alpha * (self.prev_output + sample - self.prev_input);

            self.prev_input = sample;
            self.prev_output = filtered;

            output.push(filtered);
        }

        output
    }

    /// Reset filter state (call when starting new recording)
    pub fn reset(&mut self) {
        self.prev_input = 0.0;
        self.prev_output = 0.0;
    }
}

pub fn audio_to_mono(audio: &[f32], channels: u16) -> Vec<f32> {
    let mut mono_samples = Vec::with_capacity(audio.len() / channels as usize);

    // For microphone arrays (> 2 channels), only use first 2 channels
    // Many microphone arrays have auxiliary channels for beam-forming/noise cancellation
    // that can contain anti-phase signals. Averaging all channels can cause destructive
    // interference resulting in near-zero output.
    let effective_channels = if channels > 2 { 2 } else { channels };

    // Iterate over the audio slice in chunks, each containing `channels` samples
    for chunk in audio.chunks(channels as usize) {
        // Sum only the first effective_channels (typically 1-2 for mic arrays)
        let sum: f32 = chunk.iter().take(effective_channels as usize).sum();

        // Calculate the average mono sample using effective channel count
        let mono_sample = sum / effective_channels as f32;

        // Store the computed mono sample
        mono_samples.push(mono_sample);
    }

    mono_samples
}

/// High-quality audio resampling with adaptive parameters based on sample rate ratio
///
/// This function automatically selects the best resampling parameters based on:
/// - Sample rate ratio (upsampling vs downsampling)
/// - Quality requirements (integer ratios get optimized paths)
/// - Anti-aliasing needs
///
/// Supports all common sample rates: 8kHz, 16kHz, 24kHz, 44.1kHz, 48kHz, etc.
pub fn resample(input: &[f32], from_sample_rate: u32, to_sample_rate: u32) -> Result<Vec<f32>> {
    if input.is_empty() {
        return Ok(Vec::new());
    }

    // Fast path: No resampling needed
    if from_sample_rate == to_sample_rate {
        return Ok(input.to_vec());
    }

    let ratio = to_sample_rate as f64 / from_sample_rate as f64;

    // Adaptive parameters based on sample rate ratio
    let (sinc_len, interpolation_type, oversampling) = if ratio >= 2.0 {
        // Large upsampling (e.g., 8kHz → 16kHz, 16kHz → 48kHz, 24kHz → 48kHz)
        // Needs high quality to avoid artifacts
        debug!(
            "High-quality upsampling: {}Hz → {}Hz (ratio: {:.2}x)",
            from_sample_rate, to_sample_rate, ratio
        );
        (
            512,                          // Longer sinc for smoother interpolation
            SincInterpolationType::Cubic, // Cubic for best quality
            512,                          // Higher oversampling
        )
    } else if ratio >= 1.5 {
        // Moderate upsampling (e.g., 32kHz → 48kHz)
        debug!(
            "Moderate upsampling: {}Hz → {}Hz (ratio: {:.2}x)",
            from_sample_rate, to_sample_rate, ratio
        );
        (384, SincInterpolationType::Cubic, 384)
    } else if ratio > 1.0 {
        // Small upsampling (e.g., 44.1kHz → 48kHz)
        debug!(
            "Small upsampling: {}Hz → {}Hz (ratio: {:.2}x)",
            from_sample_rate, to_sample_rate, ratio
        );
        (256, SincInterpolationType::Linear, 256)
    } else if ratio <= 0.5 {
        // Large downsampling (e.g., 48kHz → 16kHz, 48kHz → 8kHz)
        // Needs strong anti-aliasing
        debug!(
            "Anti-aliased downsampling: {}Hz → {}Hz (ratio: {:.2}x)",
            from_sample_rate, to_sample_rate, ratio
        );
        (
            512,                          // Longer sinc for anti-aliasing
            SincInterpolationType::Cubic, // Cubic for quality
            512,
        )
    } else {
        // Moderate downsampling (e.g., 48kHz → 24kHz, 48kHz → 32kHz)
        debug!(
            "Moderate downsampling: {}Hz → {}Hz (ratio: {:.2}x)",
            from_sample_rate, to_sample_rate, ratio
        );
        (384, SincInterpolationType::Linear, 384)
    };

    let params = SincInterpolationParameters {
        sinc_len,
        f_cutoff: 0.95, // Preserve most of the frequency content
        interpolation: interpolation_type,
        oversampling_factor: oversampling,
        window: WindowFunction::BlackmanHarris2, // Best window for audio
    };

    let mut resampler = SincFixedIn::<f32>::new(
        ratio,
        2.0, // Maximum relative deviation
        params,
        input.len(),
        1, // Mono
    )?;

    let waves_in = vec![input.to_vec()];
    let waves_out = resampler.process(&waves_in, None)?;

    debug!(
        "Resampling complete: {} samples → {} samples",
        input.len(),
        waves_out[0].len()
    );

    Ok(waves_out.into_iter().next().unwrap())
}

// Alias for compatibility with existing code
pub fn resample_audio(input: &[f32], from_sample_rate: u32, to_sample_rate: u32) -> Vec<f32> {
    match resample(input, from_sample_rate, to_sample_rate) {
        Ok(result) => result,
        Err(e) => {
            debug!("Resampling failed: {}, returning original audio", e);
            input.to_vec()
        }
    }
}
