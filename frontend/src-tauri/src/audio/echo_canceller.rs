// audio/echo_canceller.rs
//
// Acoustic echo cancellation for the live microphone transcription path.
//
// The live pipeline transcribes the microphone and system audio separately. When
// the remote party plays through laptop speakers, the microphone hears them too,
// so the same speech is transcribed once from "system" and again from "mic".
// The two copies rarely transcribe to identical text (the mic copy has been
// through the speaker and the room), so the exact-text rule in
// `transcription/dedup.rs` cannot catch them.
//
// `MicEchoCanceller` removes that echo from the mic before its VAD sees it,
// using the system audio as the far-end reference. It runs WebRTC's AEC3 via
// the pure-Rust `sonora` port, echo canceller only: RNNoise and EBU R128
// already run at capture, and a second gain stage would fight the normaliser.
//
// # Why the mic is held back
//
// AEC3 can only cancel echo that shows up in the capture stream *after* its
// reference arrived. On the pipeline's timeline the opposite happens: system
// audio reaches the pipeline later than its echo reaches the mic. Measured on a
// real meeting (MacBook Pro speakers + mic), the mic copy of an echoed segment
// started 30-90ms before the system copy. Fed as-is, AEC3 would be asked to
// cancel echo it has not seen the reference for yet, and would remove nothing.
//
// So the canceller pairs each system frame with the mic frame from
// `REFERENCE_LEAD_MS` earlier, which puts the reference comfortably ahead. The
// lead is created with priming frames of silence that are processed but never
// returned, so every cleaned sample comes back at its original position: the
// output stream is the input stream, delayed in wall-clock time only. Callers
// keep their sample-count timeline unchanged, and `flush` returns the held-back
// tail so nothing is lost at a pause or stop.

use log::{info, warn};
use sonora::config::EchoCanceller;
use sonora::{AudioProcessing, Config, StreamConfig};
use std::collections::VecDeque;

/// How far ahead of the mic the system reference is fed, in milliseconds.
///
/// It has to cover the pipeline's worst mic-before-system skew (up to ~90ms
/// measured) with room to spare, while leaving the real acoustic delay inside
/// the range AEC3's delay estimator searches. 200ms does both. It is also the
/// extra wall-clock latency added to live mic transcripts, which is negligible
/// next to the seconds a VAD segment already takes to close.
pub const REFERENCE_LEAD_MS: usize = 200;

/// AEC3's own processing delay: a capture sample comes out this much later than
/// it went in. It is AEC3's block framing (144 samples at its 16kHz band rate)
/// plus a couple of samples of band-split filtering, and does not depend on the
/// audio. Measured at 48kHz as 430-434 samples; 9ms is 432. The timeline test
/// below fails if a `sonora` upgrade changes it.
const PROCESSING_LATENCY_MS: usize = 9;

/// How often AEC statistics are logged, in processed frames (10ms each).
const STATS_LOG_INTERVAL_FRAMES: u64 = 6_000;

/// Removes speaker echo from the microphone, using system audio as the reference.
///
/// Feed it the aligned mic/system windows the pipeline already produces. The
/// output is the cleaned mic at the same sample positions, but the last
/// [`REFERENCE_LEAD_MS`] (plus AEC3's own ~9ms) of mic is held back until the
/// next call or [`flush`]. Every mic sample fed in comes out exactly once.
///
/// If the processor ever reports an error it logs once and falls back to
/// passing the mic through unchanged, so transcription never stops because of
/// echo cancellation.
///
/// [`flush`]: MicEchoCanceller::flush
pub struct MicEchoCanceller {
    /// `None` once the processor has failed: the mic then passes through.
    processor: Option<AudioProcessing>,
    /// Stands in for AEC3's processing delay after a fallback to passthrough,
    /// so the output keeps its position on the timeline.
    passthrough_delay: VecDeque<f32>,
    /// Samples per 10ms frame, the only frame size AEC3 accepts.
    frame_len: usize,
    /// Mic samples the reference is fed ahead by; a whole number of frames.
    lead_samples: usize,
    /// [`PROCESSING_LATENCY_MS`] in samples.
    latency_samples: usize,
    /// Mic samples waiting to be cleaned, starting with the priming silence.
    mic: VecDeque<f32>,
    /// System samples waiting to be fed as the reference.
    system: VecDeque<f32>,
    /// Output samples still to drop: the priming silence and AEC3's latency.
    skip_left: usize,
    /// Real mic samples fed in, and returned, since the last flush.
    fed: usize,
    returned: usize,
    render_frame: Vec<f32>,
    /// Render output AEC3 insists on writing; never read.
    render_scratch: Vec<f32>,
    capture_frame: Vec<f32>,
    cleaned_frame: Vec<f32>,
    frames_processed: u64,
}

impl MicEchoCanceller {
    /// Create a canceller for mono audio at `sample_rate`.
    pub fn new(sample_rate: u32) -> Self {
        let frame_len = (sample_rate / 100) as usize;
        let lead_frames = REFERENCE_LEAD_MS / 10;
        let stream = StreamConfig::new(sample_rate, 1);
        let processor = AudioProcessing::builder()
            .config(Config {
                echo_canceller: Some(EchoCanceller::default()),
                ..Default::default()
            })
            .capture_config(stream)
            .render_config(stream)
            .build();

        info!(
            "Mic echo canceller ready: AEC3 at {} Hz, reference leads mic by {}ms",
            sample_rate, REFERENCE_LEAD_MS
        );

        let mut canceller = Self {
            processor: Some(processor),
            passthrough_delay: VecDeque::new(),
            frame_len,
            lead_samples: lead_frames * frame_len,
            latency_samples: sample_rate as usize * PROCESSING_LATENCY_MS / 1000,
            mic: VecDeque::new(),
            system: VecDeque::new(),
            skip_left: 0,
            fed: 0,
            returned: 0,
            render_frame: vec![0.0; frame_len],
            render_scratch: vec![0.0; frame_len],
            capture_frame: vec![0.0; frame_len],
            cleaned_frame: vec![0.0; frame_len],
            frames_processed: 0,
        };
        canceller.prime();
        canceller
    }

    /// Clean one aligned window. Returns the cleaned mic that is ready, which is
    /// shorter than `mic` on the first call after creation or a flush.
    pub fn process(&mut self, mic: &[f32], system: &[f32]) -> Vec<f32> {
        self.mic.extend(mic.iter().copied());
        self.system.extend(system.iter().copied());
        self.fed += mic.len();

        let mut out = Vec::with_capacity(mic.len());
        while self.system.len() >= self.frame_len
            && self.mic.len() >= self.lead_samples + self.frame_len
        {
            self.process_frame(&mut out);
        }
        out
    }

    /// Return every held-back mic sample, cleaned, and start over on a fresh
    /// lead. Called when the pipeline flushes (pause or stop), so the end of an
    /// utterance reaches the VAD before it is flushed.
    ///
    /// The adaptive filter is kept: the echo path is the same room after a pause.
    pub fn flush(&mut self) -> Vec<f32> {
        let mut out = Vec::new();

        // Keep feeding (silence once the queues run dry) until AEC3's latency
        // has pushed the last real mic sample out.
        while self.returned < self.fed {
            self.process_frame(&mut out);
        }
        // The last frame can run past the end of the real audio into silence.
        out.truncate(out.len() - (self.returned - self.fed));

        self.mic.clear();
        self.system.clear();
        self.prime();
        out
    }

    /// Queue `lead_samples` of silence ahead of the mic. It is processed so the
    /// reference gets its lead, and its output is dropped along with AEC3's
    /// latency.
    fn prime(&mut self) {
        self.mic
            .extend(std::iter::repeat(0.0).take(self.lead_samples));
        self.skip_left = self.lead_samples + self.latency_samples;
        self.fed = 0;
        self.returned = 0;
    }

    /// Run the next render and capture frames through AEC3, zero-padding either
    /// stream when it runs short, and append the cleaned samples to `out`.
    fn process_frame(&mut self, out: &mut Vec<f32>) {
        for sample in self.render_frame.iter_mut() {
            *sample = self.system.pop_front().unwrap_or(0.0);
        }
        for sample in self.capture_frame.iter_mut() {
            *sample = self.mic.pop_front().unwrap_or(0.0);
        }

        if let Some(processor) = self.processor.as_mut() {
            let result = processor
                .process_render_f32(&[&self.render_frame], &mut [&mut self.render_scratch])
                .and_then(|_| {
                    processor
                        .process_capture_f32(&[&self.capture_frame], &mut [&mut self.cleaned_frame])
                });
            if let Err(e) = result {
                warn!(
                    "Mic echo canceller failed ({:?}); passing the mic through uncancelled from now on",
                    e
                );
                self.processor = None;
                self.passthrough_delay =
                    std::iter::repeat(0.0).take(self.latency_samples).collect();
            }
        }
        if self.processor.is_none() {
            self.passthrough_delay
                .extend(self.capture_frame.iter().copied());
            for sample in self.cleaned_frame.iter_mut() {
                *sample = self.passthrough_delay.pop_front().unwrap_or(0.0);
            }
        }

        self.frames_processed += 1;
        if self.frames_processed % STATS_LOG_INTERVAL_FRAMES == 0 {
            self.log_stats();
        }

        let skip = self.skip_left.min(self.frame_len);
        self.skip_left -= skip;
        let before = out.len();
        out.extend_from_slice(&self.cleaned_frame[skip..]);
        self.returned += out.len() - before;
    }

    fn log_stats(&self) {
        if let Some(processor) = self.processor.as_ref() {
            let stats = processor.statistics();
            info!(
                "Mic echo canceller: ERLE={:?} dB, ERL={:?} dB, estimated delay={:?}ms",
                stats.echo_return_loss_enhancement, stats.echo_return_loss, stats.delay_ms
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;
    const WINDOW: usize = 28_800; // the pipeline's 600ms mixing window

    /// Deterministic speech-like signal: noise shaped into syllable-length
    /// bursts, so it has the on/off structure and broad spectrum of speech.
    fn speech_like(seconds: f32, seed: u32) -> Vec<f32> {
        let len = (seconds * RATE as f32) as usize;
        let mut state = seed.wrapping_mul(2_654_435_761).max(1);
        let mut lowpass = 0.0f32;
        (0..len)
            .map(|i| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                let noise = (state as f32 / u32::MAX as f32) * 2.0 - 1.0;
                lowpass = 0.6 * lowpass + 0.4 * noise;
                let t = i as f32 / RATE as f32;
                let syllable = (t * 4.0 * std::f32::consts::PI).sin().max(0.0);
                let phrase = if (t * 0.7).fract() < 0.8 { 1.0 } else { 0.0 };
                0.3 * lowpass * syllable * phrase
            })
            .collect()
    }

    /// What the mic hears of `system` through speakers and a room: attenuated,
    /// smeared by a couple of reflections, and shifted by `shift` samples
    /// (positive: the mic copy appears later on the pipeline timeline).
    fn room_echo(system: &[f32], shift: isize) -> Vec<f32> {
        let taps = [(0usize, 0.35f32), (180, 0.12), (610, 0.05)];
        (0..system.len())
            .map(|i| {
                taps.iter()
                    .map(|&(delay, gain)| {
                        let j = i as isize - shift - delay as isize;
                        if j >= 0 && (j as usize) < system.len() {
                            gain * system[j as usize]
                        } else {
                            0.0
                        }
                    })
                    .sum()
            })
            .collect()
    }

    fn run(canceller: &mut MicEchoCanceller, mic: &[f32], system: &[f32]) -> Vec<f32> {
        let mut out = Vec::with_capacity(mic.len());
        for (m, s) in mic.chunks(WINDOW).zip(system.chunks(WINDOW)) {
            out.extend(canceller.process(m, s));
        }
        out.extend(canceller.flush());
        out
    }

    fn energy(samples: &[f32]) -> f64 {
        samples
            .iter()
            .map(|&x| (x as f64) * (x as f64))
            .sum::<f64>()
            + 1e-12
    }

    fn reduction_db(before: &[f32], after: &[f32]) -> f64 {
        10.0 * (energy(before) / energy(after)).log10()
    }

    /// Echo suppression over the last `tail_s` seconds, once AEC3 has converged.
    fn converged_suppression_db(shift: isize) -> f64 {
        let system = speech_like(16.0, 7);
        let mic = room_echo(&system, shift);
        let mut canceller = MicEchoCanceller::new(RATE);
        let cleaned = run(&mut canceller, &mic, &system);
        assert_eq!(cleaned.len(), mic.len());
        let tail = 6 * RATE as usize;
        reduction_db(&mic[mic.len() - tail..], &cleaned[cleaned.len() - tail..])
    }

    #[test]
    fn suppresses_echo_that_reaches_the_pipeline_before_its_reference() {
        // The measured case: the mic copy leads the system copy by ~60ms.
        let db = converged_suppression_db(-(RATE as isize) * 60 / 1000);
        assert!(db >= 15.0, "only {:.1} dB of echo removed", db);
    }

    #[test]
    fn suppresses_echo_that_trails_its_reference() {
        // The physically ordinary case: speakers plus capture latency, ~40ms.
        let db = converged_suppression_db(RATE as isize * 40 / 1000);
        assert!(db >= 15.0, "only {:.1} dB of echo removed", db);
    }

    #[test]
    fn near_end_speech_passes_when_nothing_is_playing() {
        let mic = speech_like(6.0, 11);
        let system = vec![0.0; mic.len()];
        let mut canceller = MicEchoCanceller::new(RATE);
        let cleaned = run(&mut canceller, &mic, &system);
        assert_eq!(cleaned.len(), mic.len());
        let db = reduction_db(&mic, &cleaned);
        assert!(db.abs() < 3.0, "near-end speech changed by {:.1} dB", db);
    }

    #[test]
    fn near_end_speech_survives_after_the_filter_has_converged() {
        // Converge on 10s of echo, then the local speaker talks while the remote
        // side is silent. A converged canceller must not eat the local voice.
        let far = speech_like(10.0, 7);
        let near = speech_like(4.0, 23);
        let mut system = far.clone();
        system.extend(vec![0.0; near.len()]);
        let mut mic = room_echo(&far, 0);
        mic.extend(near.iter().copied());

        let mut canceller = MicEchoCanceller::new(RATE);
        let cleaned = run(&mut canceller, &mic, &system);
        let start = far.len();
        let db = reduction_db(&near, &cleaned[start..start + near.len()]);
        assert!(db < 3.0, "local speech lost {:.1} dB after convergence", db);
    }

    #[test]
    fn cleaned_audio_keeps_its_position_on_the_timeline() {
        // A single loud burst in otherwise silent mic audio must come back at
        // the same sample offset, despite the held-back lead.
        let mut mic = vec![0.0f32; 3 * RATE as usize];
        let burst_at = 100_003;
        for (k, sample) in speech_like(0.2, 5).into_iter().enumerate() {
            mic[burst_at + k] = sample * 3.0;
        }
        let system = vec![0.0; mic.len()];
        let mut canceller = MicEchoCanceller::new(RATE);
        let cleaned = run(&mut canceller, &mic, &system);
        assert_eq!(cleaned.len(), mic.len());

        // Find the lag that best lines the output up with the input. Amplitude
        // thresholds are no good here: AEC3's high-pass filter softens the onset.
        let burst = &mic[burst_at..burst_at + 9_600];
        let correlation = |lag: isize| -> f64 {
            burst
                .iter()
                .enumerate()
                .map(|(k, &x)| x as f64 * cleaned[(burst_at as isize + lag) as usize + k] as f64)
                .sum()
        };
        let shift = (-2_400..=2_400isize)
            .max_by(|&a, &b| correlation(a).total_cmp(&correlation(b)))
            .unwrap();
        // A couple of samples of filter phase is all that may remain.
        assert!(shift.abs() <= 4, "burst moved by {} samples", shift);
    }

    #[test]
    fn holds_back_the_lead_and_returns_it_on_flush() {
        let mut canceller = MicEchoCanceller::new(RATE);
        let held = RATE as usize * (REFERENCE_LEAD_MS + PROCESSING_LATENCY_MS) / 1000;
        let window = vec![0.01f32; WINDOW];

        assert_eq!(canceller.process(&window, &window).len(), WINDOW - held);
        assert_eq!(canceller.process(&window, &window).len(), WINDOW);
        assert_eq!(canceller.flush().len(), held);
    }

    #[test]
    fn sample_counts_balance_across_a_pause() {
        // Pause and resume flush mid-recording; a ragged tail window arrives on
        // stop. Every sample in must come out exactly once.
        let mut canceller = MicEchoCanceller::new(RATE);
        let mut fed = 0;
        let mut returned = 0;
        for len in [WINDOW, WINDOW, 1_234] {
            let w = vec![0.02f32; len];
            fed += len;
            returned += canceller.process(&w, &w).len();
        }
        returned += canceller.flush().len();
        assert_eq!(returned, fed);

        for len in [WINDOW, 777] {
            let w = vec![0.02f32; len];
            fed += len;
            returned += canceller.process(&w, &w).len();
        }
        returned += canceller.flush().len();
        assert_eq!(returned, fed);
    }

    #[test]
    fn flush_with_no_audio_returns_nothing() {
        let mut canceller = MicEchoCanceller::new(RATE);
        assert!(canceller.flush().is_empty());
        assert!(canceller.flush().is_empty());
    }
}
