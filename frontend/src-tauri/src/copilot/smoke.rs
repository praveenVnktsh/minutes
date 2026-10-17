//! Real-model measurements for Copilot. Ignored by default; run with
//!
//! ```text
//! LLAMA_HELPER_BIN=target/release/llama-helper \
//! LLAMA_MODEL_PATH=".../models/summary/Qwen3.5-4B-Q4_K_M.gguf" \
//! NEMOTRON_STREAMING_MODEL_DIR=".../nemotron560"   # optional: live RTF
//! cargo test -p meetily --lib copilot::smoke -- --ignored --nocapture
//! ```
//!
//! It builds Copilot's real prompts (context builder + Qwen template + stable
//! prefix) for a synthetic 40-minute meeting, drives llama-helper the way the
//! app does (streamed, prefix-cached, two threads while "recording"), and
//! prints first-word latency cold vs warm, tokens/s and reuse. With the
//! Nemotron folder it also measures live transcription's real-time factor
//! alone and with Copilot running.

use std::io::{BufRead, BufReader, Write};
use std::time::Instant;

use super::context::{build, push_line, Line, Speaker, TimelineItem};
use super::prompt::{QuickAction, Task};
use crate::summary::summary_engine::client::{
    format_builtin_prompt_with_prefixes, RECORDING_LLM_THREADS,
};

const MODEL: &str = "qwen3.5:4b";

fn meeting(minutes: i64) -> (Vec<Line>, Vec<TimelineItem>) {
    let topics = [
        "the Q3 launch",
        "pricing for the enterprise tier",
        "the onboarding funnel",
        "hiring a second designer",
        "the data pipeline migration",
        "the churn report",
        "the SOC 2 audit",
        "the partner integration with Acme",
    ];
    let sentences = [
        "I think we should look at {} again before Friday.",
        "We agreed that {} moves to next sprint.",
        "The main risk with {} is that nobody owns it yet.",
        "Let's make sure legal has signed off on {}.",
        "Honestly {} is going better than I expected.",
    ];
    let mut lines = Vec::new();
    let mut t = 0;
    let mut i = 0usize;
    while t < minutes * 60 {
        let speaker = if i % 3 == 0 {
            Speaker::You
        } else {
            Speaker::Others
        };
        let text = sentences[i % sentences.len()].replace("{}", topics[(i / 2) % topics.len()]);
        push_line(
            &mut lines,
            Line {
                start_ms: t * 1000,
                end_ms: t * 1000 + 5000,
                speaker,
                text,
            },
        );
        t += 9;
        i += 1;
    }
    let timeline = (0..minutes)
        .map(|m| TimelineItem {
            start_ms: m * 60_000,
            kind: if m % 4 == 0 {
                "decision".into()
            } else {
                "point".into()
            },
            text: format!("Discussed {}", topics[m as usize % topics.len()]),
        })
        .collect();
    (lines, timeline)
}

struct Helper {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    model: String,
    next_id: u64,
    /// Generation threads to ask for; `None` is the helper's default.
    threads: Option<i32>,
}

#[derive(Debug, Default)]
struct Measured {
    first_word_s: f64,
    total_s: f64,
    prompt_tokens: u64,
    reused_tokens: u64,
    tokens_per_second: f64,
    text: String,
}

impl Helper {
    fn start(binary: &str, model: &str) -> Self {
        let mut child = std::process::Command::new(binary)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("llama-helper starts");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin,
            stdout,
            model: model.to_string(),
            next_id: 1,
            threads: Some(RECORDING_LLM_THREADS),
        }
    }

    fn generate(
        &mut self,
        prompt: &str,
        prefix: Option<(usize, usize)>,
        max_tokens: u32,
    ) -> Measured {
        let id = self.next_id;
        self.next_id += 1;
        let mut request = serde_json::json!({
            "type": "generate",
            "prompt": prompt,
            "max_tokens": max_tokens,
            "context_size": 32768,
            "model_path": self.model,
            "temperature": 0.3,
            "top_k": 20,
            "top_p": 0.8,
            "repeat_penalty": 1.05,
            "penalty_last_n": 256,
            "stop_tokens": ["<|im_end|>"],
            "seed": 42,
            "id": id,
            "stream": true,
        });
        if let Some(threads) = self.threads {
            request["n_threads"] = threads.into();
        }
        if let Some((prefix, anchor)) = prefix {
            request["cache_prompt"] = true.into();
            request["cache_prefix_chars"] = prefix.into();
            request["cache_anchor_chars"] = anchor.into();
            request["cache_context_size"] = 8192.into();
        }
        let started = Instant::now();
        writeln!(self.stdin, "{request}").unwrap();
        self.stdin.flush().unwrap();
        let mut measured = Measured::default();
        let mut first = None;
        loop {
            let mut line = String::new();
            self.stdout.read_line(&mut line).unwrap();
            let value: serde_json::Value = serde_json::from_str(&line).unwrap();
            if value["type"] == "token" {
                first.get_or_insert_with(|| started.elapsed().as_secs_f64());
                continue;
            }
            assert_eq!(value["type"], "response", "{line}");
            assert!(value["error"].is_null(), "{line}");
            measured.total_s = started.elapsed().as_secs_f64();
            measured.first_word_s = first.unwrap_or(measured.total_s);
            measured.prompt_tokens = value["prompt_tokens"].as_u64().unwrap_or(0);
            measured.reused_tokens = value["reused_tokens"].as_u64().unwrap_or(0);
            measured.tokens_per_second = value["tokens_per_second"].as_f64().unwrap_or(0.0);
            measured.text = value["text"].as_str().unwrap_or_default().to_string();
            return measured;
        }
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        let _ = writeln!(self.stdin, "{}", serde_json::json!({"type": "shutdown"}));
        let _ = self.child.wait();
    }
}

fn copilot_prompt(
    lines: &[Line],
    timeline: &[TimelineItem],
    task: &Task,
) -> (String, (usize, usize)) {
    let parts = build(lines, timeline, "Ask about the Acme contract", task);
    let (prompt, prefix, anchor) = format_builtin_prompt_with_prefixes(
        MODEL,
        &parts.system,
        &parts.stable,
        &parts.rest,
        parts.anchor,
    )
    .unwrap();
    (prompt, (prefix, anchor))
}

fn report(label: &str, m: &Measured) {
    eprintln!(
        "{label:<44} first word {:>5.2}s  total {:>5.2}s  prompt {:>5}  reused {:>5}  {:>5.1} tok/s",
        m.first_word_s, m.total_s, m.prompt_tokens, m.reused_tokens, m.tokens_per_second
    );
}

#[test]
#[ignore]
fn copilot_latency_and_prefix_reuse_on_the_builtin_model() {
    let (Ok(binary), Ok(model)) = (
        std::env::var("LLAMA_HELPER_BIN"),
        std::env::var("LLAMA_MODEL_PATH"),
    ) else {
        eprintln!("set LLAMA_HELPER_BIN and LLAMA_MODEL_PATH");
        return;
    };
    let mut helper = Helper::start(&binary, &model);
    let load = helper.generate(
        "<|im_start|>user\nhi<|im_end|>\n<|im_start|>assistant\n",
        None,
        2,
    );
    eprintln!("model load + tiny generation: {:.2}s", load.total_s);

    for minutes in [5, 40] {
        let (mut lines, timeline) = meeting(minutes);
        let (cold_prompt, cold_prefix) =
            copilot_prompt(&lines, &timeline, &Task::Action(QuickAction::WhatToSay));
        let cold = helper.generate(&cold_prompt, Some(cold_prefix), 300);
        report(
            &format!("minute {minutes}: What should I say? (cold)"),
            &cold,
        );
        let (prompt, prefix) =
            copilot_prompt(&lines, &timeline, &Task::Action(QuickAction::Explain));
        let warm = helper.generate(&prompt, Some(prefix), 300);
        report(&format!("minute {minutes}: Explain that (warm)"), &warm);
        // A new line from Others arrives: confirm, then draft.
        let last = lines.last().unwrap().end_ms;
        let question = Line {
            start_ms: last + 1000,
            end_ms: last + 4000,
            speaker: Speaker::Others,
            text: "Can you send me the Acme numbers after this?".into(),
        };
        push_line(&mut lines, question.clone());
        let (prompt, prefix) = copilot_prompt(
            &lines,
            &timeline,
            &Task::Confirm {
                at_ms: question.start_ms,
                line: question.text.clone(),
            },
        );
        let confirm = helper.generate(&prompt, Some(prefix), 5);
        report(&format!("minute {minutes}: confirm (5 tokens)"), &confirm);
        eprintln!("    confirm said: {:?}", confirm.text.trim());
        let (prompt, prefix) = copilot_prompt(
            &lines,
            &timeline,
            &Task::Suggest {
                at_ms: question.start_ms,
                line: question.text.clone(),
            },
        );
        let draft = helper.generate(&prompt, Some(prefix), 300);
        report(&format!("minute {minutes}: suggestion draft"), &draft);
        let (prompt, prefix) = copilot_prompt(
            &lines,
            &timeline,
            &Task::Question("What did we decide about the mobile app budget?".into()),
        );
        let honest = helper.generate(&prompt, Some(prefix), 300);
        report(
            &format!("minute {minutes}: never-discussed question"),
            &honest,
        );
        eprintln!("    answer: {:?}", honest.text.trim());
    }
}

#[test]
#[ignore]
fn live_transcription_rtf_with_copilot_running() {
    use crate::audio::live_streaming::transcriber::{LiveEvent, Source, StreamingTranscriber};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    let (Ok(binary), Ok(model), Ok(dir)) = (
        std::env::var("LLAMA_HELPER_BIN"),
        std::env::var("LLAMA_MODEL_PATH"),
        std::env::var("NEMOTRON_STREAMING_MODEL_DIR"),
    ) else {
        eprintln!("set LLAMA_HELPER_BIN, LLAMA_MODEL_PATH and NEMOTRON_STREAMING_MODEL_DIR");
        return;
    };
    let seconds: usize = std::env::var("COPILOT_RTF_SECONDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(90);
    let dir = std::path::PathBuf::from(dir);
    let engine = Arc::new(crate::audio::live_streaming::engine::create(&dir).expect("model loads"));
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

    // Feed in real time-sized windows but as fast as decoding allows, like
    // the transcriber's own contention test, and report compute / audio time.
    let run = |label: &str| -> f64 {
        let mut transcriber = StreamingTranscriber::new(engine.clone(), sr as u32);
        let mut events: Vec<LiveEvent> = Vec::new();
        let window = sr / 20;
        let started = Instant::now();
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
        eprintln!("{label}: RTF {rtf:.3}");
        rtf
    };
    let alone = run("live alone");

    let (lines, timeline) = meeting(40);
    let tasks = [
        Task::Action(QuickAction::WhatToSay),
        Task::Confirm {
            at_ms: lines.last().unwrap().start_ms,
            line: "Can you send me the numbers?".into(),
        },
        Task::Action(QuickAction::CatchUp),
        Task::Question("Who owns the Acme integration?".into()),
    ];
    let prompts: Vec<(String, (usize, usize), u32)> = tasks
        .iter()
        .map(|task| {
            let (prompt, prefix) = copilot_prompt(&lines, &timeline, task);
            (prompt, prefix, task.max_tokens())
        })
        .collect();

    for (label, pause_ms, threads) in [
        ("live + Copilot back to back, default threads", 0u64, None),
        (
            "live + Copilot back to back, 2 generation threads",
            0u64,
            Some(RECORDING_LLM_THREADS),
        ),
        (
            "live + Copilot, a request every 8 s, 2 threads",
            8_000u64,
            Some(RECORDING_LLM_THREADS),
        ),
        (
            "live + proactive suggestions at their cap (a request every 20 s)",
            20_000u64,
            Some(RECORDING_LLM_THREADS),
        ),
    ] {
        let mut helper = Helper::start(&binary, &model);
        helper.threads = threads;
        helper.generate(&prompts[0].0, Some(prompts[0].1), 2); // load + warm the prefix
        let stop = Arc::new(AtomicBool::new(false));
        let count = Arc::new(AtomicUsize::new(0));
        let worker = {
            let stop = stop.clone();
            let count = count.clone();
            let prompts = prompts.clone();
            std::thread::spawn(move || {
                let mut i = 0;
                while !stop.load(Ordering::SeqCst) {
                    let (prompt, prefix, max) = &prompts[i % prompts.len()];
                    helper.generate(prompt, Some(*prefix), *max);
                    count.fetch_add(1, Ordering::SeqCst);
                    i += 1;
                    let until = Instant::now() + std::time::Duration::from_millis(pause_ms);
                    while Instant::now() < until && !stop.load(Ordering::SeqCst) {
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                }
                helper
            })
        };
        let contended = run(label);
        stop.store(true, Ordering::SeqCst);
        drop(worker.join().unwrap());
        eprintln!(
            "    {} Copilot requests during the run; slowdown {:.2}x vs alone",
            count.load(Ordering::SeqCst),
            contended / alone
        );
    }
}

/// 16 kHz mono PCM16 WAV samples.
fn read_wav_16k(path: &std::path::Path) -> Vec<f32> {
    let bytes = std::fs::read(path).unwrap();
    let mut offset = 12;
    loop {
        let id = &bytes[offset..offset + 4];
        let size = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
        if id == b"data" {
            return bytes[offset + 8..offset + 8 + size]
                .chunks_exact(2)
                .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
                .collect();
        }
        offset += 8 + size;
    }
}
