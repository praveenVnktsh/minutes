//! Two-source streaming transcription on top of a [`Recognizer`] (sherpa on
//! the CPU, or Core ML in the helper), free of Tauri so it can be exercised
//! end to end in tests.
//!
//! Each source (microphone, system) has its own recognizer stream and silence
//! gate. Audio is fed as it arrives; `decode` runs every ready stream as one
//! batch and reports partial text and finished utterances as [`LiveEvent`]s.

use std::sync::Arc;

use super::engine::StreamingEngine;
use super::recognizer::StreamId;
use super::gate::{join_tokens, next_commit, segment_bounds, GateAction, SilenceGate};
use crate::audio::RecordingDeviceType;

/// Zeros appended before `input_finished`, so the model's last 560 ms chunk
/// (plus its look-ahead) is complete and the final words are decoded.
const TAIL_PADDING_SECONDS: f64 = 0.8;
/// A fully committed hypothesis longer than this is reset during silence, so
/// `get_result` stays cheap over a long meeting.
const RESET_AFTER_TOKENS: usize = 1500;
/// Silence after the last token before such a reset; nothing is in flight.
const RESET_AFTER_SILENCE_SECONDS: f64 = 1.5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Microphone,
    System,
}

impl Source {
    pub fn from_device(device: &RecordingDeviceType) -> Self {
        match device {
            RecordingDeviceType::Microphone => Self::Microphone,
            RecordingDeviceType::System => Self::System,
        }
    }

    /// Label used on transcript events and saved segments, matching the
    /// chunked worker.
    pub fn label(self) -> &'static str {
        match self {
            Self::Microphone => "mic",
            Self::System => "system",
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Microphone => 0,
            Self::System => 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum LiveEvent {
    /// The utterance in progress so far. Empty text clears it.
    Partial {
        source: Source,
        text: String,
        start: f64,
    },
    /// A finished utterance on the recording timeline (seconds).
    Final {
        source: Source,
        text: String,
        start: f64,
        end: f64,
    },
}

struct SourceState {
    source: Source,
    gate: SilenceGate,
    stream: Option<StreamId>,
    /// Recording time of the stream's zero.
    origin: f64,
    /// Samples fed to the current stream: its current time.
    fed: u64,
    /// Tokens of the current hypothesis already emitted as finished text.
    committed: usize,
    last_partial: String,
}

impl SourceState {
    fn new(source: Source, sample_rate: u32) -> Self {
        Self {
            source,
            gate: SilenceGate::new(sample_rate),
            stream: None,
            origin: 0.0,
            fed: 0,
            committed: 0,
            last_partial: String::new(),
        }
    }
}

pub struct StreamingTranscriber {
    engine: Arc<StreamingEngine>,
    sample_rate: u32,
    sources: [SourceState; 2],
    /// The engine's first failure (the helper exited or stopped answering).
    /// Nothing more is decoded once set; the worker decides what to do.
    failure: Option<String>,
}

impl StreamingTranscriber {
    pub fn new(engine: Arc<StreamingEngine>, sample_rate: u32) -> Self {
        Self {
            engine,
            sample_rate,
            sources: [
                SourceState::new(Source::Microphone, sample_rate),
                SourceState::new(Source::System, sample_rate),
            ],
            failure: None,
        }
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Recording time of the newest decoded token on `source`, for latency
    /// measurements.
    #[cfg(test)]
    pub fn last_token_time(&self, source: Source) -> Option<f64> {
        let state = &self.sources[source.index()];
        let result = self.engine.recognizer.result(state.stream?)?;
        result
            .times
            .last()
            .map(|t| state.origin + result.start + t)
    }

    /// Why the engine stopped working, if it did.
    pub fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }

    fn fail(&mut self, error: anyhow::Error) {
        if self.failure.is_none() {
            log::warn!("Live transcription engine failed: {error:#}");
            self.failure = Some(format!("{error:#}"));
        }
    }

    fn feed(&self, state: &mut SourceState, samples: &[f32]) {
        if let Some(id) = state.stream {
            self.engine
                .recognizer
                .accept(id, self.sample_rate, samples);
            state.fed += samples.len() as u64;
        }
    }

    /// Feed one window of a source that starts `start_seconds` into the
    /// recording. Call [`Self::decode`] afterwards (once per batch of windows
    /// is enough). A window that does not follow on from the previous one
    /// (dropped backlog, live text enabled mid-recording) closes the utterance
    /// in progress and restarts the source at the new position.
    pub fn push(
        &mut self,
        source: Source,
        window: &[f32],
        start_seconds: f64,
        events: &mut Vec<LiveEvent>,
    ) {
        let index = source.index();
        let mut state = std::mem::replace(
            &mut self.sources[index],
            SourceState::new(source, self.sample_rate),
        );
        let expected = (start_seconds.max(0.0) * self.sample_rate as f64).round() as u64;
        if state.gate.position().abs_diff(expected) > 1 {
            self.finish_stream(&mut state, events);
            state.gate.set_position(expected);
        }
        match state.gate.push(window) {
            GateAction::Skip => {}
            GateAction::Open {
                origin_seconds,
                preroll,
            } => {
                // A stream left over from a forced close is finished first.
                self.finish_stream(&mut state, events);
                state.stream = if self.failure.is_some() {
                    None
                } else {
                    match self.engine.recognizer.create_stream(self.sample_rate) {
                        Ok(id) => Some(id),
                        Err(error) => {
                            self.fail(error);
                            None
                        }
                    }
                };
                state.origin = origin_seconds;
                state.fed = 0;
                state.committed = 0;
                self.feed(&mut state, &preroll);
                self.feed(&mut state, window);
            }
            GateAction::Feed => self.feed(&mut state, window),
            GateAction::FeedThenClose => {
                self.feed(&mut state, window);
                self.finish_stream(&mut state, events);
            }
        }
        self.sources[index] = state;
    }

    /// Decode every ready stream, batched, and report what changed.
    pub fn decode(&mut self, events: &mut Vec<LiveEvent>) {
        let open: Vec<StreamId> = self
            .sources
            .iter()
            .filter_map(|state| state.stream)
            .collect();
        if open.is_empty() || self.failure.is_some() {
            return;
        }
        if let Err(error) = self.engine.recognizer.decode(&open) {
            self.fail(error);
            return;
        }

        for index in 0..self.sources.len() {
            let source = self.sources[index].source;
            let mut state = std::mem::replace(
                &mut self.sources[index],
                SourceState::new(source, self.sample_rate),
            );
            self.update(&mut state, false, events);
            self.sources[index] = state;
        }
    }

    /// Commit what the segmentation rules allow (everything when `finishing`),
    /// then report the rest as the partial line.
    fn update(&self, state: &mut SourceState, finishing: bool, events: &mut Vec<LiveEvent>) {
        let Some(stream) = state.stream else {
            return;
        };
        let Some(result) = self.engine.recognizer.result(stream) else {
            return;
        };
        let base = result.start;
        let tokens = result.tokens;
        let times: Vec<f64> = result.times.iter().map(|&t| base + t).collect();
        let len = tokens.len().min(times.len());
        // A hypothesis can only shrink after a reset, which zeroes `committed`.
        state.committed = state.committed.min(len);
        let now = state.fed as f64 / self.sample_rate as f64;

        let commit_to = if finishing {
            (state.committed < len).then_some(len)
        } else {
            next_commit(&tokens[..len], &times[..len], state.committed, now)
        };
        if let Some(end) = commit_to {
            let text = join_tokens(&tokens[state.committed..end]);
            if !text.is_empty() {
                let (start, stop) =
                    segment_bounds(state.origin, times[state.committed], times[end - 1]);
                events.push(LiveEvent::Final {
                    source: state.source,
                    text,
                    start,
                    end: stop.min(state.origin + now.max(0.0)).max(start + 0.1),
                });
            }
            state.committed = end;
        }

        let pending = join_tokens(&tokens[state.committed..len]);
        if pending != state.last_partial {
            let start = if state.committed < len {
                segment_bounds(state.origin, times[state.committed], times[len - 1]).0
            } else {
                state.origin + now
            };
            state.last_partial.clone_from(&pending);
            events.push(LiveEvent::Partial {
                source: state.source,
                text: pending,
                start,
            });
        }

        let quiet_for = len
            .checked_sub(1)
            .map(|last| now - times[last])
            .unwrap_or(now);
        if !finishing
            && state.committed == len
            && len > RESET_AFTER_TOKENS
            && quiet_for >= RESET_AFTER_SILENCE_SECONDS
        {
            if let Some(stream) = state.stream {
                self.engine.recognizer.reset(stream);
            }
            state.committed = 0;
        }
    }

    /// Flush a stream: pad, mark input finished, decode the rest, commit it,
    /// and drop the stream.
    fn finish_stream(&mut self, state: &mut SourceState, events: &mut Vec<LiveEvent>) {
        let Some(stream) = state.stream else {
            return;
        };
        if self.failure.is_none() {
            let padding = vec![0.0f32; (TAIL_PADDING_SECONDS * self.sample_rate as f64) as usize];
            let recognizer = &self.engine.recognizer;
            recognizer.accept(stream, self.sample_rate, &padding);
            recognizer.input_finished(stream);
            if let Err(error) = recognizer.decode(&[stream]) {
                self.fail(error);
            }
        }
        // `fed` excludes the padding, which is not part of the recording.
        self.update(state, true, events);
        self.engine.recognizer.close(stream);
        state.stream = None;
        state.committed = 0;
    }

    /// End the utterance in progress on one source (pause, stop or a dropped
    /// backlog). The next audio starts a fresh stream.
    pub fn flush_source(&mut self, source: Source, events: &mut Vec<LiveEvent>) {
        let index = source.index();
        let mut state = std::mem::replace(
            &mut self.sources[index],
            SourceState::new(source, self.sample_rate),
        );
        self.finish_stream(&mut state, events);
        state.gate.force_close();
        self.sources[index] = state;
    }

    pub fn finish(&mut self, events: &mut Vec<LiveEvent>) {
        self.flush_source(Source::Microphone, events);
        self.flush_source(Source::System, events);
    }

    /// Commit whatever each source has already decoded as finished text,
    /// without decoding anything more, and close its stream. For a stop that
    /// cancels a slow drain: the utterance in progress is on screen as a
    /// partial, and dropping it loses the meeting's last words whenever no
    /// final pass replaces the live transcript.
    pub fn commit_decoded(&mut self, events: &mut Vec<LiveEvent>) {
        for index in 0..self.sources.len() {
            let source = self.sources[index].source;
            let mut state = std::mem::replace(
                &mut self.sources[index],
                SourceState::new(source, self.sample_rate),
            );
            self.update(&mut state, true, events);
            if let Some(stream) = state.stream.take() {
                self.engine.recognizer.close(stream);
            }
            state.committed = 0;
            state.gate.force_close();
            self.sources[index] = state;
        }
    }
}

impl Drop for StreamingTranscriber {
    fn drop(&mut self) {
        for state in &mut self.sources {
            if let Some(stream) = state.stream.take() {
                self.engine.recognizer.close(stream);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Same setup as below: a cancelled stop commits the decoded part of the
    /// utterance in progress instead of dropping it.
    #[test]
    #[ignore]
    fn a_cancelled_stop_keeps_the_decoded_partial() {
        let Ok(dir) = std::env::var("NEMOTRON_STREAMING_MODEL_DIR") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let engine = Arc::new(super::super::engine::create(&dir).expect("model loads"));
        let audio = read_wav_16k(&dir.join("test_wavs").join("0.wav"));
        let sr = 16_000u32;
        let mut transcriber = StreamingTranscriber::new(engine, sr);
        let mut events = Vec::new();
        // The first 4 s of speech with no pause, so little or nothing is final.
        for (i, window) in audio[..(4 * sr as usize).min(audio.len())]
            .chunks(800)
            .enumerate()
        {
            let start = (i * 800) as f64 / sr as f64;
            transcriber.push(Source::System, window, start, &mut events);
            transcriber.decode(&mut events);
        }
        let pending = events.iter().rev().find_map(|event| match event {
            LiveEvent::Partial { text, .. } => Some(text.clone()),
            _ => None,
        });
        assert!(pending.is_some_and(|text| !text.is_empty()), "{events:?}");
        events.clear();
        transcriber.commit_decoded(&mut events);
        let finals: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                LiveEvent::Final { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect();
        eprintln!("{finals:?}");
        assert!(!finals.is_empty(), "{events:?}");
        assert!(finals.iter().all(|text| !text.trim().is_empty()));
    }

    /// Live transcription speed with and without the built-in LLM generating
    /// at the same time. Needs `NEMOTRON_STREAMING_MODEL_DIR` (with
    /// `test_wavs/0.wav`), `LLAMA_HELPER_BIN` and `LLAMA_MODEL_PATH`; set
    /// `LLAMA_CONTENTION_SECONDS` to change the audio length (default 120).
    /// Prints the real-time factor (compute time / audio time) of both runs.
    #[test]
    #[ignore]
    fn live_transcription_contention_with_the_builtin_llm() {
        use std::io::{BufRead, BufReader, Write};
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::time::Instant;

        let (Ok(dir), Ok(helper), Ok(model)) = (
            std::env::var("NEMOTRON_STREAMING_MODEL_DIR"),
            std::env::var("LLAMA_HELPER_BIN"),
            std::env::var("LLAMA_MODEL_PATH"),
        ) else {
            return;
        };
        let seconds: usize = std::env::var("LLAMA_CONTENTION_SECONDS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(120);
        let dir = std::path::PathBuf::from(dir);
        let engine = Arc::new(super::super::engine::create(&dir).expect("model loads"));
        // Two speakers' worth of 48 kHz audio: speech on both sources, offset
        // so they are not identical, like a call.
        let speech = read_wav_16k(&dir.join("test_wavs").join("0.wav"));
        let sr = 48_000usize;
        let mut system = Vec::with_capacity(seconds * sr);
        while system.len() < seconds * sr {
            system.extend(speech.iter().flat_map(|&s| [s, s, s]));
            system.extend(std::iter::repeat_n(0.0, sr));
        }
        system.truncate(seconds * sr);
        let mut mic = vec![0.0; 3 * sr];
        mic.extend_from_slice(&system);
        mic.truncate(seconds * sr);

        let run = |label: &str| -> f64 {
            let mut transcriber = StreamingTranscriber::new(engine.clone(), sr as u32);
            let mut events = Vec::new();
            let window = sr / 20; // the pipeline's 50 ms windows
            let started = Instant::now();
            // Decode once per ~560 ms of audio, as the worker batches.
            for (i, (sys, mic)) in system.chunks(window).zip(mic.chunks(window)).enumerate() {
                let start = (i * window) as f64 / sr as f64;
                transcriber.push(Source::System, sys, start, &mut events);
                transcriber.push(Source::Microphone, mic, start, &mut events);
                if i % 11 == 10 {
                    transcriber.decode(&mut events);
                }
            }
            transcriber.finish(&mut events);
            let rtf = started.elapsed().as_secs_f64() / seconds as f64;
            let finals = events
                .iter()
                .filter(|event| matches!(event, LiveEvent::Final { .. }))
                .count();
            eprintln!(
                "{label}: RTF {rtf:.3} over {seconds} s of two-source audio ({finals} finals)"
            );
            rtf
        };

        let alone = run("live alone");

        // A live-summary-shaped request: ~1,200 words of transcript, 150
        // output tokens, Qwen chat template with thinking off.
        let words =
            "we discussed the onboarding flow and the release date for the new version ".repeat(90);
        let prompt = format!(
            "<|im_start|>system\nSummarise the meeting window as JSON entries.<|im_end|>\n<|im_start|>user\n{words}<|im_end|>\n<|im_start|>assistant\n<think>\n\n</think>\n\n"
        );
        let request = serde_json::json!({
            "type": "generate",
            "prompt": prompt,
            "max_tokens": 150,
            "context_size": 8192,
            "model_path": model,
            "temperature": 0.2,
            "seed": 42,
        })
        .to_string();
        let mut child = std::process::Command::new(&helper)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("llama-helper starts");
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut generate = move |stdin: &mut std::process::ChildStdin| -> f64 {
            let started = Instant::now();
            writeln!(stdin, "{request}").unwrap();
            stdin.flush().unwrap();
            let mut line = String::new();
            stdout.read_line(&mut line).unwrap();
            assert!(line.contains("\"response\""), "{line}");
            started.elapsed().as_secs_f64()
        };
        let warm = generate(&mut stdin);
        eprintln!("first generation (model load + run): {warm:.2} s");
        let solo = generate(&mut stdin);
        eprintln!("one timeline generation alone: {solo:.2} s");

        let stop = Arc::new(AtomicBool::new(false));
        let count = Arc::new(AtomicUsize::new(0));
        let llm = {
            let stop = stop.clone();
            let count = count.clone();
            std::thread::spawn(move || {
                let mut total = 0.0;
                while !stop.load(Ordering::SeqCst) {
                    total += generate(&mut stdin);
                    count.fetch_add(1, Ordering::SeqCst);
                }
                (stdin, total)
            })
        };
        let contended = run("live with back-to-back LLM generations");
        stop.store(true, Ordering::SeqCst);
        let (mut stdin, total) = llm.join().unwrap();
        let generations = count.load(Ordering::SeqCst);
        eprintln!(
            "LLM: {generations} generations during the contended run, {:.2} s each on average",
            total / generations.max(1) as f64
        );
        let _ = writeln!(stdin, "{}", serde_json::json!({"type": "shutdown"}));
        let _ = child.wait();
        eprintln!("slowdown: {:.2}x", contended / alone);
        assert!(
            alone < 1.0,
            "live transcription alone is slower than real time"
        );
    }

    /// 16 kHz mono PCM16 WAV samples.
    fn read_wav_16k(path: &std::path::Path) -> Vec<f32> {
        let bytes = std::fs::read(path).unwrap();
        let mut offset = 12;
        loop {
            let id = &bytes[offset..offset + 4];
            let size =
                u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
            if id == b"data" {
                return bytes[offset + 8..offset + 8 + size]
                    .chunks_exact(2)
                    .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
                    .collect();
            }
            offset += 8 + size;
        }
    }

    /// End to end against the real model. Set `NEMOTRON_STREAMING_MODEL_DIR` to
    /// a folder holding the pinned files (and `test_wavs/` from the same repo)
    /// and run with `--ignored`.
    #[test]
    #[ignore]
    fn transcribes_two_utterances_on_the_recording_timeline() {
        let Ok(dir) = std::env::var("NEMOTRON_STREAMING_MODEL_DIR") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let engine = Arc::new(super::super::engine::create(&dir).expect("model loads"));
        // 16 kHz mono PCM16 WAV: find the `data` chunk and decode it.
        let read = |name: &str| -> Vec<f32> {
            let bytes = std::fs::read(dir.join("test_wavs").join(name)).unwrap();
            let mut offset = 12;
            loop {
                let id = &bytes[offset..offset + 4];
                let size =
                    u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
                if id == b"data" {
                    return bytes[offset + 8..offset + 8 + size]
                        .chunks_exact(2)
                        .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
                        .collect();
                }
                offset += 8 + size;
            }
        };
        // Upsample to the pipeline's 48 kHz to exercise sherpa's resampler.
        let up = |audio: Vec<f32>| -> Vec<f32> { audio.iter().flat_map(|&s| [s, s, s]).collect() };
        let sr = 48_000u32;
        let mut audio = vec![0.0; 2 * sr as usize]; // 2 s of silence first
        audio.extend(up(read("0.wav"))); // 2.0 .. 8.62 s
        audio.extend(vec![0.0; 3 * sr as usize]);
        audio.extend(up(read("1.wav"))); // 11.62 .. 28.34 s

        let mut transcriber = StreamingTranscriber::new(engine, sr);
        let mut events = Vec::new();
        let silence = vec![0.0; 2400];
        for (i, window) in audio.chunks(2400).enumerate() {
            let start = (i * 2400) as f64 / sr as f64;
            transcriber.push(Source::System, window, start, &mut events);
            transcriber.push(Source::Microphone, &silence, start, &mut events);
            transcriber.decode(&mut events);
        }
        transcriber.finish(&mut events);

        let finals: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                LiveEvent::Final {
                    source,
                    text,
                    start,
                    end,
                } => Some((*source, text.clone(), *start, *end)),
                _ => None,
            })
            .collect();
        for item in &finals {
            eprintln!("{item:?}");
        }
        assert!(finals.iter().all(|(source, ..)| *source == Source::System));
        let first = &finals[0];
        assert!(
            first.1.to_lowercase().contains("yellow lamps"),
            "{}",
            first.1
        );
        assert!(first.2 >= 1.9 && first.2 < 3.0, "first start {}", first.2);
        let all: String = finals
            .iter()
            .map(|f| f.1.to_lowercase())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(all.contains("soul in heaven"), "{all}");
        assert!(
            all.contains("the squalid quarter"),
            "no words lost at boundaries: {all}"
        );
        assert!(
            all.contains("whose place was"),
            "no words lost at boundaries: {all}"
        );
        let last = finals.last().unwrap();
        assert!(last.3 <= 28.6 && last.3 > 26.0, "last end {}", last.3);
        assert!(events
            .iter()
            .any(|event| matches!(event, LiveEvent::Partial { text, .. } if !text.is_empty())));
    }
}
